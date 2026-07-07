// SPDX-License-Identifier: GPL-3.0-or-later
//! Processes: one pass over /proc/<pid> — stat (state, ppid, nice,
//! threads, cpu ticks, start time, vsize, rss), cmdline, io, exe.
//! CPU% is tick-delta over the window; pid reuse is caught by keying
//! the delta state on (pid, starttime).
//!
//! /proc/<pid>/io and /proc/<pid>/exe are unreadable for other
//! users' processes — those fields go None rather than zero, so the
//! UI can honestly show "—" instead of a fake idle.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;

use crate::snapshot::ProcessRecord;

use super::read::SelfInterval;

/// The bits we lift from one /proc/<pid>/stat line.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatLine {
    pub state: char,
    pub ppid: i32,
    pub cpu_ticks: u64,
    pub nice: i32,
    pub threads: u32,
    pub starttime_ticks: u64,
    pub vsize_bytes: u64,
    pub rss_pages: i64,
}

/// comm may contain spaces and parens; everything after the LAST ')'
/// is fixed-position. Returns (comm, parsed fields).
pub fn parse_stat_line(content: &str) -> Option<(String, StatLine)> {
    let open = content.find('(')?;
    let close = content.rfind(')')?;
    let comm = content[open + 1..close].to_string();
    let rest: Vec<&str> = content[close + 1..].split_ascii_whitespace().collect();
    // rest[0] = state (field 3); field N lives at rest[N - 3].
    if rest.len() < 22 {
        return None;
    }
    let int = |index: usize| -> i64 { rest[index].parse().unwrap_or(0) };
    Some((
        comm,
        StatLine {
            state: rest[0].chars().next().unwrap_or('?'),
            ppid: int(1) as i32,
            cpu_ticks: (int(11) + int(12)) as u64, // utime + stime
            nice: int(16) as i32,
            threads: int(17) as u32,
            starttime_ticks: int(19) as u64,
            vsize_bytes: int(20) as u64,
            rss_pages: int(21),
        },
    ))
}

pub fn state_word(state: char) -> &'static str {
    match state {
        'R' => "running",
        'S' => "sleeping",
        'D' => "disk sleep",
        'Z' => "zombie",
        'T' => "stopped",
        't' => "tracing stop",
        'X' | 'x' => "dead",
        'I' => "idle",
        'P' => "parked",
        _ => "unknown",
    }
}

/// read_bytes / write_bytes from /proc/<pid>/io (storage I/O, not
/// rchar/wchar).
pub fn parse_io(content: &str) -> (u64, u64) {
    let mut read_bytes = 0;
    let mut write_bytes = 0;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("read_bytes: ") {
            read_bytes = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("write_bytes: ") {
            write_bytes = value.trim().parse().unwrap_or(0);
        }
    }
    (read_bytes, write_bytes)
}

#[derive(Clone, Copy)]
struct PreviousProcess {
    starttime_ticks: u64,
    cpu_ticks: u64,
    io_read_bytes: u64,
    io_write_bytes: u64,
}

pub struct ProcessCollector {
    window: SelfInterval,
    previous: HashMap<i32, PreviousProcess>,
    users: UserCache,
    clk_tck: f64,
    page_size: u64,
    read_buffer: String,
}

impl Default for ProcessCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessCollector {
    pub fn new() -> Self {
        ProcessCollector {
            window: SelfInterval::default(),
            previous: HashMap::new(),
            users: UserCache::new(),
            clk_tck: unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64,
            page_size: unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64,
            read_buffer: String::with_capacity(4096),
        }
    }

    /// Read a small /proc file into the reusable buffer.
    fn read_proc(&mut self, path: &str) -> Option<&str> {
        self.read_buffer.clear();
        let mut file = fs::File::open(path).ok()?;
        file.read_to_string(&mut self.read_buffer).ok()?;
        Some(self.read_buffer.as_str())
    }

    pub fn collect(&mut self, now: std::time::Instant, boot_ts: f64) -> Vec<ProcessRecord> {
        let interval_seconds = self.window.tick(now);
        self.users.refresh_if_stale();
        let core_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f32;

        let mut records = Vec::with_capacity(self.previous.len().max(128));
        let mut next_previous = HashMap::with_capacity(self.previous.len().max(128));

        let Ok(entries) = fs::read_dir("/proc") else {
            return records;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let pid: i32 = match name.parse() {
                Ok(pid) => pid,
                Err(_) => continue,
            };

            let stat_path = format!("/proc/{pid}/stat");
            let Some((comm, stat)) = self
                .read_proc(&stat_path)
                .and_then(parse_stat_line)
            else {
                continue; // exited mid-scan
            };

            let uid = entry.metadata().map(|m| m.uid()).unwrap_or(u32::MAX);
            let user = self.users.name_for(uid);

            let mut command_line = String::new();
            if let Some(raw) = self.read_proc(&format!("/proc/{pid}/cmdline")) {
                command_line = raw.replace('\0', " ").trim().to_string();
            }
            let is_kernel_thread = command_line.is_empty();

            let io = self
                .read_proc(&format!("/proc/{pid}/io"))
                .map(parse_io);

            let exe_basename = fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .and_then(|path| {
                    path.file_name()
                        .map(|n| n.to_string_lossy().trim_end_matches(" (deleted)").to_string())
                });

            // Deltas — only valid when this is the same process
            // instance we saw last tick.
            let mut cpu_percent = 0.0f32;
            let mut disk_read_bps = None;
            let mut disk_write_bps = None;
            if interval_seconds > 0.0
                && let Some(previous) = self.previous.get(&pid)
                && previous.starttime_ticks == stat.starttime_ticks
            {
                let tick_delta = stat.cpu_ticks.saturating_sub(previous.cpu_ticks) as f64;
                cpu_percent = ((tick_delta / self.clk_tck) / interval_seconds * 100.0) as f32;
                cpu_percent = cpu_percent.clamp(0.0, core_count * 100.0);
                if let Some((read_bytes, write_bytes)) = io {
                    disk_read_bps = Some(
                        read_bytes.saturating_sub(previous.io_read_bytes) as f64
                            / interval_seconds,
                    );
                    disk_write_bps = Some(
                        write_bytes.saturating_sub(previous.io_write_bytes) as f64
                            / interval_seconds,
                    );
                }
            } else if io.is_some() {
                disk_read_bps = Some(0.0);
                disk_write_bps = Some(0.0);
            }

            next_previous.insert(
                pid,
                PreviousProcess {
                    starttime_ticks: stat.starttime_ticks,
                    cpu_ticks: stat.cpu_ticks,
                    io_read_bytes: io.map(|(r, _)| r).unwrap_or(0),
                    io_write_bytes: io.map(|(_, w)| w).unwrap_or(0),
                },
            );

            records.push(ProcessRecord {
                pid,
                ppid: stat.ppid,
                name: comm,
                user,
                state: stat.state.to_string(),
                state_word: state_word(stat.state).to_string(),
                is_kernel_thread,
                cpu_percent,
                memory_rss_bytes: stat.rss_pages.max(0) as u64 * self.page_size,
                memory_virtual_bytes: stat.vsize_bytes,
                threads: stat.threads,
                nice: stat.nice,
                started_ts: boot_ts + stat.starttime_ticks as f64 / self.clk_tck,
                cpu_time_seconds: stat.cpu_ticks as f64 / self.clk_tck,
                disk_read_bps,
                disk_write_bps,
                gpu_busy_percent: 0.0, // merged in by the gpu collector
                gpu_vram_bytes: 0,
                net_rx_bps: None, // merged in by the net-process source
                net_tx_bps: None,
                command_line,
                exe_basename,
            });
        }

        self.previous = next_previous;
        records
    }
}

/// uid → username from /etc/passwd, reloaded when the file changes.
struct UserCache {
    names: HashMap<u32, String>,
    passwd_mtime: Option<std::time::SystemTime>,
}

impl UserCache {
    fn new() -> Self {
        let mut cache = UserCache {
            names: HashMap::new(),
            passwd_mtime: None,
        };
        cache.refresh_if_stale();
        cache
    }

    fn refresh_if_stale(&mut self) {
        let mtime = fs::metadata("/etc/passwd").and_then(|m| m.modified()).ok();
        if mtime == self.passwd_mtime && !self.names.is_empty() {
            return;
        }
        self.passwd_mtime = mtime;
        self.names.clear();
        if let Ok(content) = fs::read_to_string("/etc/passwd") {
            for line in content.lines() {
                let mut fields = line.split(':');
                let Some(name) = fields.next() else { continue };
                let _password = fields.next();
                if let Some(uid) = fields.next().and_then(|u| u.parse().ok()) {
                    self.names.insert(uid, name.to_string());
                }
            }
        }
    }

    fn name_for(&self, uid: u32) -> String {
        self.names
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_line_survives_hostile_comm() {
        // A comm with spaces, parens, and digits — the classic trap.
        let line = "1234 (Web Content (x) 2) S 1 1234 1234 0 -1 4194560 \
                    1000 0 5 0 700 300 0 0 20 5 17 0 98765 123456789 4321 \
                    18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";
        let (comm, stat) = parse_stat_line(line).expect("parses");
        assert_eq!(comm, "Web Content (x) 2");
        assert_eq!(stat.state, 'S');
        assert_eq!(stat.ppid, 1);
        assert_eq!(stat.cpu_ticks, 1000); // 700 utime + 300 stime
        assert_eq!(stat.nice, 5);
        assert_eq!(stat.threads, 17);
        assert_eq!(stat.starttime_ticks, 98765);
        assert_eq!(stat.vsize_bytes, 123_456_789);
        assert_eq!(stat.rss_pages, 4321);
    }

    #[test]
    fn io_parses_storage_bytes_not_rchar() {
        let content = "rchar: 999999\nwchar: 888888\nsyscr: 1\nsyscw: 2\n\
                       read_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n";
        assert_eq!(parse_io(content), (4096, 8192));
    }

    #[test]
    fn state_words_cover_the_zoo() {
        assert_eq!(state_word('R'), "running");
        assert_eq!(state_word('D'), "disk sleep");
        assert_eq!(state_word('I'), "idle");
        assert_eq!(state_word('?'), "unknown");
    }
}
