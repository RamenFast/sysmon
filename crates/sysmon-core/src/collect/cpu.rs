// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU: /proc/stat deltas (per-core busy %), /proc/loadavg, cpufreq
//! sysfs. Busy % uses the htop identity — busy = total − idle −
//! iowait — over the window between two samples; guest time is
//! already folded into user by the kernel, so summing every column
//! except guest/guest_nice avoids double counting.

use std::fs;

use crate::snapshot::CpuSnapshot;

use super::read::{SelfInterval, read_trimmed, read_u64};

/// One /proc/stat cpu line, in ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuTicks {
    pub total: u64,
    /// idle + iowait: neither is work.
    pub idle: u64,
    /// The iowait share of `idle`, kept apart so it can be reported.
    pub iowait: u64,
}

/// Parse a `cpu…` line from /proc/stat (either the aggregate `cpu`
/// or a per-core `cpuN`). Columns: user nice system idle iowait irq
/// softirq steal guest guest_nice — later kernels may append more;
/// anything past steal that isn't guest is counted as busy.
pub fn parse_cpu_line(line: &str) -> Option<CpuTicks> {
    let mut fields = line.split_ascii_whitespace();
    let label = fields.next()?;
    if !label.starts_with("cpu") {
        return None;
    }
    let values: Vec<u64> = fields.filter_map(|f| f.parse().ok()).collect();
    if values.len() < 5 {
        return None;
    }
    let idle = values[3] + values[4]; // idle + iowait
    // guest/guest_nice (indices 8, 9) are already inside user/nice.
    let counted = values.iter().take(8).sum::<u64>();
    Some(CpuTicks {
        total: counted,
        idle,
        iowait: values[4],
    })
}

/// Everything we lift from one reading of /proc/stat.
#[derive(Clone, Debug, Default)]
pub struct ProcStat {
    pub aggregate: CpuTicks,
    pub per_core: Vec<CpuTicks>,
    pub context_switches: u64,
    pub boot_ts: f64,
}

pub fn parse_proc_stat(content: &str) -> ProcStat {
    let mut stat = ProcStat::default();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("cpu") {
            if rest.starts_with(' ') {
                if let Some(ticks) = parse_cpu_line(line) {
                    stat.aggregate = ticks;
                }
            } else if let Some(ticks) = parse_cpu_line(line) {
                stat.per_core.push(ticks);
            }
        } else if let Some(rest) = line.strip_prefix("ctxt ") {
            stat.context_switches = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("btime ") {
            stat.boot_ts = rest.trim().parse().unwrap_or(0.0);
        }
    }
    stat
}

fn busy_percent(previous: CpuTicks, current: CpuTicks) -> f32 {
    let total_delta = current.total.saturating_sub(previous.total);
    if total_delta == 0 {
        return 0.0;
    }
    let idle_delta = current.idle.saturating_sub(previous.idle);
    let busy = total_delta.saturating_sub(idle_delta) as f64 / total_delta as f64;
    (busy * 100.0).clamp(0.0, 100.0) as f32
}

fn iowait_percent(previous: CpuTicks, current: CpuTicks) -> f32 {
    let total_delta = current.total.saturating_sub(previous.total);
    if total_delta == 0 {
        return 0.0;
    }
    let iowait_delta = current.iowait.saturating_sub(previous.iowait);
    (iowait_delta as f64 / total_delta as f64 * 100.0).clamp(0.0, 100.0) as f32
}

/// The clock the working cores ran at: each core's current frequency
/// weighted by how busy it was over the window. A parked core keeps
/// reporting its last requested clock to cpufreq, so the plain mean
/// across 32 threads mostly measures idle cores; this answers "how
/// fast is the work running" (turbostat's Bzy_MHz). None when nothing
/// was busy enough to weigh (< 0.5% of one core in total).
pub fn busy_weighted_mhz(per_core_percent: &[f32], per_core_khz: &[Option<u64>]) -> Option<f64> {
    let mut weight = 0.0f64;
    let mut sum = 0.0f64;
    for (busy, khz) in per_core_percent.iter().zip(per_core_khz) {
        if let Some(khz) = khz {
            let w = f64::from(*busy);
            weight += w;
            sum += w * (*khz as f64 / 1000.0);
        }
    }
    (weight >= 0.5).then(|| sum / weight)
}

#[derive(Clone, Debug, Default)]
pub struct LoadAvg {
    pub load_1m: f64,
    pub load_5m: f64,
    pub load_15m: f64,
    pub tasks_running: u32,
    pub tasks_total: u32,
}

/// "2.11 1.63 1.37 1/538 1726488"
pub fn parse_loadavg(content: &str) -> LoadAvg {
    let mut load = LoadAvg::default();
    let mut fields = content.split_ascii_whitespace();
    load.load_1m = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0.0);
    load.load_5m = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0.0);
    load.load_15m = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0.0);
    if let Some(ratio) = fields.next()
        && let Some((running, total)) = ratio.split_once('/')
    {
        load.tasks_running = running.parse().unwrap_or(0);
        load.tasks_total = total.parse().unwrap_or(0);
    }
    load
}

pub struct CpuCollector {
    window: SelfInterval,
    previous: Option<ProcStat>,
    /// scaling_cur_freq per core, discovered once.
    cpufreq_paths: Vec<String>,
    /// Hardware clock limits (cpuinfo_min/max_freq): static, read once.
    frequency_range_mhz: (Option<f64>, Option<f64>),
}

impl Default for CpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuCollector {
    pub fn new() -> Self {
        let mut cpufreq_paths = Vec::new();
        let mut frequency_range_mhz = (None, None);
        if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
            let mut cores: Vec<String> = entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let digits = name.strip_prefix("cpu")?;
                    digits.chars().all(|c| c.is_ascii_digit()).then_some(name)
                })
                .collect();
            cores.sort_by_key(|name| name[3..].parse::<u32>().unwrap_or(u32::MAX));
            for core in &cores {
                cpufreq_paths.push(format!("/sys/devices/system/cpu/{core}/cpufreq/scaling_cur_freq"));
            }
            if let Some(first) = cores.first() {
                let mhz = |file: &str| {
                    read_u64(format!("/sys/devices/system/cpu/{first}/cpufreq/{file}"))
                        .map(|khz| khz as f64 / 1000.0)
                };
                frequency_range_mhz = (mhz("cpuinfo_min_freq"), mhz("cpuinfo_max_freq"));
            }
        }
        CpuCollector {
            window: SelfInterval::default(),
            previous: None,
            cpufreq_paths,
            frequency_range_mhz,
        }
    }

    /// Boot time (btime) — steady, from /proc/stat; needed by the
    /// process collector for start timestamps.
    pub fn boot_ts(&mut self) -> f64 {
        if let Some(previous) = &self.previous
            && previous.boot_ts > 0.0
        {
            return previous.boot_ts;
        }
        fs::read_to_string("/proc/stat")
            .map(|content| parse_proc_stat(&content).boot_ts)
            .unwrap_or(0.0)
    }

    pub fn collect(&mut self, now: std::time::Instant) -> CpuSnapshot {
        let interval_seconds = self.window.tick(now);
        let mut snapshot = CpuSnapshot::default();

        let current = fs::read_to_string("/proc/stat")
            .map(|content| parse_proc_stat(&content))
            .unwrap_or_default();

        if let Some(previous) = &self.previous {
            snapshot.per_core_percent = current
                .per_core
                .iter()
                .zip(previous.per_core.iter())
                .map(|(cur, prev)| busy_percent(*prev, *cur))
                .collect();
            snapshot.overall_percent = busy_percent(previous.aggregate, current.aggregate);
            snapshot.iowait_percent = iowait_percent(previous.aggregate, current.aggregate);
            if interval_seconds > 0.0 {
                snapshot.context_switches_per_second = current
                    .context_switches
                    .saturating_sub(previous.context_switches)
                    as f64
                    / interval_seconds;
            }
        } else {
            snapshot.per_core_percent = vec![0.0; current.per_core.len()];
        }
        snapshot.core_count = current.per_core.len();
        self.previous = Some(current);

        let load = read_trimmed("/proc/loadavg")
            .map(|content| parse_loadavg(&content))
            .unwrap_or_default();
        snapshot.load_1m = load.load_1m;
        snapshot.load_5m = load.load_5m;
        snapshot.load_15m = load.load_15m;
        snapshot.tasks_running = load.tasks_running;
        snapshot.tasks_total = load.tasks_total;

        let per_core_khz: Vec<Option<u64>> =
            self.cpufreq_paths.iter().map(read_u64).collect();
        let known: Vec<u64> = per_core_khz.iter().flatten().copied().collect();
        if !known.is_empty() {
            let mean_khz = known.iter().sum::<u64>() as f64 / known.len() as f64;
            snapshot.frequency_mhz = Some(mean_khz / 1000.0);
        }
        snapshot.frequency_busy_mhz = busy_weighted_mhz(&snapshot.per_core_percent, &per_core_khz);
        (snapshot.frequency_min_mhz, snapshot.frequency_max_mhz) = self.frequency_range_mhz;

        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT_FIXTURE: &str = "\
cpu  100 20 50 800 30 5 5 0 0 0
cpu0 50 10 25 400 15 2 3 0 0 0
cpu1 50 10 25 400 15 3 2 0 0 0
ctxt 123456789
btime 1751800000
processes 54321
procs_running 3
procs_blocked 0
";

    #[test]
    fn proc_stat_parses_aggregate_cores_ctxt_btime() {
        let stat = parse_proc_stat(STAT_FIXTURE);
        assert_eq!(stat.per_core.len(), 2);
        assert_eq!(stat.aggregate.total, 100 + 20 + 50 + 800 + 30 + 5 + 5);
        assert_eq!(stat.aggregate.idle, 830);
        assert_eq!(stat.context_switches, 123_456_789);
        assert_eq!(stat.boot_ts, 1_751_800_000.0);
    }

    #[test]
    fn busy_percent_is_delta_based() {
        let previous = CpuTicks {
            total: 1000,
            idle: 800,
            iowait: 100,
        };
        let current = CpuTicks {
            total: 2000,
            idle: 1400,
            iowait: 250,
        };
        // 1000 new ticks, 600 idle → 40% busy; 150 of the idle ticks
        // were iowait → 15% iowait (counted idle, reported apart).
        assert_eq!(busy_percent(previous, current), 40.0);
        assert_eq!(iowait_percent(previous, current), 15.0);
    }

    #[test]
    fn busy_clock_ignores_parked_cores() {
        // One core working at 4.6 GHz, three parked at 0.6 GHz: the
        // plain mean says 1.6 GHz, the work runs at 4.6.
        let busy = [100.0, 0.0, 0.0, 0.0];
        let khz = [Some(4_600_000), Some(600_000), Some(600_000), Some(600_000)];
        assert_eq!(busy_weighted_mhz(&busy, &khz), Some(4600.0));
        // Nothing busy → no claim.
        assert_eq!(busy_weighted_mhz(&[0.0; 4], &khz), None);
    }

    #[test]
    fn loadavg_parses_ratio_field() {
        let load = parse_loadavg("2.11 1.63 1.37 1/538 1726488");
        assert_eq!(load.load_1m, 2.11);
        assert_eq!(load.tasks_running, 1);
        assert_eq!(load.tasks_total, 538);
    }
}
