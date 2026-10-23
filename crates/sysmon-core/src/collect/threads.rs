// SPDX-License-Identifier: GPL-3.0-or-later
//! One process, thread by thread: what each thread is doing and which
//! CPU it is on. "st is at 125%" becomes "two threads, mostly on CPU 7
//! and CPU 19" (Ben, 3.8: "view what cores that is on specifically").
//!
//! Read on demand for one pid (the Inspector's, a details window's,
//! `sysmon probe threads --pid N`): /proc/<pid>/task/*/stat is one
//! small read per thread, cheap for one process and not something
//! the every-process sample should pay for.
//!
//! Where the time went: a thread's CPU time over the window is put on
//! the CPU it was last seen on (stat field 39, `processor`). A thread
//! that moved during the window counts on the CPU it ended on, the
//! same reading `ps -o psr` and htop's PROCESSOR column give. The
//! kernel keeps no per-CPU split of a thread's time, so this is the
//! honest resolution, and it is disclosed as `cpu_attribution`.

use std::collections::HashMap;
use std::fs;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::process::state_word;
use super::read::SelfInterval;

/// How per-CPU shares are attributed (on the wire, so a reader knows
/// what the split means without reading this file).
pub const CPU_ATTRIBUTION: &str = "last_cpu";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadRecord {
    pub tid: i32,
    /// The thread's own name (`comm`, set by the program: "gdbus",
    /// "Renderer", a worker pool's "tokio-rt-worker").
    pub name: String,
    pub state: String,
    pub state_word: String,
    /// Share of ONE CPU over the window (100 = a whole CPU).
    pub cpu_percent: f32,
    /// The logical CPU it last ran on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    pub cpu_time_seconds: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessThreads {
    pub pid: i32,
    pub name: String,
    pub interval_seconds: f64,
    /// Logical CPUs on the machine (the length of `per_cpu_percent`).
    pub cpu_count: usize,
    /// The threads' sum: matches the process's CPU % (100 = one CPU).
    pub cpu_percent: f32,
    /// This process's share of each logical CPU, by `cpu_attribution`.
    pub per_cpu_percent: Vec<f32>,
    pub cpu_attribution: String,
    /// The CPUs it may run on (`Cpus_allowed_list`), when that is not
    /// every CPU: a pinned or `taskset` process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_cpus: Option<String>,
    /// Busiest first, then by tid.
    pub threads: Vec<ThreadRecord>,
}

/// The parts of a task's stat line this view needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TaskStat {
    pub state: char,
    /// utime + stime, clock ticks.
    pub cpu_ticks: u64,
    pub starttime_ticks: u64,
    pub processor: Option<u32>,
}

/// `/proc/<pid>/task/<tid>/stat` → (comm, fields). The comm can hold
/// spaces and parens, so fields are counted after the LAST `)`.
pub fn parse_task_stat(content: &str) -> Option<(String, TaskStat)> {
    let open = content.find('(')?;
    let close = content.rfind(')')?;
    let comm = content.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = content[close + 1..].split_ascii_whitespace().collect();
    // rest[0] = state (field 3); field N lives at rest[N - 3].
    if rest.len() < 20 {
        return None;
    }
    let int = |index: usize| -> u64 { rest.get(index).and_then(|v| v.parse().ok()).unwrap_or(0) };
    Some((
        comm,
        TaskStat {
            state: rest[0].chars().next().unwrap_or('?'),
            cpu_ticks: int(11) + int(12),
            starttime_ticks: int(19),
            processor: rest.get(36).and_then(|v| v.parse().ok()),
        },
    ))
}

/// "0-3,8,10-11" → [0, 1, 2, 3, 8, 10, 11]. Garbage parts are skipped.
pub fn parse_cpu_list(list: &str) -> Vec<u32> {
    let mut cpus = Vec::new();
    for part in list.trim().split(',').filter(|part| !part.is_empty()) {
        match part.split_once('-') {
            Some((low, high)) => {
                if let (Ok(low), Ok(high)) = (low.trim().parse::<u32>(), high.trim().parse::<u32>()) {
                    cpus.extend(low..=high.min(low.saturating_add(4096)));
                }
            }
            None => cpus.extend(part.trim().parse::<u32>().ok()),
        }
    }
    cpus
}

/// Seconds since boot, as ticks (the clock stat's starttime counts in).
fn uptime_ticks(clk_tck: f64) -> Option<u64> {
    let uptime = fs::read_to_string("/proc/uptime").ok()?;
    let seconds: f64 = uptime.split_ascii_whitespace().next()?.parse().ok()?;
    Some((seconds * clk_tck) as u64)
}

/// A thread's CPU ticks over the window: the delta since the last read,
/// or all of them when it was born inside the window (a compiler's
/// worker, a short job: without this they'd never be counted). A
/// recycled tid is a new thread (its starttime differs).
fn window_ticks(previous: Option<(u64, u64)>, stat: &TaskStat, window_start_ticks: Option<u64>) -> Option<u64> {
    match previous {
        Some((ticks, started)) if started == stat.starttime_ticks => Some(stat.cpu_ticks.saturating_sub(ticks)),
        _ => window_start_ticks.filter(|start| stat.starttime_ticks >= *start).map(|_| stat.cpu_ticks),
    }
}

/// Logical CPUs the kernel can schedule on (configured, so an offline
/// CPU keeps its slot and indices stay stable).
pub fn cpu_count() -> usize {
    (unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) }).max(1) as usize
}

/// Per-thread rates for one process. Owns its window (the collector
/// law): two views sampling different pids each keep their own.
pub struct ThreadSampler {
    window: SelfInterval,
    pid: Option<i32>,
    /// tid → (ticks, the thread's starttime: a recycled tid is a new
    /// thread and must not inherit a delta).
    previous: HashMap<i32, (u64, u64)>,
    /// Uptime ticks at the last read: threads started after it were
    /// born inside the window.
    window_start: Option<u64>,
    clk_tck: f64,
    cpu_count: usize,
}

impl Default for ThreadSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadSampler {
    pub fn new() -> Self {
        ThreadSampler {
            window: SelfInterval::default(),
            pid: None,
            previous: HashMap::new(),
            window_start: None,
            clk_tck: (unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64).max(1.0),
            cpu_count: cpu_count(),
        }
    }

    /// Read every thread of `pid`. None when the process is gone (or
    /// its task list is unreadable). The first read of a pid has zero
    /// rates: a rate needs two reads.
    pub fn sample(&mut self, pid: i32) -> Option<ProcessThreads> {
        if self.pid != Some(pid) {
            self.pid = Some(pid);
            self.previous.clear();
            self.window = SelfInterval::default();
        }
        let tasks = fs::read_dir(format!("/proc/{pid}/task")).ok()?;
        let interval_seconds = self.window.tick(Instant::now());
        let window_start = self.window_start.filter(|_| interval_seconds > 0.0);
        self.window_start = uptime_ticks(self.clk_tck);
        let mut threads = Vec::new();
        let mut seen = HashMap::new();
        let mut per_cpu = vec![0.0f32; self.cpu_count];
        for entry in tasks.flatten() {
            let Some(tid) = entry.file_name().to_str().and_then(|name| name.parse::<i32>().ok()) else {
                continue;
            };
            let Ok(content) = fs::read_to_string(entry.path().join("stat")) else {
                continue; // the thread ended between the listing and the read
            };
            let Some((name, stat)) = parse_task_stat(&content) else {
                continue;
            };
            let cpu_percent = match window_ticks(self.previous.get(&tid).copied(), &stat, window_start) {
                Some(ticks) if interval_seconds > 0.0 => (ticks as f64 / self.clk_tck / interval_seconds * 100.0) as f32,
                _ => 0.0,
            };
            seen.insert(tid, (stat.cpu_ticks, stat.starttime_ticks));
            if let Some(cpu) = stat.processor {
                let index = cpu as usize;
                if index >= per_cpu.len() {
                    per_cpu.resize(index + 1, 0.0);
                }
                per_cpu[index] += cpu_percent;
            }
            threads.push(ThreadRecord {
                tid,
                name,
                state: stat.state.to_string(),
                state_word: state_word(stat.state).to_string(),
                cpu_percent,
                cpu: stat.processor,
                cpu_time_seconds: stat.cpu_ticks as f64 / self.clk_tck,
            });
        }
        self.previous = seen;
        if threads.is_empty() {
            return None;
        }
        threads.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent).then(a.tid.cmp(&b.tid)));
        let name = fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|comm| comm.trim().to_string())
            .unwrap_or_default();
        let allowed_cpus = fs::read_to_string(format!("/proc/{pid}/status")).ok().and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
                .map(|list| list.trim().to_string())
        });
        // Only worth saying when it narrows something.
        let allowed_cpus = allowed_cpus.filter(|list| parse_cpu_list(list).len() < per_cpu.len());
        Some(ProcessThreads {
            pid,
            name,
            interval_seconds,
            cpu_count: per_cpu.len(),
            cpu_percent: threads.iter().map(|thread| thread.cpu_percent).sum(),
            per_cpu_percent: per_cpu,
            cpu_attribution: CPU_ATTRIBUTION.to_string(),
            allowed_cpus,
            threads,
        })
    }
}

/// One process's time on one CPU (the reverse view: who is on CPU 7).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CoreOccupant {
    pub pid: i32,
    pub name: String,
    /// Share of that one CPU over the window.
    pub cpu_percent: f32,
}

/// How many processes each CPU names.
pub const OCCUPANTS_PER_CPU: usize = 3;

/// Thread samples (pid, cpu, percent) → each CPU's busiest processes,
/// threads of one process summed, quiet ones dropped.
pub fn top_per_cpu(samples: &[(i32, u32, f32)], names: &HashMap<i32, String>, cpu_count: usize, per_cpu: usize) -> Vec<Vec<CoreOccupant>> {
    let mut sums: HashMap<(u32, i32), f32> = HashMap::new();
    for &(pid, cpu, percent) in samples {
        *sums.entry((cpu, pid)).or_default() += percent;
    }
    let mut top = vec![Vec::new(); cpu_count];
    for ((cpu, pid), percent) in sums {
        if percent < 0.5 {
            continue;
        }
        let index = cpu as usize;
        if index >= top.len() {
            top.resize(index + 1, Vec::new());
        }
        top[index].push(CoreOccupant { pid, name: names.get(&pid).cloned().unwrap_or_default(), cpu_percent: percent });
    }
    for occupants in &mut top {
        occupants.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent).then(a.pid.cmp(&b.pid)));
        occupants.truncate(per_cpu);
    }
    top
}

/// Every thread on the machine, grouped by the CPU it is on: what the
/// CPU card's per-CPU bars name on hover. A walk of /proc/*/task/*/stat
/// (~3000 small reads, a few ms), so it runs only when asked
/// (`Wants::cpu_occupants`), never in the default sample. Same
/// last-CPU attribution as `ThreadSampler`.
pub struct OccupancySampler {
    window: SelfInterval,
    previous: HashMap<i32, (u64, u64)>,
    window_start: Option<u64>,
    clk_tck: f64,
    cpu_count: usize,
}

impl Default for OccupancySampler {
    fn default() -> Self {
        Self::new()
    }
}

impl OccupancySampler {
    pub fn new() -> Self {
        OccupancySampler {
            window: SelfInterval::default(),
            previous: HashMap::new(),
            window_start: None,
            clk_tck: (unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64).max(1.0),
            cpu_count: cpu_count(),
        }
    }

    /// Each CPU's busiest processes over the window since the last
    /// call (empty lists on the first).
    pub fn sample(&mut self) -> Vec<Vec<CoreOccupant>> {
        let interval_seconds = self.window.tick(Instant::now());
        let window_start = self.window_start.filter(|_| interval_seconds > 0.0);
        self.window_start = uptime_ticks(self.clk_tck);
        let mut seen = HashMap::with_capacity(self.previous.len().max(1024));
        let mut samples = Vec::new();
        let mut names = HashMap::new();
        let Ok(proc_dir) = fs::read_dir("/proc") else {
            return vec![Vec::new(); self.cpu_count];
        };
        for process in proc_dir.flatten() {
            let Some(pid) = process.file_name().to_str().and_then(|name| name.parse::<i32>().ok()) else {
                continue;
            };
            let Ok(tasks) = fs::read_dir(process.path().join("task")) else {
                continue;
            };
            for task in tasks.flatten() {
                let Some(tid) = task.file_name().to_str().and_then(|name| name.parse::<i32>().ok()) else {
                    continue;
                };
                let Ok(content) = fs::read_to_string(task.path().join("stat")) else {
                    continue;
                };
                let Some((comm, stat)) = parse_task_stat(&content) else {
                    continue;
                };
                if tid == pid {
                    names.insert(pid, comm);
                }
                if let (Some(ticks), Some(cpu)) = (window_ticks(self.previous.get(&tid).copied(), &stat, window_start), stat.processor)
                    && ticks > 0
                    && interval_seconds > 0.0
                {
                    let percent = (ticks as f64 / self.clk_tck / interval_seconds * 100.0) as f32;
                    samples.push((pid, cpu, percent));
                }
                seen.insert(tid, (stat.cpu_ticks, stat.starttime_ticks));
            }
        }
        self.previous = seen;
        top_per_cpu(&samples, &names, self.cpu_count, OCCUPANTS_PER_CPU)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupants_rank_processes_per_cpu() {
        // (pid, cpu, percent) per thread: two threads of pid 7 on CPU 3
        // add up; a quiet thread doesn't make the list.
        let samples = [(7, 3, 30.0), (7, 3, 20.0), (9, 3, 40.0), (9, 1, 10.0), (11, 3, 0.1), (12, 3, 5.0)];
        let names: HashMap<i32, String> = [(7, "st"), (9, "xz"), (11, "idle"), (12, "sway")]
            .into_iter()
            .map(|(pid, name)| (pid, name.to_string()))
            .collect();
        let top = top_per_cpu(&samples, &names, 4, 2);
        assert_eq!(top.len(), 4);
        assert!(top[0].is_empty() && top[2].is_empty());
        assert_eq!(top[1], vec![CoreOccupant { pid: 9, name: "xz".into(), cpu_percent: 10.0 }]);
        let on3: Vec<(i32, f32)> = top[3].iter().map(|o| (o.pid, o.cpu_percent)).collect();
        assert_eq!(on3, vec![(7, 50.0), (9, 40.0)], "summed per process, busiest first, capped at 2");
    }

    #[test]
    fn a_thread_born_inside_the_window_counts_all_its_time() {
        let stat = |ticks, started| TaskStat { state: 'R', cpu_ticks: ticks, starttime_ticks: started, processor: Some(2) };
        // Seen before: the delta.
        assert_eq!(window_ticks(Some((100, 50)), &stat(130, 50), Some(1000)), Some(30));
        // New, started after the window opened: everything it ran.
        assert_eq!(window_ticks(None, &stat(40, 1010), Some(1000)), Some(40));
        // A recycled tid, born in the window: its own ticks, not a delta.
        assert_eq!(window_ticks(Some((500, 50)), &stat(7, 1005), Some(1000)), Some(7));
        // New but older than the window (a first read): unknown.
        assert_eq!(window_ticks(None, &stat(40, 900), Some(1000)), None);
        assert_eq!(window_ticks(None, &stat(40, 1010), None), None);
    }

    #[test]
    fn task_stat_reads_ticks_and_the_last_cpu() {
        // A ghostty thread on this machine, 2026-10-23 (comm with a
        // space and parens, to prove the last-`)` rule).
        let line = "694601 (gdbus (x) 2) S 694574 694574 694574 0 -1 4194624 177 0 0 0 \
                    41 17 0 0 20 0 9 0 30188712 1268146176 26770 18446744073709551615 1 1 0 0 0 0 0 \
                    4096 0 0 0 0 -1 19 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let (comm, stat) = parse_task_stat(line).expect("parses");
        assert_eq!(comm, "gdbus (x) 2");
        assert_eq!(stat.state, 'S');
        assert_eq!(stat.cpu_ticks, 41 + 17);
        assert_eq!(stat.starttime_ticks, 30_188_712);
        assert_eq!(stat.processor, Some(19));
    }

    #[test]
    fn a_short_stat_line_is_refused_not_misread() {
        assert!(parse_task_stat("12 (x) S 1 2 3").is_none());
    }

    #[test]
    fn cpu_lists_expand() {
        assert_eq!(parse_cpu_list("0-3,8,10-11\n"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_cpu_list("5"), vec![5]);
        assert!(parse_cpu_list("").is_empty());
        assert_eq!(parse_cpu_list("x,2"), vec![2]);
    }

    #[test]
    fn own_threads_add_up_and_land_on_real_cpus() {
        let busy = std::thread::spawn(|| {
            let started = Instant::now();
            let mut x = 0u64;
            while started.elapsed().as_millis() < 600 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            }
            x
        });
        let pid = std::process::id() as i32;
        let mut sampler = ThreadSampler::new();
        let first = sampler.sample(pid).expect("own threads");
        assert!(first.threads.len() >= 2, "main + the busy thread");
        assert_eq!(first.cpu_percent, 0.0, "a first read has no rate");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let second = sampler.sample(pid).expect("own threads");
        busy.join().unwrap();
        assert!(second.cpu_percent > 30.0, "the spinning thread shows: {}", second.cpu_percent);
        let spread: f32 = second.per_cpu_percent.iter().sum();
        assert!((spread - second.cpu_percent).abs() < 0.01, "every percent lands on a CPU");
        assert!(second.threads.iter().all(|t| t.cpu.is_some_and(|cpu| (cpu as usize) < second.cpu_count)));
        assert!(second.threads[0].cpu_percent >= second.threads.last().unwrap().cpu_percent, "busiest first");
    }
}
