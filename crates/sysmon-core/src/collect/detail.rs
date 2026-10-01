// SPDX-License-Identifier: GPL-3.0-or-later
//! Deep detail for ONE process — what the Inspector shows and
//! `sysmon probe` never pays for: the memory split (RSS vs PSS vs
//! USS vs swap, from smaps_rollup), open file count, working
//! directory, cgroup, OOM score, context switches.
//!
//! Read on demand for the selected pid only; smaps_rollup walks the
//! process's page tables, which is cheap for one process and wasteful
//! for six hundred.

use std::fs;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessDetail {
    pub pid: i32,
    /// Resident: every page mapped in, shared pages counted in full.
    pub rss_bytes: u64,
    /// Proportional: shared pages split between their sharers — the
    /// honest "how much of the machine's RAM is this one".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pss_bytes: Option<u64>,
    /// Unique: private pages only — what quitting it would free.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uss_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_files: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oom_score: Option<i32>,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
}

/// smaps_rollup (kB lines) → (rss, pss, uss = private clean + dirty, swap).
pub fn parse_smaps_rollup(content: &str) -> (u64, u64, u64, u64) {
    let mut rss = 0;
    let mut pss = 0;
    let mut private = 0;
    let mut swap = 0;
    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let kilobytes: u64 = rest
            .split_ascii_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        match key {
            "Rss" => rss = kilobytes,
            "Pss" => pss = kilobytes,
            "Private_Clean" | "Private_Dirty" => private += kilobytes,
            "Swap" => swap = kilobytes,
            _ => {}
        }
    }
    (rss * 1024, pss * 1024, private * 1024, swap * 1024)
}

/// Read everything readable; fields another user's process hides
/// stay None rather than zero.
pub fn read_process_detail(pid: i32) -> Option<ProcessDetail> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut detail = ProcessDetail {
        pid,
        ..Default::default()
    };
    for line in status.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let number = || {
            rest.split_ascii_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        match key {
            "VmRSS" => detail.rss_bytes = number() * 1024,
            "voluntary_ctxt_switches" => detail.voluntary_switches = number(),
            "nonvoluntary_ctxt_switches" => detail.involuntary_switches = number(),
            _ => {}
        }
    }
    if let Ok(rollup) = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")) {
        let (rss, pss, uss, swap) = parse_smaps_rollup(&rollup);
        if rss > 0 {
            detail.rss_bytes = rss;
        }
        detail.pss_bytes = Some(pss);
        detail.uss_bytes = Some(uss);
        detail.swap_bytes = Some(swap);
    }
    detail.open_files = fs::read_dir(format!("/proc/{pid}/fd"))
        .ok()
        .map(|entries| entries.count() as u32);
    detail.cwd = fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    detail.exe_path = fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    detail.cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .and_then(|content| {
            content
                .lines()
                .find_map(|line| line.strip_prefix("0::").map(str::to_string))
        });
    detail.oom_score = fs::read_to_string(format!("/proc/{pid}/oom_score"))
        .ok()
        .and_then(|score| score.trim().parse().ok());
    Some(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_splits_rss_pss_uss() {
        // sway on this machine, 2026-09-30.
        let content = "63afb56a3000-7ffeaf29d000 ---p 00000000 00:00 0  [rollup]\n\
                       Rss:              160300 kB\nPss:               87855 kB\n\
                       Shared_Clean:      86084 kB\nShared_Dirty:          8 kB\n\
                       Private_Clean:      3524 kB\nPrivate_Dirty:     70684 kB\n\
                       Swap:                  0 kB\n";
        let (rss, pss, uss, swap) = parse_smaps_rollup(content);
        assert_eq!(rss, 160_300 * 1024);
        assert_eq!(pss, 87_855 * 1024);
        assert_eq!(uss, (3_524 + 70_684) * 1024);
        assert_eq!(swap, 0);
        assert!(uss <= pss && pss <= rss, "USS ≤ PSS ≤ RSS");
    }

    #[test]
    fn own_process_detail_is_readable_and_ordered() {
        let detail = read_process_detail(std::process::id() as i32).expect("own detail");
        let pss = detail.pss_bytes.expect("own smaps_rollup is readable");
        let uss = detail.uss_bytes.unwrap();
        assert!(uss <= pss && pss <= detail.rss_bytes);
        assert!(detail.open_files.unwrap() >= 3, "stdin/out/err at least");
        assert!(detail.cwd.is_some());
    }
}
