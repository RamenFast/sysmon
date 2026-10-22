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
//!
//! Every statistic here is mergeable ([`Stats::merge`]), which is what
//! makes long ranges fast (3.5): a sealed hour is condensed once into
//! an [`HourRollup`](super::rollup::HourRollup), and a 30-day or 1-year
//! summary merges those instead of re-reading every record. The
//! [`Summarizer`] takes either, in time order, and the result is the
//! same as reading the records one by one: an hour is merged whole
//! only when all of it lies inside the range and inside one timeline
//! block; anything else is read record by record. The first record of
//! a merged hour gets its window from the record before it, exactly as
//! a record-by-record read would (`first_value` × that window).

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::{GpuRecord, HistoryRecord};
use super::rollup::HourRollup;

/// Timeline resolution: blocks per range.
pub const TIMELINE_BUCKETS: usize = 48;

/// Relative bucket width of the p95 sketch.
const SKETCH_GAMMA: f64 = 1.01;

/// Units a metric is in, for formatting (the wire carries raw numbers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn wire(self) -> &'static str {
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
    /// The median gap between samples: the cadence it was recorded at
    /// (to 10 ms).
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
    /// The NVIDIA card's programs (3.7), the same shape.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nvidia_processes: Vec<GpuProcessSummary>,
    /// Lines on disk that could not be read (a torn tail).
    pub skipped_lines: usize,
    /// Hours merged from their rollups instead of read record by
    /// record (0: every record was read). Same numbers either way.
    pub rollup_hours: usize,
}

impl Summary {
    pub fn metric(&self, key: &str) -> Option<&MetricSummary> {
        self.metrics.iter().find(|metric| metric.key == key)
    }
}

/// Log-bucket counts in a dense array (`offset` is the index of
/// `counts[0]`). A reading spans a few hundred to a few thousand
/// buckets, so merging a year of hourly sketches is array adds, not
/// map inserts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sketch {
    offset: i32,
    counts: Vec<u64>,
}

impl Sketch {
    pub fn add(&mut self, index: i32, count: u64) {
        if self.counts.is_empty() {
            self.offset = index;
            self.counts.push(count);
            return;
        }
        if index < self.offset {
            let grow = (self.offset - index) as usize;
            self.counts.splice(0..0, std::iter::repeat_n(0, grow));
            self.offset = index;
        }
        let slot = (index - self.offset) as usize;
        if slot >= self.counts.len() {
            self.counts.resize(slot + 1, 0);
        }
        self.counts[slot] += count;
    }

    /// (index, count) for every non-empty bucket, ascending.
    pub fn buckets(&self) -> impl Iterator<Item = (i32, u64)> + '_ {
        self.counts.iter().enumerate().filter(|(_, c)| **c > 0).map(|(i, c)| (self.offset + i as i32, *c))
    }

    pub fn merge(&mut self, other: &Sketch) {
        for (index, count) in other.buckets() {
            self.add(index, count);
        }
    }
}

/// Exact count / sum / min / max / latest / total, sketched p95. Two
/// of these over consecutive stretches merge into the one a single
/// pass over both would have built.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub latest: f64,
    /// Σ value × its record's window (bytes moved, for a rate).
    pub total: f64,
    pub nonpositive: u64,
    pub sketch: Sketch,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            latest: 0.0,
            total: 0.0,
            nonpositive: 0,
            sketch: Sketch::default(),
        }
    }
}

impl Stats {
    pub fn add(&mut self, value: f64, window_seconds: f64) {
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
            self.sketch.add(sketch_index(value), 1);
        }
    }

    /// Fold in the stats of a stretch that came *after* this one.
    pub fn merge(&mut self, later: &Stats) {
        if later.count == 0 {
            return;
        }
        self.count += later.count;
        self.sum += later.sum;
        self.min = self.min.min(later.min);
        self.max = self.max.max(later.max);
        self.latest = later.latest;
        self.total += later.total;
        self.nonpositive += later.nonpositive;
        self.sketch.merge(&later.sketch);
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
        for (index, count) in self.sketch.buckets() {
            seen += count;
            if seen >= rank {
                return sketch_value(index).clamp(self.min, self.max);
            }
        }
        self.max
    }
}

pub(crate) fn sketch_index(value: f64) -> i32 {
    (value.ln() / SKETCH_GAMMA.ln()).ceil() as i32
}

/// The bucket's representative: within (γ−1)/(γ+1) ≈ 0.5% of every
/// value the bucket holds.
fn sketch_value(index: i32) -> f64 {
    2.0 * SKETCH_GAMMA.powi(index) / (SKETCH_GAMMA + 1.0)
}

/// Gaps between consecutive samples, counted in 10 ms buckets: the
/// median is the cadence, and two histograms merge by adding.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GapHistogram {
    pub counts: BTreeMap<u32, u64>,
}

impl GapHistogram {
    pub fn add(&mut self, gap_seconds: f64) {
        if gap_seconds.is_finite() && gap_seconds >= 0.0 {
            let centiseconds = (gap_seconds * 100.0).round().min(f64::from(u32::MAX)) as u32;
            *self.counts.entry(centiseconds).or_default() += 1;
        }
    }

    pub fn merge(&mut self, other: &GapHistogram) {
        for (gap, count) in &other.counts {
            *self.counts.entry(*gap).or_default() += count;
        }
    }

    /// The sorted gaps' element at len/2 (the upper median).
    pub fn median_seconds(&self) -> Option<f64> {
        let total: u64 = self.counts.values().sum();
        if total == 0 {
            return None;
        }
        let mut seen = 0;
        for (gap, count) in &self.counts {
            seen += count;
            if seen > total / 2 {
                return Some(f64::from(*gap) / 100.0);
            }
        }
        None
    }
}

/// Where one record's readings go: a summary's accumulators or an
/// hour's rollup. Both see the same keys, labels and values in the
/// same order, from one [`visit`].
pub(crate) trait ReadingSink {
    fn value(&mut self, key: &str, label: &dyn Fn() -> String, unit: Unit, value: f64);
    /// The scale ceiling of a key already given a value this record.
    fn ceiling(&mut self, key: &str, ceiling: f64);
}

pub(crate) fn wants(section: &str, part: &str) -> bool {
    section == "all" || section == part
}

/// The section a metric key belongs to (`gpu.busy_percent` → gpu).
pub(crate) fn key_section(key: &str) -> &str {
    key.split('.').next().unwrap_or(key)
}

/// Every reading of `section` ("all" or one of HISTORY_SECTIONS) a
/// record carries, in declaration order.
pub(crate) fn visit(record: &HistoryRecord, section: &str, scratch: &mut String, sink: &mut impl ReadingSink) {
    if wants(section, "cpu")
        && let Some(cpu) = &record.cpu
    {
        sink.value("cpu.busy_percent", &|| "CPU busy".into(), Unit::Percent, f64::from(cpu.busy_percent));
        sink.value("cpu.kernel_percent", &|| "Kernel time".into(), Unit::Percent, f64::from(cpu.kernel_percent));
        sink.value("cpu.iowait_percent", &|| "I/O wait".into(), Unit::Percent, f64::from(cpu.iowait_percent));
        if let Some(mhz) = cpu.busy_clock_mhz {
            sink.value("cpu.busy_clock_mhz", &|| "Busy clock".into(), Unit::Mhz, f64::from(mhz));
        }
        if let Some(celsius) = cpu.temperature_celsius {
            sink.value("cpu.temperature_celsius", &|| "Temperature".into(), Unit::Celsius, f64::from(celsius));
        }
        sink.value("cpu.load_1m_load", &|| "Load (1 min)".into(), Unit::Load, f64::from(cpu.load_1m));
    }
    if wants(section, "memory")
        && let Some(memory) = &record.memory
    {
        sink.value("memory.used_bytes", &|| "RAM used".into(), Unit::Bytes, memory.used_bytes as f64);
        sink.value("memory.cached_bytes", &|| "Cache".into(), Unit::Bytes, memory.cached_bytes as f64);
        sink.value("memory.swap_used_bytes", &|| "Swap used".into(), Unit::Bytes, memory.swap_used_bytes as f64);
        sink.value("memory.committed_bytes", &|| "Committed".into(), Unit::Bytes, memory.committed_bytes as f64);
        for key in ["memory.used_bytes", "memory.cached_bytes"] {
            sink.ceiling(key, memory.total_bytes as f64);
        }
    }
    if wants(section, "gpu")
        && let Some(gpu) = &record.gpu
    {
        sink.value("gpu.busy_percent", &|| "GPU busy".into(), Unit::Percent, f64::from(gpu.busy_percent));
        sink.value("gpu.vram_used_bytes", &|| "VRAM used".into(), Unit::Bytes, gpu.vram_used_bytes as f64);
        sink.value("gpu.gtt_used_bytes", &|| "GTT used".into(), Unit::Bytes, gpu.gtt_used_bytes as f64);
        if let Some(watts) = gpu.power_watts {
            sink.value("gpu.power_watts", &|| "Power".into(), Unit::Watts, f64::from(watts));
        }
        if let Some(mhz) = gpu.core_clock_mhz {
            sink.value("gpu.core_clock_mhz", &|| "Core clock".into(), Unit::Mhz, f64::from(mhz));
        }
        if let Some(celsius) = gpu.edge_celsius {
            sink.value("gpu.edge_celsius", &|| "Edge temp".into(), Unit::Celsius, f64::from(celsius));
        }
        if let Some(celsius) = gpu.junction_celsius {
            sink.value("gpu.junction_celsius", &|| "Junction temp".into(), Unit::Celsius, f64::from(celsius));
        }
        if let Some(celsius) = gpu.memory_celsius {
            sink.value("gpu.memory_temp_celsius", &|| "Memory temp".into(), Unit::Celsius, f64::from(celsius));
        }
        if let Some(rpm) = gpu.fan_rpm {
            sink.value("gpu.fan_rpm", &|| "Fan".into(), Unit::Rpm, f64::from(rpm));
        }
        sink.ceiling("gpu.busy_percent", 100.0);
        sink.ceiling("gpu.vram_used_bytes", gpu.vram_total_bytes as f64);
    }
    if wants(section, "nvidia")
        && let Some(gpu) = &record.nvidia
    {
        // Labelled apart from the AMD card's, so "all" never shows two
        // lines both called "GPU busy".
        sink.value("nvidia.busy_percent", &|| "NVIDIA busy".into(), Unit::Percent, f64::from(gpu.busy_percent));
        sink.value("nvidia.vram_used_bytes", &|| "NVIDIA VRAM used".into(), Unit::Bytes, gpu.vram_used_bytes as f64);
        if let Some(watts) = gpu.power_watts {
            sink.value("nvidia.power_watts", &|| "NVIDIA power".into(), Unit::Watts, f64::from(watts));
        }
        if let Some(mhz) = gpu.core_clock_mhz {
            sink.value("nvidia.core_clock_mhz", &|| "NVIDIA core clock".into(), Unit::Mhz, f64::from(mhz));
        }
        if let Some(celsius) = gpu.core_celsius {
            sink.value("nvidia.core_celsius", &|| "NVIDIA temp".into(), Unit::Celsius, f64::from(celsius));
        }
        if let Some(rpm) = gpu.fan_rpm {
            sink.value("nvidia.fan_rpm", &|| "NVIDIA fan".into(), Unit::Rpm, f64::from(rpm));
        }
        sink.ceiling("nvidia.busy_percent", 100.0);
        sink.ceiling("nvidia.vram_used_bytes", gpu.vram_total_bytes as f64);
    }
    if wants(section, "network")
        && let Some(network) = &record.network
    {
        sink.value("network.download_bps", &|| "Download".into(), Unit::BytesPerSecond, network.download_bps);
        sink.value("network.upload_bps", &|| "Upload".into(), Unit::BytesPerSecond, network.upload_bps);
    }
    if wants(section, "disks") && !record.disks.is_empty() {
        let read: f64 = record.disks.iter().map(|d| d.read_bps).sum();
        let write: f64 = record.disks.iter().map(|d| d.write_bps).sum();
        sink.value("disks.read_bps", &|| "Read (all)".into(), Unit::BytesPerSecond, read);
        sink.value("disks.write_bps", &|| "Write (all)".into(), Unit::BytesPerSecond, write);
        for disk in &record.disks {
            scratch.clear();
            let _ = write!(scratch, "disks.{} busy_percent", disk.device);
            sink.value(scratch, &|| format!("{} busy", disk.device), Unit::Percent, f64::from(disk.util_percent));
        }
    }
    if wants(section, "sensors") {
        for temp in &record.temps {
            scratch.clear();
            let _ = write!(scratch, "sensors.{}_celsius", temp.sensor);
            sink.value(scratch, &|| temp.sensor.clone(), Unit::Celsius, f64::from(temp.celsius));
        }
        for fan in &record.fans {
            scratch.clear();
            let _ = write!(scratch, "sensors.{}_rpm", fan.sensor);
            sink.value(scratch, &|| fan.sensor.clone(), Unit::Rpm, f64::from(fan.rpm));
        }
    }
}

/// One GPU's programs in one sample (`record.gpu` or
/// `record.nvidia`), grouped by name: (name, vram, busy, gtt, pids),
/// names in order.
pub(crate) fn sample_programs(gpu: Option<&GpuRecord>) -> Vec<(&str, f64, f64, f64, Vec<i32>)> {
    let Some(gpu) = gpu else { return Vec::new() };
    let mut this_sample: BTreeMap<&str, (f64, f64, f64, Vec<i32>)> = BTreeMap::new();
    for process in &gpu.processes {
        let entry = this_sample.entry(process.name.as_str()).or_default();
        entry.0 += process.vram_bytes as f64;
        entry.1 += f64::from(process.busy_percent);
        entry.2 += process.gtt_bytes as f64;
        entry.3.push(process.pid);
    }
    this_sample.into_iter().map(|(name, (vram, busy, gtt, pids))| (name, vram, busy.min(100.0), gtt, pids)).collect()
}

/// One program's running account (summary or rollup).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProgramStats {
    pub pids: Vec<i32>,
    pub vram: Stats,
    pub busy: Stats,
    pub gtt_max: f64,
}

impl ProgramStats {
    pub fn add_sample(&mut self, vram: f64, busy: f64, gtt: f64, pids: &[i32], window: f64) {
        for pid in pids {
            if !self.pids.contains(pid) {
                self.pids.push(*pid);
            }
        }
        self.vram.add(vram, window);
        self.busy.add(busy, window);
        self.gtt_max = self.gtt_max.max(gtt);
    }

    pub fn merge(&mut self, later: &ProgramStats) {
        for pid in &later.pids {
            if !self.pids.contains(pid) {
                self.pids.push(*pid);
            }
        }
        self.vram.merge(&later.vram);
        self.busy.merge(&later.busy);
        self.gtt_max = self.gtt_max.max(later.gtt_max);
    }
}

/// One reading's accumulation over a range.
#[derive(Clone, Debug)]
struct Accumulator {
    label: String,
    unit: Unit,
    ceiling: Option<f64>,
    stats: Stats,
    blocks: Vec<(f64, u64)>,
}

impl Accumulator {
    fn new(label: String, unit: Unit, blocks: usize) -> Accumulator {
        Accumulator { label, unit, ceiling: None, stats: Stats::default(), blocks: vec![(0.0, 0); blocks] }
    }

    fn raise_ceiling(&mut self, ceiling: f64) {
        if ceiling > 0.0 {
            self.ceiling = Some(self.ceiling.map_or(ceiling, |c: f64| c.max(ceiling)));
        }
    }

    fn finish(&self, key: String) -> MetricSummary {
        let stats = &self.stats;
        let empty = stats.count == 0;
        MetricSummary {
            key,
            label: self.label.clone(),
            unit: self.unit,
            samples: stats.count as usize,
            min: if empty { 0.0 } else { stats.min },
            avg: if empty { 0.0 } else { stats.sum / stats.count as f64 },
            p95: stats.percentile(0.95),
            max: if empty { 0.0 } else { stats.max },
            latest: stats.latest,
            total_bytes: (self.unit == Unit::BytesPerSecond).then_some(stats.total),
            ceiling: self.ceiling,
            timeline: self.blocks.iter().map(|(sum, n)| (*n > 0).then(|| sum / *n as f64)).collect(),
        }
    }
}

/// Readings in first-seen order, keyed; dynamic ones (a sensor, a
/// disk) are added the first time they appear.
struct Metrics {
    order: Vec<String>,
    by_key: HashMap<String, Accumulator>,
    blocks: usize,
}

impl Metrics {
    fn entry(&mut self, key: &str, label: &dyn Fn() -> String, unit: Unit) -> &mut Accumulator {
        if !self.by_key.contains_key(key) {
            self.order.push(key.to_string());
            self.by_key.insert(key.to_string(), Accumulator::new(label(), unit, self.blocks));
        }
        self.by_key.get_mut(key).expect("just inserted")
    }
}

/// The record-by-record sink: values go into the current block.
struct RecordSink<'a> {
    metrics: &'a mut Metrics,
    block: usize,
    window: f64,
}

impl ReadingSink for RecordSink<'_> {
    fn value(&mut self, key: &str, label: &dyn Fn() -> String, unit: Unit, value: f64) {
        let block = self.block;
        let window = self.window;
        let accumulator = self.metrics.entry(key, label, unit);
        if !value.is_finite() {
            return;
        }
        accumulator.stats.add(value, window);
        if let Some(slot) = accumulator.blocks.get_mut(block) {
            slot.0 += value;
            slot.1 += 1;
        }
    }

    fn ceiling(&mut self, key: &str, ceiling: f64) {
        if let Some(accumulator) = self.metrics.by_key.get_mut(key) {
            accumulator.raise_ceiling(ceiling);
        }
    }
}

/// One GPU's programs over the range, biggest peak VRAM first.
fn program_summaries(programs: BTreeMap<String, ProgramStats>, samples: usize) -> Vec<GpuProcessSummary> {
    let mut summaries: Vec<GpuProcessSummary> = programs
        .into_iter()
        .filter(|(_, program)| program.vram.count > 0)
        .map(|(name, program)| {
            let vram = Accumulator { stats: program.vram, ..Accumulator::new(String::new(), Unit::Bytes, 1) }
                .finish(String::new());
            let busy = Accumulator { stats: program.busy, ..Accumulator::new(String::new(), Unit::Percent, 1) }
                .finish(String::new());
            GpuProcessSummary {
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
            }
        })
        .collect();
    summaries.sort_by(|a, b| b.vram_max_bytes.total_cmp(&a.vram_max_bytes).then(a.name.cmp(&b.name)));
    summaries
}

/// A summary being built from records and hour rollups, fed oldest
/// first.
pub struct Summarizer {
    section: String,
    from_ts: f64,
    to_ts: f64,
    blocks: usize,
    block_seconds: f64,
    metrics: Metrics,
    programs: BTreeMap<String, ProgramStats>,
    nvidia_programs: BTreeMap<String, ProgramStats>,
    samples: usize,
    covered: f64,
    previous_ts: Option<f64>,
    first_ts: Option<f64>,
    gaps: GapHistogram,
    skipped_lines: usize,
    rollup_hours: usize,
    scratch: String,
}

impl Summarizer {
    pub fn new(section: &str, from_ts: f64, to_ts: f64, blocks: usize) -> Summarizer {
        let blocks = blocks.max(1);
        let span = (to_ts - from_ts).max(1e-9);
        Summarizer {
            section: section.to_string(),
            from_ts,
            to_ts,
            blocks,
            block_seconds: span / blocks as f64,
            metrics: Metrics { order: Vec::new(), by_key: HashMap::new(), blocks },
            programs: BTreeMap::new(),
            nvidia_programs: BTreeMap::new(),
            samples: 0,
            covered: 0.0,
            previous_ts: None,
            first_ts: None,
            gaps: GapHistogram::default(),
            skipped_lines: 0,
            rollup_hours: 0,
            scratch: String::new(),
        }
    }

    /// The timeline block a timestamp falls in.
    pub fn block_of(&self, ts: f64) -> usize {
        (((ts - self.from_ts) / self.block_seconds).max(0.0) as usize).min(self.blocks - 1)
    }

    pub fn range(&self) -> (f64, f64) {
        (self.from_ts, self.to_ts)
    }

    /// Whether an hour rollup can be merged whole: every record of it
    /// inside the range and inside one timeline block.
    pub fn can_merge(&self, rollup: &HourRollup) -> bool {
        rollup.first_ts >= self.from_ts
            && rollup.last_ts <= self.to_ts
            && self.block_of(rollup.first_ts) == self.block_of(rollup.last_ts)
            && self.previous_ts.is_none_or(|previous| previous < rollup.first_ts)
    }

    pub fn note_skipped(&mut self, lines: usize) {
        self.skipped_lines += lines;
    }

    /// The window the next record starting at `ts` covers.
    fn window_for(&self, ts: f64, interval_seconds: f64) -> f64 {
        let since_previous = self.previous_ts.map_or(ts - self.from_ts, |previous| ts - previous);
        interval_seconds.min(since_previous).max(0.0)
    }

    pub fn add_record(&mut self, record: &HistoryRecord) {
        if record.ts < self.from_ts || record.ts > self.to_ts {
            return;
        }
        self.samples += 1;
        self.first_ts.get_or_insert(record.ts);
        let window = self.window_for(record.ts, record.interval_seconds);
        self.covered += window;
        if let Some(previous) = self.previous_ts {
            self.gaps.add(record.ts - previous);
        }
        self.previous_ts = Some(record.ts);
        let block = self.block_of(record.ts);
        let mut sink = RecordSink { metrics: &mut self.metrics, block, window };
        visit(record, &self.section, &mut self.scratch, &mut sink);
        for (part, gpu) in [("gpu", record.gpu.as_ref()), ("nvidia", record.nvidia.as_ref())] {
            if !wants(&self.section, part) {
                continue;
            }
            let programs = if part == "gpu" { &mut self.programs } else { &mut self.nvidia_programs };
            for (name, vram, busy, gtt, pids) in sample_programs(gpu) {
                let program = match programs.get_mut(name) {
                    Some(program) => program,
                    None => programs.entry(name.to_string()).or_default(),
                };
                program.add_sample(vram, busy, gtt, &pids, window);
            }
        }
    }

    /// Merge a whole hour (the caller checked [`Summarizer::can_merge`]).
    pub fn add_rollup(&mut self, rollup: &HourRollup) {
        let first_window = self.window_for(rollup.first_ts, rollup.first_interval);
        self.covered += first_window + rollup.covered_rest;
        if let Some(previous) = self.previous_ts {
            self.gaps.add(rollup.first_ts - previous);
        }
        self.gaps.merge(&rollup.gaps);
        self.samples += rollup.samples as usize;
        self.first_ts.get_or_insert(rollup.first_ts);
        self.previous_ts = Some(rollup.last_ts);
        self.rollup_hours += 1;
        let block = self.block_of(rollup.first_ts);
        for metric in &rollup.metrics {
            if !wants(&self.section, key_section(&metric.key)) {
                continue;
            }
            let accumulator = self.metrics.entry(&metric.key, &|| metric.label.clone(), metric.unit);
            let mut stats = metric.stats.clone();
            if let Some(value) = metric.first_value {
                stats.total += value * first_window;
            }
            accumulator.stats.merge(&stats);
            if let Some(slot) = accumulator.blocks.get_mut(block) {
                slot.0 += metric.stats.sum;
                slot.1 += metric.stats.count;
            }
            if let Some(ceiling) = metric.ceiling {
                accumulator.raise_ceiling(ceiling);
            }
        }
        for (part, from) in [("gpu", &rollup.programs), ("nvidia", &rollup.nvidia_programs)] {
            if !wants(&self.section, part) {
                continue;
            }
            let programs = if part == "gpu" { &mut self.programs } else { &mut self.nvidia_programs };
            for (name, program) in from {
                match programs.get_mut(name) {
                    Some(existing) => existing.merge(program),
                    None => {
                        programs.insert(name.clone(), program.clone());
                    }
                }
            }
        }
    }

    pub fn finish(self) -> Summary {
        let samples = self.samples;
        let gpu_processes = program_summaries(self.programs, samples);
        let nvidia_processes = program_summaries(self.nvidia_programs, samples);

        let mut metrics = self.metrics;
        let percent_keys: Vec<String> = metrics.order.iter().filter(|key| key.ends_with("_percent")).cloned().collect();
        for key in percent_keys {
            if let Some(accumulator) = metrics.by_key.get_mut(&key) {
                accumulator.raise_ceiling(100.0);
            }
        }
        Summary {
            section: self.section,
            from_ts: self.from_ts,
            to_ts: self.to_ts,
            samples,
            covered_seconds: self.covered,
            cadence_seconds: self.gaps.median_seconds(),
            first_ts: self.first_ts,
            last_ts: self.previous_ts,
            timeline_block_seconds: self.block_seconds,
            metrics: metrics.order.iter().map(|key| metrics.by_key[key].finish(key.clone())).collect(),
            gpu_processes,
            nvidia_processes,
            skipped_lines: self.skipped_lines,
            rollup_hours: self.rollup_hours,
        }
    }
}

/// Summarize `section` ("all" or one of HISTORY_SECTIONS) over the
/// records `feed` hands in, oldest first. `feed` is called with a sink
/// and returns the lines it could not read. (`store::summarize_dir`
/// reads a history directory with rollups; this is the plain path.)
pub fn summarize(
    section: &str,
    from_ts: f64,
    to_ts: f64,
    blocks: usize,
    feed: impl FnOnce(&mut dyn FnMut(HistoryRecord)) -> usize,
) -> Summary {
    let mut summarizer = Summarizer::new(section, from_ts, to_ts, blocks);
    let skipped = feed(&mut |record: HistoryRecord| summarizer.add_record(&record));
    summarizer.note_skipped(skipped);
    summarizer.finish()
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
        assert_eq!(summary.rollup_hours, 0);
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

    #[test]
    fn a_dense_sketch_grows_both_ways_and_merges() {
        let mut a = Sketch::default();
        a.add(10, 1);
        a.add(7, 2);
        a.add(12, 1);
        let mut b = Sketch::default();
        b.add(5, 1);
        b.add(10, 3);
        a.merge(&b);
        assert_eq!(a.buckets().collect::<Vec<_>>(), vec![(5, 1), (7, 2), (10, 4), (12, 1)]);
    }

    #[test]
    fn stats_merge_like_one_pass() {
        let values: Vec<f64> = (0..500).map(|i| f64::from((i * 37) % 101) - 3.0).collect();
        let mut whole = Stats::default();
        values.iter().for_each(|v| whole.add(*v, 2.0));
        let mut first = Stats::default();
        let mut second = Stats::default();
        values[..200].iter().for_each(|v| first.add(*v, 2.0));
        values[200..].iter().for_each(|v| second.add(*v, 2.0));
        first.merge(&second);
        assert_eq!(first.count, whole.count);
        assert_eq!(first.sketch, whole.sketch);
        assert_eq!((first.min, first.max, first.latest), (whole.min, whole.max, whole.latest));
        assert!((first.sum - whole.sum).abs() < 1e-9 && (first.total - whole.total).abs() < 1e-9);
        assert_eq!(first.percentile(0.95), whole.percentile(0.95));
    }

    #[test]
    fn the_cadence_is_the_median_gap_to_ten_milliseconds() {
        let mut gaps = GapHistogram::default();
        for gap in [5.007, 5.034, 5.035, 5.2, 300.0] {
            gaps.add(gap);
        }
        assert_eq!(gaps.median_seconds(), Some(5.04), "5.035 rounds to 5.04");
        assert_eq!(GapHistogram::default().median_seconds(), None);
    }
}
