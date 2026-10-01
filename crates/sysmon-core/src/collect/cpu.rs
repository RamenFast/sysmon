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
    /// system + irq + softirq: time the kernel itself was working.
    /// Inside `total − idle`, never overlapping iowait or guest.
    pub kernel: u64,
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
        kernel: values[2] + values[5] + values[6],
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

/// The kernel's share of the window (system + irq + softirq), 0–100.
/// Always ≤ `busy_percent` for the same pair: kernel ticks are busy
/// ticks (C7).
fn kernel_percent(previous: CpuTicks, current: CpuTicks) -> f32 {
    let total_delta = current.total.saturating_sub(previous.total);
    if total_delta == 0 {
        return 0.0;
    }
    let kernel_delta = current.kernel.saturating_sub(previous.kernel);
    (kernel_delta as f64 / total_delta as f64 * 100.0).clamp(0.0, 100.0) as f32
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
    cppc: Option<CppcReader>,
}

/// The clock each core actually *delivered* across the window, from
/// ACPI CPPC feedback counters: (delivered − delivered₀)/(reference −
/// reference₀) × nominal_freq, the APERF/MPERF ratio turbostat reads,
/// counted only while the core runs (C1a).
///
/// Each `feedback_ctrs` read is a firmware mailbox round-trip
/// (≈0.7 ms wall, ≈0.3 ms CPU; 32 cores ≈ 21 ms wall, 10 ms CPU). The
/// sweep happens in the sample that uses it, right after /proc/stat,
/// so both edges of the window are this sample's edges (C1c: an
/// earlier background-thread version answered with the previous
/// window). The wait lands on the sampler's own thread, never a UI one.
struct CppcReader {
    nominal_mhz: f64,
    paths: Vec<String>,
    previous: Option<CppcSweep>,
}

/// One (reference, delivered) reading per core, None where unreadable.
type CppcSweep = Vec<Option<(u64, u64)>>;

impl CppcReader {
    fn new(cores: &[String]) -> Option<CppcReader> {
        let nominal_mhz = read_u64(format!("/sys/devices/system/cpu/{}/acpi_cppc/nominal_freq", cores.first()?))? as f64;
        let paths: Vec<String> =
            cores.iter().map(|core| format!("/sys/devices/system/cpu/{core}/acpi_cppc/feedback_ctrs")).collect();
        parse_feedback_ctrs(&fs::read_to_string(&paths[0]).ok()?)?;
        Some(CppcReader {
            nominal_mhz,
            paths,
            previous: None,
        })
    }

    /// Per-core delivered MHz since the last call (None per core when
    /// unreadable or wrapped); None on the first call, which only sets
    /// the window's opening edge.
    fn take(&mut self) -> Option<Vec<Option<f64>>> {
        let current: CppcSweep = self
            .paths
            .iter()
            .map(|path| fs::read_to_string(path).ok().as_deref().and_then(parse_feedback_ctrs))
            .collect();
        let window = self.previous.as_ref().map(|previous| delivered_mhz(self.nominal_mhz, previous, &current));
        self.previous = Some(current);
        window
    }
}

/// Per-core delivered MHz between two sweeps. A wrap or reset shows as
/// a counter going backwards: that core drops out of the window.
fn delivered_mhz(nominal_mhz: f64, before: &CppcSweep, after: &CppcSweep) -> Vec<Option<f64>> {
    before
        .iter()
        .zip(after)
        .map(|(before, after)| {
            let ((r0, d0), (r1, d1)) = ((*before)?, (*after)?);
            let (reference, delivered) = (r1.checked_sub(r0)?, d1.checked_sub(d0)?);
            (reference > 0).then(|| nominal_mhz * delivered as f64 / reference as f64)
        })
        .collect()
}

/// "cpuN" names in the same order as `cpufreq_paths` (which is the
/// /proc/stat order), so CPPC results line up with per-core busy %.
fn cores_for_cppc(cpufreq_paths: &[String]) -> Option<Vec<String>> {
    let cores: Vec<String> = cpufreq_paths
        .iter()
        .filter_map(|path| path.strip_prefix("/sys/devices/system/cpu/")?.split('/').next().map(str::to_string))
        .collect();
    (!cores.is_empty()).then_some(cores)
}

/// "ref:1691286126842 del:1944026549165" → (reference, delivered).
pub fn parse_feedback_ctrs(text: &str) -> Option<(u64, u64)> {
    let mut fields = text.split_whitespace();
    let reference = fields.next()?.strip_prefix("ref:")?.parse().ok()?;
    let delivered = fields.next()?.strip_prefix("del:")?.parse().ok()?;
    Some((reference, delivered))
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
        let cppc = cores_for_cppc(&cpufreq_paths).and_then(|cores| CppcReader::new(&cores));
        CpuCollector {
            window: SelfInterval::default(),
            previous: None,
            cpufreq_paths,
            frequency_range_mhz,
            cppc,
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
        // The CPPC edge right beside the /proc/stat edge: one window.
        let delivered = self.cppc.as_mut().and_then(CppcReader::take);

        if let Some(previous) = &self.previous {
            snapshot.per_core_percent = current
                .per_core
                .iter()
                .zip(previous.per_core.iter())
                .map(|(cur, prev)| busy_percent(*prev, *cur))
                .collect();
            snapshot.overall_percent = busy_percent(previous.aggregate, current.aggregate);
            snapshot.iowait_percent = iowait_percent(previous.aggregate, current.aggregate);
            snapshot.kernel_percent = kernel_percent(previous.aggregate, current.aggregate);
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
        // The busy clock: what the busy cores delivered over the window
        // (CPPC) when we have a window of it; else the instant read.
        let delivered_mhz = delivered.as_deref().and_then(|cores| {
            let as_khz: Vec<Option<u64>> = cores.iter().map(|mhz| mhz.map(|m| (m * 1000.0) as u64)).collect();
            busy_weighted_mhz(&snapshot.per_core_percent, &as_khz)
        });
        (snapshot.frequency_busy_mhz, snapshot.frequency_busy_source) = match delivered_mhz {
            Some(mhz) => (Some(mhz), Some("delivered over the window".to_string())),
            None => {
                let instant = busy_weighted_mhz(&snapshot.per_core_percent, &per_core_khz);
                (instant, instant.map(|_| "instant read".to_string()))
            }
        };
        (snapshot.frequency_min_mhz, snapshot.frequency_max_mhz) = self.frequency_range_mhz;

        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_ctrs_parse_and_refuse_junk() {
        assert_eq!(parse_feedback_ctrs("ref:1691286126842 del:1944026549165\n"), Some((1_691_286_126_842, 1_944_026_549_165)));
        assert_eq!(parse_feedback_ctrs("del:1 ref:2"), None, "field order is the ABI");
        assert_eq!(parse_feedback_ctrs("ref:x del:1"), None);
        assert_eq!(parse_feedback_ctrs(""), None);
    }

    #[test]
    fn a_wrapped_or_idle_core_drops_out_of_the_window() {
        // C1b: going backwards (wrap/reset) or a zero reference delta
        // must yield None for that core, never a huge or infinite MHz.
        let before = vec![Some((1000, 1000)), Some((1000, 5000)), Some((1000, 1000)), None];
        let after = vec![Some((2000, 2200)), Some((2000, 100)), Some((1000, 1500)), Some((5, 5))];
        let window = delivered_mhz(3400.0, &before, &after);
        assert_eq!(window[0], Some(3400.0 * 1.2));
        assert_eq!(window[1], None, "delivered went backwards");
        assert_eq!(window[2], None, "no reference ticks");
        assert_eq!(window[3], None, "no previous edge");
    }

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
            kernel: 50,
        };
        let current = CpuTicks {
            total: 2000,
            idle: 1400,
            iowait: 250,
            kernel: 150,
        };
        // 1000 new ticks, 600 idle → 40% busy; 150 of the idle ticks
        // were iowait → 15% iowait (counted idle, reported apart).
        assert_eq!(busy_percent(previous, current), 40.0);
        assert_eq!(iowait_percent(previous, current), 15.0);
        // 100 kernel ticks of the 1000 → 10%, inside the 40% busy.
        assert_eq!(kernel_percent(previous, current), 10.0);
    }

    /// C7: kernel time is system + irq + softirq only. Never iowait
    /// (that's idle), never guest (already inside user/nice), and so
    /// never above the busy share it is drawn inside of.
    #[test]
    fn kernel_time_is_system_irq_softirq_and_never_exceeds_busy() {
        //                user nice sys  idle  iow  irq soft steal guest gnice
        let a = parse_cpu_line("cpu  100  20   50   800   30   5   5    0     40    0").unwrap();
        let b = parse_cpu_line("cpu  300  20   250  1000  130  25  15   0     40    0").unwrap();
        // deltas: user 200, sys 200, idle 200, iowait 100, irq 20, softirq 10
        // total 730; kernel 230; busy = 730 − 300 idle = 430
        assert_eq!(a.kernel, 60);
        assert_eq!(b.kernel, 290);
        let kernel = kernel_percent(a, b);
        let busy = busy_percent(a, b);
        assert!((kernel - 230.0 / 730.0 * 100.0).abs() < 0.01, "{kernel}");
        assert!(kernel <= busy, "kernel {kernel} > busy {busy}");
        // A window that is all iowait has no kernel time at all.
        let c = parse_cpu_line("cpu  300  20   250  1000  630  25  15   0     40    0").unwrap();
        assert_eq!(kernel_percent(b, c), 0.0);
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
