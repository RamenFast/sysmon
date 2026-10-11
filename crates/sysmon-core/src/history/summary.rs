// SPDX-License-Identifier: GPL-3.0-or-later
//! A time range of history, summarized: min / avg / p95 / max and a
//! compact timeline per reading, plus the GPU's processes over the
//! range (what the game held while the model stayed loaded).
//!
//! Streaming on purpose: a week at a 2 s cadence is 300 000 records,
//! so nothing keeps every value. min, max, avg and latest are exact.
//! p95 comes from a log-bucket sketch (DDSketch's idea): every value
//! lands in a bucket no wider than 1% of itself, so the p95 it reports
//! is within 0.5% of the exact nearest-rank p95, and is clamped to the
//! exact min and max. Rounded for display that is the same number.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use super::HistoryRecord;

/// Timeline resolution: blocks per range.
pub const TIMELINE_BUCKETS: usize = 48;

/// Relative bucket width of the p95 sketch.
const SKETCH_GAMMA: f64 = 1.01;

/// Units a metric is in, for formatting (the wire carries raw numbers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Percent,
    Bytes,
    BytesPerSecond,
    Celsius,
    Watts,
    Mhz,
    Rpm,
    Load,
}

impl Unit {
    /// The suffix the wire key carries (`gpu.vram_used_bytes`).
    fn wire(self) -> &'static str {
        match self {
            Unit::Percent => "percent",
            Unit::Bytes => "bytes",
            Unit::BytesPerSecond => "bps",
            Unit::Celsius => "celsius",
            Unit::Watts => "watts",
            Unit::Mhz => "mhz",
            Unit::Rpm => "rpm",
            Unit::Load => "load",
        }
    }
}

/// One reading over the range.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MetricSummary {
    /// Stable id: `<section>.<reading>_<unit>`, e.g. `gpu.busy_percent`,
    /// `sensors.k10temp Tctl_celsius`.
    pub key: String,
    /// What a person calls it ("VRAM used").
    pub label: String,
    pub unit: Unit,
    /// Samples that carried this reading.
    pub samples: usize,
    pub min: f64,
    pub avg: f64,
    pub p95: f64,
    pub max: f64,
    pub latest: f64,
    /// For a bytes-per-second reading: bytes moved over the range
    /// (each rate × its own window).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<f64>,
    /// The scale's ceiling when it has one (100 for a percent, the card
    /// or RAM size for memory): what the timeline is drawn against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<f64>,
    /// The mean of each timeline block, oldest first; null where no
    /// sample fell (SysMon was not recording then).
    pub timeline: Vec<Option<f64>>,
}

/// One program on the GPU over the range. Processes are grouped by
/// name per sample (a browser's GPU process and its helpers count as
/// one program while they share a name).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GpuProcessSummary {
    pub name: String,
    /// Every pid seen under this name.
    pub pids: Vec<i32>,
    /// Samples the program was on the GPU in.
    pub samples: usize,
    /// Share of the range's samples it was present in, 0–100.
    pub present_percent: f64,
    pub vram_avg_bytes: f64,
    pub vram_p95_bytes: f64,
    pub vram_max_bytes: f64,
    pub busy_avg_percent: f64,
    pub busy_p95_percent: f64,
    pub busy_max_percent: f64,
    pub gtt_max_bytes: f64,
}

/// A summarized range.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Summary {
    /// The history section, or "all".
    pub section: String,
    pub from_ts: f64,
    pub to_ts: f64,
    /// Records in the range.
    pub samples: usize,
    /// Seconds of the range the samples cover (each record's own
    /// window, never more than the gap since the record before it).
    pub covered_seconds: f64,
    /// The median gap between samples: the cadence it was recorded at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cadence_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_ts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ts: Option<f64>,
    pub timeline_block_seconds: f64,
    pub metrics: Vec<MetricSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gpu_processes: Vec<GpuProcessSummary>,
    /// Lines on disk that could not be read (a torn tail).
    pub skipped_lines: usize,
}

impl Summary {
    pub fn metric(&self, key: &str) -> Option<&MetricSummary> {
        self.metrics.iter().find(|metric| metric.key == key)
    }
}

/// Exact min/max/avg, sketched p95, block means.
#[derive(Clone, Debug)]
struct Accumulator {
    label: String,
    unit: Unit,
    ceiling: Option<f64>,
    count: usize,
    sum: f64,
    min: f64,
    max: f64,
    latest: f64,
    total: f64,
    nonpositive: u64,
    sketch: BTreeMap<i32, u64>,
    blocks: Vec<(f64, u32)>,
}

impl Accumulator {
    fn new(label: String, unit: Unit, blocks: usize) -> Accumulator {
        Accumulator {
            label,
            unit,
            ceiling: None,
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            latest: 0.0,
            total: 0.0,
            nonpositive: 0,
            sketch: BTreeMap::new(),
            blocks: vec![(0.0, 0); blocks],
        }
    }

    fn add(&mut self, value: f64, block: usize, window_seconds: f64) {
        if !value.is_finite() {
            return;
        }
        self.count += 1;
        self.sum += value;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.latest = value;
        self.total += value * window_seconds;
        if value <= 0.0 {
            self.nonpositive += 1;
        } else {
            *self.sketch.entry(sketch_index(value)).or_default() += 1;
        }
        if let Some(slot) = self.blocks.get_mut(block) {
            slot.0 += value;
            slot.1 += 1;
        }
    }

    /// Nearest-rank percentile: the smallest value with at least
    /// `fraction` of the samples at or below it.
    fn percentile(&self, fraction: f64) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let rank = ((fraction * self.count as f64).ceil() as u64).max(1);
        if rank <= self.nonpositive {
            return self.min.min(0.0).max(self.min);
        }
        let mut seen = self.nonpositive;
        for (index, count) in &self.sketch {
            seen += count;
            if seen >= rank {
                return sketch_value(*index).clamp(self.min, self.max);
            }
        }
        self.max
    }

    fn finish(&self, key: String) -> MetricSummary {
        let empty = self.count == 0;
        MetricSummary {
            key,
            label: self.label.clone(),
            unit: self.unit,
            samples: self.count,
            min: if empty { 0.0 } else { self.min },
            avg: if empty { 0.0 } else { self.sum / self.count as f64 },
            p95: self.percentile(0.95),
            max: if empty { 0.0 } else { self.max },
            latest: self.latest,
            total_bytes: (self.unit == Unit::BytesPerSecond).then_some(self.total),
            ceiling: self.ceiling,
            timeline: self.blocks.iter().map(|(sum, n)| (*n > 0).then(|| sum / f64::from(*n))).collect(),
        }
    }
}

fn sketch_index(value: f64) -> i32 {
    (value.ln() / SKETCH_GAMMA.ln()).ceil() as i32
}

/// The bucket's representative: within (γ−1)/(γ+1) ≈ 0.5% of every
/// value the bucket holds.
fn sketch_value(index: i32) -> f64 {
    2.0 * SKETCH_GAMMA.powi(index) / (SKETCH_GAMMA + 1.0)
}

/// Readings in declaration order, keyed; dynamic ones (a sensor, a
/// disk) are added the first time they appear.
struct Metrics {
    order: Vec<String>,
    by_key: HashMap<String, Accumulator>,
    blocks: usize,
}

impl Metrics {
    fn add(&mut self, key: String, label: impl FnOnce() -> String, unit: Unit, value: f64, block: usize, window: f64) {
        let blocks = self.blocks;
        let accumulator = self.by_key.entry(key.clone()).or_insert_with(|| {
            self.order.push(key);
            Accumulator::new(label(), unit, blocks)
        });
        accumulator.add(value, block, window);
    }

    fn ceiling(&mut self, key: &str, ceiling: f64) {
        if let Some(accumulator) = self.by_key.get_mut(key)
            && ceiling > 0.0
        {
            accumulator.ceiling = Some(accumulator.ceiling.map_or(ceiling, |c: f64| c.max(ceiling)));
        }
    }
}

#[derive(Default)]
struct ProgramAccumulator {
    pids: Vec<i32>,
    vram: Option<Accumulator>,
    busy: Option<Accumulator>,
    gtt_max: f64,
}

fn wants(section: &str, part: &str) -> bool {
    section == "all" || section == part
}

/// Summarize `section` ("all" or one of HISTORY_SECTIONS) over the
/// records `feed` hands in, oldest first. `feed` is called with a sink;
/// `Recording::for_each` is the usual feeder.
pub fn summarize(
    section: &str,
    from_ts: f64,
    to_ts: f64,
    blocks: usize,
    feed: impl FnOnce(&mut dyn FnMut(HistoryRecord)) -> usize,
) -> Summary {
    let blocks = blocks.max(1);
    let span = (to_ts - from_ts).max(1e-9);
    let block_seconds = span / blocks as f64;
    let mut metrics = Metrics { order: Vec::new(), by_key: HashMap::new(), blocks };
    let mut programs: BTreeMap<String, ProgramAccumulator> = BTreeMap::new();
    let mut samples = 0usize;
    let mut covered = 0.0f64;
    let mut previous_ts: Option<f64> = None;
    let mut first_ts = None;
    let mut gaps: Vec<f32> = Vec::new();

    let skipped_lines = feed(&mut |record: HistoryRecord| {
        if record.ts < from_ts || record.ts > to_ts {
            return;
        }
        samples += 1;
        first_ts.get_or_insert(record.ts);
        let since_previous = previous_ts.map_or(record.ts - from_ts, |previous| record.ts - previous);
        let window = record.interval_seconds.min(since_previous).max(0.0);
        covered += window;
        if let Some(previous) = previous_ts
            && gaps.len() < 100_000
        {
            gaps.push((record.ts - previous) as f32);
        }
        previous_ts = Some(record.ts);
        let block = (((record.ts - from_ts) / block_seconds) as usize).min(blocks - 1);
        // One reading: key `<key>_<unit>`, its label, its value.
        macro_rules! add {
            ($key:expr, $label:expr, $unit:expr, $value:expr) => {
                metrics.add(format!("{}_{}", $key, $unit.wire()), || $label.to_string(), $unit, $value, block, window)
            };
        }

        if wants(section, "cpu")
            && let Some(cpu) = &record.cpu
        {
            add!("cpu.busy", "CPU busy", Unit::Percent, f64::from(cpu.busy_percent));
            add!("cpu.kernel", "Kernel time", Unit::Percent, f64::from(cpu.kernel_percent));
            add!("cpu.iowait", "I/O wait", Unit::Percent, f64::from(cpu.iowait_percent));
            if let Some(mhz) = cpu.busy_clock_mhz {
                add!("cpu.busy_clock", "Busy clock", Unit::Mhz, f64::from(mhz));
            }
            if let Some(celsius) = cpu.temperature_celsius {
                add!("cpu.temperature", "Temperature", Unit::Celsius, f64::from(celsius));
            }
            add!("cpu.load_1m", "Load (1 min)", Unit::Load, f64::from(cpu.load_1m));
        }
        if wants(section, "memory")
            && let Some(memory) = &record.memory
        {
            add!("memory.used", "RAM used", Unit::Bytes, memory.used_bytes as f64);
            add!("memory.cached", "Cache", Unit::Bytes, memory.cached_bytes as f64);
            add!("memory.swap_used", "Swap used", Unit::Bytes, memory.swap_used_bytes as f64);
            add!("memory.committed", "Committed", Unit::Bytes, memory.committed_bytes as f64);
            for key in ["memory.used_bytes", "memory.cached_bytes"] {
                metrics.ceiling(key, memory.total_bytes as f64);
            }
        }
        if wants(section, "gpu")
            && let Some(gpu) = &record.gpu
        {
            add!("gpu.busy", "GPU busy", Unit::Percent, f64::from(gpu.busy_percent));
            add!("gpu.vram_used", "VRAM used", Unit::Bytes, gpu.vram_used_bytes as f64);
            add!("gpu.gtt_used", "GTT used", Unit::Bytes, gpu.gtt_used_bytes as f64);
            if let Some(watts) = gpu.power_watts {
                add!("gpu.power", "Power", Unit::Watts, f64::from(watts));
            }
            if let Some(mhz) = gpu.core_clock_mhz {
                add!("gpu.core_clock", "Core clock", Unit::Mhz, f64::from(mhz));
            }
            if let Some(celsius) = gpu.edge_celsius {
                add!("gpu.edge", "Edge temp", Unit::Celsius, f64::from(celsius));
            }
            if let Some(celsius) = gpu.junction_celsius {
                add!("gpu.junction", "Junction temp", Unit::Celsius, f64::from(celsius));
            }
            if let Some(celsius) = gpu.memory_celsius {
                add!("gpu.memory_temp", "Memory temp", Unit::Celsius, f64::from(celsius));
            }
            if let Some(rpm) = gpu.fan_rpm {
                add!("gpu.fan", "Fan", Unit::Rpm, f64::from(rpm));
            }
            metrics.ceiling("gpu.busy_percent", 100.0);
            metrics.ceiling("gpu.vram_used_bytes", gpu.vram_total_bytes as f64);
            // Programs: one entry per name in this sample.
            let mut this_sample: BTreeMap<&str, (f64, f64, f64, Vec<i32>)> = BTreeMap::new();
            for process in &gpu.processes {
                let entry = this_sample.entry(process.name.as_str()).or_default();
                entry.0 += process.vram_bytes as f64;
                entry.1 += f64::from(process.busy_percent);
                entry.2 += process.gtt_bytes as f64;
                entry.3.push(process.pid);
            }
            for (name, (vram, busy, gtt, pids)) in this_sample {
                let program = programs.entry(name.to_string()).or_default();
                for pid in pids {
                    if !program.pids.contains(&pid) {
                        program.pids.push(pid);
                    }
                }
                program
                    .vram
                    .get_or_insert_with(|| Accumulator::new(String::new(), Unit::Bytes, 1))
                    .add(vram, 0, window);
                program
                    .busy
                    .get_or_insert_with(|| Accumulator::new(String::new(), Unit::Percent, 1))
                    .add(busy.min(100.0), 0, window);
                program.gtt_max = program.gtt_max.max(gtt);
            }
        }
        if wants(section, "network")
            && let Some(network) = &record.network
        {
            add!("network.download", "Download", Unit::BytesPerSecond, network.download_bps);
            add!("network.upload", "Upload", Unit::BytesPerSecond, network.upload_bps);
        }
        if wants(section, "disks") && !record.disks.is_empty() {
            let read: f64 = record.disks.iter().map(|d| d.read_bps).sum();
            let write: f64 = record.disks.iter().map(|d| d.write_bps).sum();
            metrics.add("disks.read_bps".into(), || "Read (all)".into(), Unit::BytesPerSecond, read, block, window);
            metrics.add("disks.write_bps".into(), || "Write (all)".into(), Unit::BytesPerSecond, write, block, window);
            for disk in &record.disks {
                let key = format!("disks.{} busy_percent", disk.device);
                metrics.add(key, || format!("{} busy", disk.device), Unit::Percent, f64::from(disk.util_percent), block, window);
            }
        }
        if wants(section, "sensors") {
            for temp in &record.temps {
                let key = format!("sensors.{}_celsius", temp.sensor);
                metrics.add(key, || temp.sensor.clone(), Unit::Celsius, f64::from(temp.celsius), block, window);
            }
            for fan in &record.fans {
                let key = format!("sensors.{}_rpm", fan.sensor);
                metrics.add(key, || fan.sensor.clone(), Unit::Rpm, f64::from(fan.rpm), block, window);
            }
        }
    });

    let cadence_seconds = (!gaps.is_empty()).then(|| {
        gaps.sort_by(f32::total_cmp);
        f64::from(gaps[gaps.len() / 2])
    });
    let mut gpu_processes: Vec<GpuProcessSummary> = programs
        .into_iter()
        .filter_map(|(name, program)| {
            let vram = program.vram?.finish(String::new());
            let busy = program.busy?.finish(String::new());
            Some(GpuProcessSummary {
                name,
                pids: program.pids,
                samples: vram.samples,
                present_percent: if samples > 0 { vram.samples as f64 / samples as f64 * 100.0 } else { 0.0 },
                vram_avg_bytes: vram.avg,
                vram_p95_bytes: vram.p95,
                vram_max_bytes: vram.max,
                busy_avg_percent: busy.avg,
                busy_p95_percent: busy.p95,
                busy_max_percent: busy.max,
                gtt_max_bytes: program.gtt_max,
            })
        })
        .collect();
    gpu_processes.sort_by(|a, b| b.vram_max_bytes.total_cmp(&a.vram_max_bytes).then(a.name.cmp(&b.name)));

    let percent_keys: Vec<String> =
        metrics.order.iter().filter(|key| key.ends_with("_percent")).cloned().collect();
    for key in percent_keys {
        metrics.ceiling(&key, 100.0);
    }
    Summary {
        section: section.to_string(),
        from_ts,
        to_ts,
        samples,
        covered_seconds: covered,
        cadence_seconds,
        first_ts,
        last_ts: previous_ts,
        timeline_block_seconds: block_seconds,
        metrics: metrics.order.iter().map(|key| metrics.by_key[key].finish(key.clone())).collect(),
        gpu_processes,
        skipped_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{CpuRecord, GpuProcessRecord, GpuRecord, NetworkRecord};

    fn feed(records: Vec<HistoryRecord>) -> impl FnOnce(&mut dyn FnMut(HistoryRecord)) -> usize {
        move |sink| {
            records.into_iter().for_each(sink);
            0
        }
    }

    #[test]
    fn exact_min_avg_max_and_a_p95_within_half_a_percent() {
        // 1000 samples, busy = a shuffled ramp 0.1 … 100.0.
        let records: Vec<HistoryRecord> = (0..1000)
            .map(|i| {
                let value = f64::from((i * 7919) % 1000 + 1) / 10.0;
                HistoryRecord {
                    ts: 1000.0 + f64::from(i),
                    interval_seconds: 1.0,
                    cpu: Some(CpuRecord { busy_percent: value as f32, ..Default::default() }),
                    ..Default::default()
                }
            })
            .collect();
        let summary = summarize("cpu", 1000.0, 1999.0, 10, feed(records));
        let busy = summary.metric("cpu.busy_percent").expect("busy");
        assert_eq!(busy.samples, 1000);
        assert!((busy.min - 0.1).abs() < 1e-4 && (busy.max - 100.0).abs() < 1e-4);
        assert!((busy.avg - 50.05).abs() < 1e-3, "avg {}", busy.avg);
        // Exact nearest-rank p95 of 0.1..100.0 step 0.1 is 95.0.
        assert!((busy.p95 - 95.0).abs() <= 95.0 * 0.005, "p95 {}", busy.p95);
        assert_eq!(busy.timeline.len(), 10);
        assert_eq!(busy.ceiling, Some(100.0));
        assert_eq!(summary.cadence_seconds, Some(1.0));
        assert!((summary.covered_seconds - 999.0).abs() < 1e-9, "the first sample's window starts at the range");
    }

    #[test]
    fn rates_total_their_bytes_and_gaps_show_as_empty_blocks() {
        // 10 samples at 2 … 20 s of a 40 s range, nothing after.
        let records: Vec<HistoryRecord> = (0..10)
            .map(|i| HistoryRecord {
                ts: f64::from(i) * 2.0 + 2.0,
                interval_seconds: 2.0,
                network: Some(NetworkRecord { download_bps: 1_000_000.0, upload_bps: 0.0 }),
                ..Default::default()
            })
            .collect();
        let summary = summarize("network", 0.0, 40.0, 4, feed(records));
        let down = summary.metric("network.download_bps").unwrap();
        assert_eq!(down.total_bytes, Some(20_000_000.0), "1 MB/s for 20 s");
        assert_eq!(down.timeline[0], Some(1_000_000.0));
        assert_eq!(down.timeline[3], None, "no sample, no fake zero");
        assert!((summary.covered_seconds - 20.0).abs() < 1e-9);
    }

    #[test]
    fn gpu_programs_group_by_name_and_count_their_presence() {
        let sample = |ts: f64, game: Option<u64>| HistoryRecord {
            ts,
            interval_seconds: 1.0,
            gpu: Some(GpuRecord {
                busy_percent: 50.0,
                vram_used_bytes: 30 << 30,
                vram_total_bytes: 32 << 30,
                processes: [
                    Some(GpuProcessRecord { pid: 10, name: "strata".into(), vram_bytes: 24 << 30, ..Default::default() }),
                    game.map(|gib| GpuProcessRecord {
                        pid: 20,
                        name: "ArcRaiders".into(),
                        busy_percent: 90.0,
                        vram_bytes: gib << 30,
                        ..Default::default()
                    }),
                    // Two helpers of one program share a name.
                    Some(GpuProcessRecord { pid: 30, name: "thorium".into(), vram_bytes: 100 << 20, ..Default::default() }),
                    Some(GpuProcessRecord { pid: 31, name: "thorium".into(), vram_bytes: 50 << 20, ..Default::default() }),
                ]
                .into_iter()
                .flatten()
                .collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let records = vec![sample(1.0, None), sample(2.0, Some(4)), sample(3.0, Some(6)), sample(4.0, None)];
        let summary = summarize("gpu", 0.0, 4.0, 4, feed(records));
        let names: Vec<&str> = summary.gpu_processes.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["strata", "ArcRaiders", "thorium"], "by peak VRAM");
        let game = &summary.gpu_processes[1];
        assert_eq!(game.samples, 2);
        assert_eq!(game.present_percent, 50.0);
        assert_eq!(game.vram_max_bytes, (6u64 << 30) as f64);
        assert_eq!(game.vram_avg_bytes, (5u64 << 30) as f64, "averaged over the samples it ran in");
        let browser = &summary.gpu_processes[2];
        assert_eq!(browser.pids, [30, 31]);
        assert_eq!(browser.vram_max_bytes, (150u64 << 20) as f64, "helpers sum within a sample");
        assert_eq!(summary.metric("gpu.vram_used_bytes").unwrap().ceiling, Some((32u64 << 30) as f64));
    }

    #[test]
    fn an_empty_range_is_empty_not_zeroes() {
        let summary = summarize("gpu", 0.0, 60.0, 4, feed(Vec::new()));
        assert_eq!(summary.samples, 0);
        assert!(summary.metrics.is_empty());
        assert_eq!(summary.first_ts, None);
    }
}
