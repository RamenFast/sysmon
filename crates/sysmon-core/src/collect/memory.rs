// SPDX-License-Identifier: GPL-3.0-or-later
//! Memory: one pass over /proc/meminfo.
//!
//! Definitions match v1 (which matched psutil), so upgrade day
//! doesn't change the numbers Ben reads:
//!   used      = MemTotal − MemAvailable
//!   percent   = used / MemTotal
//!   cached    = Cached + SReclaimable  (reclaimable page cache)
//!   swap used = SwapTotal − SwapFree

use std::fs;

use crate::snapshot::MemorySnapshot;

/// Parse /proc/meminfo (values are kB) into the snapshot.
pub fn parse_meminfo(content: &str) -> MemorySnapshot {
    let mut total = 0u64;
    let mut free = 0u64;
    let mut available = 0u64;
    let mut cached = 0u64;
    let mut s_reclaimable = 0u64;
    let mut buffers = 0u64;
    let mut dirty = 0u64;
    let mut shmem = 0u64;
    let mut swap_total = 0u64;
    let mut swap_free = 0u64;
    let mut swap_cached = 0u64;

    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let kilobytes: u64 = rest
            .trim()
            .split_ascii_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        match key {
            "MemTotal" => total = kilobytes,
            "MemFree" => free = kilobytes,
            "MemAvailable" => available = kilobytes,
            "Cached" => cached = kilobytes,
            "SReclaimable" => s_reclaimable = kilobytes,
            "Buffers" => buffers = kilobytes,
            "Dirty" => dirty = kilobytes,
            "Shmem" => shmem = kilobytes,
            "SwapTotal" => swap_total = kilobytes,
            "SwapFree" => swap_free = kilobytes,
            "SwapCached" => swap_cached = kilobytes,
            _ => {}
        }
    }

    let kb = |kilobytes: u64| kilobytes * 1024;
    let used = total.saturating_sub(available);
    MemorySnapshot {
        total_bytes: kb(total),
        used_bytes: kb(used),
        available_bytes: kb(available),
        free_bytes: kb(free),
        cached_bytes: kb(cached + s_reclaimable),
        buffers_bytes: kb(buffers),
        dirty_bytes: kb(dirty),
        shared_bytes: kb(shmem),
        used_percent: if total > 0 {
            (used as f64 / total as f64 * 100.0) as f32
        } else {
            0.0
        },
        swap_total_bytes: kb(swap_total),
        swap_used_bytes: kb(swap_total.saturating_sub(swap_free)),
        swap_cached_bytes: kb(swap_cached),
    }
}

pub struct MemoryCollector;

impl MemoryCollector {
    pub fn collect(&self) -> MemorySnapshot {
        fs::read_to_string("/proc/meminfo")
            .map(|content| parse_meminfo(&content))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO_FIXTURE: &str = "\
MemTotal:       32800000 kB
MemFree:         3400000 kB
MemAvailable:   24200000 kB
Buffers:          500000 kB
Cached:         19000000 kB
SwapCached:            0 kB
Dirty:              1234 kB
Shmem:             22000 kB
SReclaimable:    1200000 kB
SwapTotal:             0 kB
SwapFree:              0 kB
";

    #[test]
    fn meminfo_uses_v1_identities() {
        let memory = parse_meminfo(MEMINFO_FIXTURE);
        assert_eq!(memory.total_bytes, 32_800_000 * 1024);
        assert_eq!(memory.used_bytes, (32_800_000 - 24_200_000) * 1024);
        assert_eq!(memory.cached_bytes, (19_000_000 + 1_200_000) * 1024);
        assert_eq!(memory.dirty_bytes, 1234 * 1024);
        assert_eq!(memory.swap_total_bytes, 0);
        assert!((memory.used_percent - 26.219513).abs() < 0.01);
    }
}
