// SPDX-License-Identifier: GPL-3.0-or-later
//! Memory: one pass over /proc/meminfo, plus the installed modules
//! from the firmware's SMBIOS tables (read once — sticks don't move
//! while the machine runs).
//!
//! Definitions match v1 (which matched psutil), so upgrade day
//! doesn't change the numbers Ben reads:
//!   used      = MemTotal − MemAvailable
//!   percent   = used / MemTotal
//!   cached    = Cached + SReclaimable  (reclaimable page cache)
//!   swap used = SwapTotal − SwapFree
//!
//! Three different "totals" exist and the card shows all of them, so
//! none masquerades as another:
//!   installed = Σ module sizes (SMBIOS type 17) — the number on the box
//!   usable    = MemTotal — installed minus firmware/kernel reservations
//!   used      = usable − available

use std::fs;

use crate::snapshot::{MemoryModule, MemorySnapshot};

/// udev's export of the DMI tables: world-readable, unlike
/// /sys/firmware/dmi/entries (root only) or dmidecode.
const UDEV_DMI_DATABASE: &str = "/run/udev/data/+dmi:id";

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

    // /proc/meminfo's "kB" is KiB.
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
        installed_bytes: None,
        modules: Vec::new(),
    }
}

/// Parse udev's DMI database (`E:MEMORY_DEVICE_<n>_<KEY>=<value>`
/// lines) into populated modules, slot order. Empty slots report
/// SIZE 0 or omit it and are skipped.
pub fn parse_dmi_memory(content: &str) -> Vec<MemoryModule> {
    let mut slots: std::collections::BTreeMap<u32, MemoryModule> = Default::default();
    for line in content.lines() {
        let Some(rest) = line
            .strip_prefix("E:MEMORY_DEVICE_")
            .or_else(|| line.strip_prefix("MEMORY_DEVICE_"))
        else {
            continue;
        };
        let Some((key, value)) = rest.split_once('=') else {
            continue;
        };
        let Some((index, field)) = key.split_once('_') else {
            continue;
        };
        let Ok(index) = index.parse::<u32>() else {
            continue;
        };
        let module = slots.entry(index).or_default();
        let mts = || value.parse::<u32>().ok().filter(|&speed| speed > 0);
        match field {
            "SIZE" => module.size_bytes = value.parse().unwrap_or(0),
            "LOCATOR" => module.locator = value.to_string(),
            "TYPE" => module.kind = value.to_string(),
            "SPEED_MTS" => module.rated_speed_mts = mts(),
            "CONFIGURED_SPEED_MTS" => module.configured_speed_mts = mts(),
            "PART_NUMBER" => {
                let part = value.trim();
                if !part.is_empty() && part != "Unknown" && part != "Not Specified" {
                    module.part_number = Some(part.to_string());
                }
            }
            _ => {}
        }
    }
    slots
        .into_values()
        .filter(|module| module.size_bytes > 0)
        .collect()
}

pub struct MemoryCollector {
    modules: Vec<MemoryModule>,
}

impl Default for MemoryCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryCollector {
    pub fn new() -> Self {
        MemoryCollector {
            modules: fs::read_to_string(UDEV_DMI_DATABASE)
                .map(|content| parse_dmi_memory(&content))
                .unwrap_or_default(),
        }
    }

    pub fn collect(&self) -> MemorySnapshot {
        let mut snapshot = fs::read_to_string("/proc/meminfo")
            .map(|content| parse_meminfo(&content))
            .unwrap_or_default();
        if !self.modules.is_empty() {
            snapshot.installed_bytes = Some(self.modules.iter().map(|m| m.size_bytes).sum());
            snapshot.modules = self.modules.clone();
        }
        snapshot
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

    /// The shape udev writes on this machine (X570, 4× DDR4 at the
    /// JEDEC 2133 default with the 3200 XMP profile not enabled), plus
    /// an empty slot that must not count.
    #[test]
    fn dmi_modules_sum_to_installed_and_keep_both_speeds() {
        let content = "\
E:MEMORY_ARRAY_MAX_CAPACITY=137438953472
E:MEMORY_DEVICE_0_SIZE=34359738368
E:MEMORY_DEVICE_0_LOCATOR=DIMM 0
E:MEMORY_DEVICE_0_TYPE=DDR4
E:MEMORY_DEVICE_0_SPEED_MTS=2133
E:MEMORY_DEVICE_0_CONFIGURED_SPEED_MTS=2133
E:MEMORY_DEVICE_0_PART_NUMBER=CMK128GX4M4E3200C16
E:MEMORY_DEVICE_1_SIZE=34359738368
E:MEMORY_DEVICE_1_LOCATOR=DIMM 1
E:MEMORY_DEVICE_1_TYPE=DDR4
E:MEMORY_DEVICE_1_CONFIGURED_SPEED_MTS=2133
E:MEMORY_DEVICE_2_SIZE=0
E:MEMORY_DEVICE_2_LOCATOR=DIMM 2
E:MEMORY_DEVICE_2_PART_NUMBER=Unknown
";
        let modules = parse_dmi_memory(content);
        assert_eq!(modules.len(), 2, "the empty slot is not a module");
        assert_eq!(modules.iter().map(|m| m.size_bytes).sum::<u64>(), 2 * 34_359_738_368);
        assert_eq!(modules[0].locator, "DIMM 0");
        assert_eq!(modules[0].kind, "DDR4");
        assert_eq!(modules[0].configured_speed_mts, Some(2133));
        assert_eq!(modules[0].rated_speed_mts, Some(2133));
        assert_eq!(modules[0].part_number.as_deref(), Some("CMK128GX4M4E3200C16"));
        assert_eq!(modules[1].rated_speed_mts, None);
    }
}
