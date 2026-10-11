// SPDX-License-Identifier: GPL-3.0-or-later
//! One sealed hour, condensed: every reading's exact count / sum / min
//! / max / latest / total and its p95 sketch, the GPU programs, the
//! gaps between samples. Built once from the hour's records (when the
//! hour is sealed, and again when it is repacked into the archive) and
//! kept ALONGSIDE the raw records, never instead of them: it is derived
//! data, rebuilt from the raw lines whenever it is missing or stale.
//!
//! Its one job is speed. A 30-day summary at a 2 s cadence is 1.3
//! million records; merging 720 rollups gives the same min, avg, p95,
//! max and timeline in a fraction of the time (`summary.rs` explains
//! when an hour may be merged whole). ~12 KB of JSON an hour, ~3 KB
//! compressed: about 5% of the raw archive.
//!
//! A record's rate window depends on the record before it, which for
//! an hour's first record lies outside the hour. So the rollup leaves
//! the first record's share out of each `total` and keeps its value
//! (`first_value`) and interval: the reader adds `value × window` once
//! it knows the window.

use serde::{Deserialize, Serialize};

use super::HistoryRecord;
use super::summary::{GapHistogram, ProgramStats, ReadingSink, Sketch, Stats, Unit, sample_programs, visit};

/// One reading over one hour.
#[derive(Clone, Debug, PartialEq)]
pub struct RollupMetric {
    pub key: String,
    pub label: String,
    pub unit: Unit,
    /// `total` leaves out the first record's value × window.
    pub stats: Stats,
    /// This reading's value in the hour's first record, when it had it.
    pub first_value: Option<f64>,
    pub ceiling: Option<f64>,
}

/// One hour, condensed.
#[derive(Clone, Debug, PartialEq)]
pub struct HourRollup {
    /// The hour's start, unix seconds (UTC hour).
    pub start: f64,
    pub samples: u64,
    pub first_ts: f64,
    pub last_ts: f64,
    pub first_interval: f64,
    /// Seconds covered by every record but the first.
    pub covered_rest: f64,
    /// Gaps between the hour's records (not the gap before the first).
    pub gaps: GapHistogram,
    /// First-seen order, as a record-by-record read would list them.
    pub metrics: Vec<RollupMetric>,
    /// By name.
    pub programs: Vec<(String, ProgramStats)>,
    /// A hot hour's rollup: the size of the `.jsonl.gz` it was built
    /// from. A restart inside a sealed hour appends a gzip member, and
    /// a size that no longer matches means the rollup is stale (it is
    /// ignored and rebuilt). 0 inside the archive (immutable there).
    pub source_bytes: u64,
}

struct Builder {
    metrics: Vec<RollupMetric>,
    index: std::collections::HashMap<String, usize>,
    first: bool,
    window: f64,
}

impl ReadingSink for Builder {
    fn value(&mut self, key: &str, label: &dyn Fn() -> String, unit: Unit, value: f64) {
        let slot = match self.index.get(key) {
            Some(slot) => *slot,
            None => {
                self.metrics.push(RollupMetric {
                    key: key.to_string(),
                    label: label(),
                    unit,
                    stats: Stats::default(),
                    first_value: None,
                    ceiling: None,
                });
                self.index.insert(key.to_string(), self.metrics.len() - 1);
                self.metrics.len() - 1
            }
        };
        if !value.is_finite() {
            return;
        }
        let metric = &mut self.metrics[slot];
        metric.stats.add(value, self.window);
        if self.first {
            metric.first_value = Some(value);
        }
    }

    fn ceiling(&mut self, key: &str, ceiling: f64) {
        if let Some(slot) = self.index.get(key)
            && ceiling > 0.0
        {
            let metric = &mut self.metrics[*slot];
            metric.ceiling = Some(metric.ceiling.map_or(ceiling, |c: f64| c.max(ceiling)));
        }
    }
}

impl HourRollup {
    /// The rollup of one hour's records: sorted by time, one per
    /// timestamp (the reader's order). None for an empty hour.
    pub fn build(start: f64, records: &[HistoryRecord]) -> Option<HourRollup> {
        let first = records.first()?;
        let mut builder = Builder { metrics: Vec::new(), index: Default::default(), first: true, window: 0.0 };
        let mut programs: std::collections::BTreeMap<String, ProgramStats> = Default::default();
        let mut gaps = GapHistogram::default();
        let mut covered_rest = 0.0;
        let mut previous: Option<f64> = None;
        let mut scratch = String::new();
        for record in records {
            let window = match previous {
                // The first record's window is the reader's to decide.
                None => 0.0,
                Some(previous) => {
                    gaps.add(record.ts - previous);
                    record.interval_seconds.min(record.ts - previous).max(0.0)
                }
            };
            covered_rest += window;
            builder.first = previous.is_none();
            builder.window = window;
            visit(record, "all", &mut scratch, &mut builder);
            for (name, vram, busy, gtt, pids) in sample_programs(record) {
                programs.entry(name.to_string()).or_default().add_sample(vram, busy, gtt, &pids, window);
            }
            previous = Some(record.ts);
        }
        Some(HourRollup {
            start,
            samples: records.len() as u64,
            first_ts: first.ts,
            last_ts: records.last().map_or(first.ts, |r| r.ts),
            first_interval: first.interval_seconds,
            covered_rest,
            gaps,
            metrics: builder.metrics,
            programs: programs.into_iter().collect(),
            source_bytes: 0,
        })
    }

    /// One JSON line (no newline).
    pub fn to_line(&self) -> String {
        serde_json::to_string(&RollupWire::from(self)).unwrap_or_default()
    }

    pub fn from_line(line: &str) -> Option<HourRollup> {
        serde_json::from_str::<RollupWire>(line).ok().map(HourRollup::from)
    }
}

// ------------------------------------------------------------- the wire

/// Stats on disk: `[count, sum, min, max, latest, total, nonpositive]`
/// and the sketch as `[first index, count, Δindex, count, …]`.
#[derive(Serialize, Deserialize)]
struct StatsWire(u64, f64, f64, f64, f64, f64, u64, Vec<i64>);

impl From<&Stats> for StatsWire {
    fn from(stats: &Stats) -> Self {
        let mut sketch = Vec::new();
        let mut previous = 0i64;
        for (index, count) in stats.sketch.buckets() {
            sketch.push(i64::from(index) - previous);
            sketch.push(count as i64);
            previous = i64::from(index);
        }
        let (min, max) = if stats.count == 0 { (0.0, 0.0) } else { (stats.min, stats.max) };
        StatsWire(stats.count, stats.sum, min, max, stats.latest, stats.total, stats.nonpositive, sketch)
    }
}

impl From<StatsWire> for Stats {
    fn from(wire: StatsWire) -> Self {
        let StatsWire(count, sum, min, max, latest, total, nonpositive, pairs) = wire;
        let mut sketch = Sketch::default();
        let mut index = 0i64;
        for pair in pairs.chunks_exact(2) {
            index += pair[0];
            sketch.add(index as i32, pair[1].max(0) as u64);
        }
        if count == 0 {
            return Stats::default();
        }
        Stats { count, sum, min, max, latest, total, nonpositive, sketch }
    }
}

#[derive(Serialize, Deserialize)]
struct MetricWire {
    key: String,
    label: String,
    unit: Unit,
    stats: StatsWire,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    first: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    ceiling: Option<f64>,
}

#[derive(Serialize, Deserialize)]
struct ProgramWire {
    name: String,
    pids: Vec<i32>,
    vram: StatsWire,
    busy: StatsWire,
    gtt_max: f64,
}

#[derive(Serialize, Deserialize)]
struct RollupWire {
    v: u32,
    start: f64,
    samples: u64,
    first_ts: f64,
    last_ts: f64,
    first_interval: f64,
    covered_rest: f64,
    /// `[[gap in 10 ms steps, count], …]`
    gaps: Vec<(u32, u64)>,
    metrics: Vec<MetricWire>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    programs: Vec<ProgramWire>,
    #[serde(skip_serializing_if = "is_zero", default)]
    source_bytes: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl From<&HourRollup> for RollupWire {
    fn from(rollup: &HourRollup) -> Self {
        RollupWire {
            v: 1,
            start: rollup.start,
            samples: rollup.samples,
            first_ts: rollup.first_ts,
            last_ts: rollup.last_ts,
            first_interval: rollup.first_interval,
            covered_rest: rollup.covered_rest,
            gaps: rollup.gaps.counts.iter().map(|(g, c)| (*g, *c)).collect(),
            metrics: rollup
                .metrics
                .iter()
                .map(|m| MetricWire {
                    key: m.key.clone(),
                    label: m.label.clone(),
                    unit: m.unit,
                    stats: StatsWire::from(&m.stats),
                    first: m.first_value,
                    ceiling: m.ceiling,
                })
                .collect(),
            programs: rollup
                .programs
                .iter()
                .map(|(name, p)| ProgramWire {
                    name: name.clone(),
                    pids: p.pids.clone(),
                    vram: StatsWire::from(&p.vram),
                    busy: StatsWire::from(&p.busy),
                    gtt_max: p.gtt_max,
                })
                .collect(),
            source_bytes: rollup.source_bytes,
        }
    }
}

impl From<RollupWire> for HourRollup {
    fn from(wire: RollupWire) -> Self {
        HourRollup {
            start: wire.start,
            samples: wire.samples,
            first_ts: wire.first_ts,
            last_ts: wire.last_ts,
            first_interval: wire.first_interval,
            covered_rest: wire.covered_rest,
            gaps: GapHistogram { counts: wire.gaps.into_iter().collect() },
            metrics: wire
                .metrics
                .into_iter()
                .map(|m| RollupMetric {
                    key: m.key,
                    label: m.label,
                    unit: m.unit,
                    stats: Stats::from(m.stats),
                    first_value: m.first,
                    ceiling: m.ceiling,
                })
                .collect(),
            programs: wire
                .programs
                .into_iter()
                .map(|p| {
                    (p.name, ProgramStats { pids: p.pids, vram: Stats::from(p.vram), busy: Stats::from(p.busy), gtt_max: p.gtt_max })
                })
                .collect(),
            source_bytes: wire.source_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::summary::{Summarizer, summarize};
    use crate::history::{CpuRecord, DiskRecord, GpuProcessRecord, GpuRecord, NetworkRecord, TempRecord};

    /// Deterministic, uneven records: gaps, missing sections, a
    /// program that comes and goes, a disk that appears late.
    pub(crate) fn wobbly_records(start: f64, hours: u32, cadence: f64) -> Vec<HistoryRecord> {
        let mut records = Vec::new();
        let mut ts = start + 0.731;
        let mut seed = 0x9e37_79b9_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 10_000) as f64 / 10_000.0
        };
        while ts < start + f64::from(hours) * 3_600.0 {
            let r = next();
            // A 20-minute hole in the second hour: SysMon was closed.
            if !(start + 4_000.0..start + 5_200.0).contains(&ts) {
                records.push(HistoryRecord {
                    ts: (ts * 1000.0).round() / 1000.0,
                    interval_seconds: cadence + r * 0.03,
                    cpu: Some(CpuRecord { busy_percent: (r * 100.0) as f32, load_1m: (r * 4.0) as f32, ..Default::default() }),
                    gpu: (r > 0.1).then(|| GpuRecord {
                        busy_percent: ((r * 7.0) % 1.0 * 100.0) as f32,
                        vram_used_bytes: 20_000_000_000 + (r * 9e9) as u64,
                        vram_total_bytes: 34_208_743_424,
                        power_watts: Some((30.0 + r * 250.0) as f32),
                        processes: [
                            Some(GpuProcessRecord { pid: 1, name: "strata".into(), vram_bytes: 24_500_000_000, ..Default::default() }),
                            (r > 0.5).then(|| GpuProcessRecord {
                                pid: 2,
                                name: "ArcRaiders".into(),
                                busy_percent: (r * 95.0) as f32,
                                vram_bytes: (r * 7e9) as u64,
                                gtt_bytes: 1_000_000,
                            }),
                        ]
                        .into_iter()
                        .flatten()
                        .collect(),
                        ..Default::default()
                    }),
                    network: Some(NetworkRecord { download_bps: (r * 1e7).round(), upload_bps: 0.0 }),
                    disks: if ts > start + 2_000.0 {
                        vec![DiskRecord { device: "nvme0n1".into(), read_bps: (r * 5e6).round(), write_bps: 0.0, util_percent: (r * 30.0) as f32 }]
                    } else {
                        Vec::new()
                    },
                    temps: vec![TempRecord { sensor: "k10temp Tctl".into(), celsius: (40.0 + r * 30.0) as f32 }],
                    ..Default::default()
                });
            }
            ts += cadence + (r - 0.5) * 0.2;
        }
        records
    }

    fn hour_groups(records: &[HistoryRecord]) -> Vec<(f64, Vec<HistoryRecord>)> {
        let mut groups: Vec<(f64, Vec<HistoryRecord>)> = Vec::new();
        for record in records {
            let start = (record.ts / 3_600.0).floor() * 3_600.0;
            match groups.last_mut() {
                Some((s, group)) if *s == start => group.push(record.clone()),
                _ => groups.push((start, vec![record.clone()])),
            }
        }
        groups
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    /// The point of rollups: a summary merged from them is the summary
    /// read record by record, for every section, over ranges that cut
    /// hours, align with them, and span a hole.
    #[test]
    fn merged_rollups_summarize_exactly_like_the_records() {
        let start = 1_791_673_200.0;
        let records = wobbly_records(start, 6, 2.0);
        let groups = hour_groups(&records);
        let rollups: Vec<HourRollup> = groups
            .iter()
            .map(|(s, g)| HourRollup::from_line(&HourRollup::build(*s, g).unwrap().to_line()).expect("round trip"))
            .collect();
        for section in ["all", "cpu", "gpu", "network", "disks", "sensors", "memory"] {
            for (from, to, blocks) in [
                (start, start + 6.0 * 3_600.0, 6),
                (start - 100.0, start + 6.0 * 3_600.0 + 100.0, 3),
                (start + 1_234.5, start + 5.5 * 3_600.0, 48),
                (start, start + 6.0 * 3_600.0, 1),
            ] {
                let plain = summarize(section, from, to, blocks, |sink| {
                    records.iter().cloned().for_each(sink);
                    0
                });
                let mut summarizer = Summarizer::new(section, from, to, blocks);
                for ((_, group), rollup) in groups.iter().zip(&rollups) {
                    if summarizer.can_merge(rollup) {
                        summarizer.add_rollup(rollup);
                    } else {
                        group.iter().for_each(|r| summarizer.add_record(r));
                    }
                }
                let merged = summarizer.finish();
                let what = format!("{section} {from}..{to} /{blocks}");
                if blocks <= 6 && from <= start {
                    assert!(merged.rollup_hours > 0, "{what}: some hour should merge whole");
                }
                assert_eq!(merged.samples, plain.samples, "{what}");
                assert!(close(merged.covered_seconds, plain.covered_seconds), "{what}: covered");
                assert_eq!(merged.cadence_seconds, plain.cadence_seconds, "{what}");
                assert_eq!((merged.first_ts, merged.last_ts), (plain.first_ts, plain.last_ts), "{what}");
                assert_eq!(merged.metrics.len(), plain.metrics.len(), "{what}");
                for (m, p) in merged.metrics.iter().zip(&plain.metrics) {
                    assert_eq!((&m.key, &m.label, m.unit, m.samples), (&p.key, &p.label, p.unit, p.samples), "{what}");
                    assert_eq!((m.min, m.max, m.latest, m.p95, m.ceiling), (p.min, p.max, p.latest, p.p95, p.ceiling), "{what} {}", m.key);
                    assert!(close(m.avg, p.avg), "{what} {} avg", m.key);
                    match (m.total_bytes, p.total_bytes) {
                        (Some(a), Some(b)) => assert!(close(a, b), "{what} {} total {a} vs {b}", m.key),
                        (a, b) => assert_eq!(a, b),
                    }
                    for (a, b) in m.timeline.iter().zip(&p.timeline) {
                        match (a, b) {
                            (Some(a), Some(b)) => assert!(close(*a, *b), "{what} {} timeline", m.key),
                            _ => assert_eq!(a, b, "{what} {} timeline", m.key),
                        }
                    }
                }
                assert_eq!(merged.gpu_processes.len(), plain.gpu_processes.len(), "{what}");
                for (m, p) in merged.gpu_processes.iter().zip(&plain.gpu_processes) {
                    assert_eq!((&m.name, &m.pids, m.samples), (&p.name, &p.pids, p.samples), "{what}");
                    assert_eq!((m.vram_max_bytes, m.vram_p95_bytes, m.busy_p95_percent), (p.vram_max_bytes, p.vram_p95_bytes, p.busy_p95_percent));
                    assert!(close(m.vram_avg_bytes, p.vram_avg_bytes) && close(m.present_percent, p.present_percent));
                }
            }
        }
    }

    #[test]
    fn an_empty_hour_has_no_rollup() {
        assert!(HourRollup::build(0.0, &[]).is_none());
    }
}
