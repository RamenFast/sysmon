// SPDX-License-Identifier: GPL-3.0-or-later
//! "Copy specs": the machine as plain text, for pasting into a forum
//! post, a chat or a bug report. The top bar's copy button calls
//! [`specs_text`] with the latest snapshot plus the few static facts
//! the snapshot doesn't carry (CPU model, board, distro), read here
//! once per click rather than added to the wire contract.
//!
//! Sizes are printed in both unit systems on purpose: a spec sheet is
//! read by people who use either, and "128 GiB of sticks shows as
//! 137.4 GB" is exactly the confusion it should settle, not cause.

use std::fs;

use sysmon_core::snapshot::SystemSnapshot;
use sysmon_core::units::{Units, format_size};

/// Static host facts the snapshot doesn't carry. Each is `None` when
/// its source is unreadable; the line is then left out, never guessed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostFacts {
    pub cpu_model: Option<String>,
    pub board: Option<String>,
    pub os: Option<String>,
}

impl HostFacts {
    pub fn read() -> Self {
        HostFacts {
            cpu_model: fs::read_to_string("/proc/cpuinfo").ok().and_then(|c| cpu_model(&c)),
            board: fs::read_to_string("/run/udev/data/+dmi:id").ok().and_then(|c| board(&c)),
            os: fs::read_to_string("/etc/os-release").ok().and_then(|c| os_name(&c)),
        }
    }
}

fn cpu_model(cpuinfo: &str) -> Option<String> {
    cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name")?.split_once(':').map(|(_, v)| v.trim().to_string()))
        .filter(|model| !model.is_empty())
}

/// udev's DMI export names the board as ID_VENDOR + ID_MODEL.
fn board(dmi: &str) -> Option<String> {
    let field = |key: &str| {
        dmi.lines()
            .find_map(|line| line.strip_prefix("E:")?.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "Default string" && *value != "To Be Filled By O.E.M.")
    };
    match (field("ID_VENDOR"), field("ID_MODEL")) {
        (Some(vendor), Some(model)) => Some(format!("{vendor} {model}")),
        (None, Some(model)) => Some(model.to_string()),
        _ => None,
    }
}

fn os_name(os_release: &str) -> Option<String> {
    os_release
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|name| !name.is_empty())
}

/// "137.4 GB (128.0 GiB)".
fn both(bytes: u64) -> String {
    format!("{} ({})", format_size(bytes, Units::Decimal), format_size(bytes, Units::Binary))
}

/// The spec sheet. Lines whose source is missing are omitted.
pub fn specs_text(snapshot: &SystemSnapshot, host: &HostFacts) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut row = |label: &str, value: String| lines.push(format!("{label:<8} {value}"));

    if let Some(system) = &snapshot.system {
        row("Host", system.hostname.clone());
    }
    if let Some(os) = &host.os {
        row("OS", os.clone());
    }
    if let Some(system) = &snapshot.system {
        row("Kernel", system.kernel.clone());
    }

    let threads = snapshot.cpu.as_ref().map(|cpu| cpu.core_count).filter(|&n| n > 0);
    let max_clock = snapshot.cpu.as_ref().and_then(|cpu| cpu.frequency_max_mhz);
    if host.cpu_model.is_some() || threads.is_some() {
        let mut parts: Vec<String> = Vec::new();
        if let Some(model) = &host.cpu_model {
            parts.push(model.clone());
        }
        if let Some(threads) = threads {
            parts.push(format!("{threads} threads"));
        }
        if let Some(mhz) = max_clock {
            parts.push(format!("up to {:.2} GHz", mhz / 1000.0));
        }
        row("CPU", parts.join(" · "));
    }

    if let Some(memory) = &snapshot.memory {
        if let Some(first) = memory.modules.first() {
            let same = memory.modules.iter().all(|m| m.size_bytes == first.size_bytes && m.kind == first.kind);
            let mut sticks = if same {
                // Stick sizes are powers of two, sold in binary units.
                format!("{} × {} GB {}", memory.modules.len(), first.size_bytes >> 30, first.kind)
            } else {
                format!("{} sticks", memory.modules.len())
            };
            if let Some(speed) = first.configured_speed_mts {
                sticks += &format!(" @ {speed} MT/s");
            }
            if let Some(part) = &first.part_number {
                sticks += &format!(" ({part})");
            }
            row("RAM", sticks);
        }
        if let Some(installed) = memory.installed_bytes {
            row("", format!("installed {}", both(installed)));
        }
        row("", format!("usable    {}  (after firmware + kernel reservations)", both(memory.total_bytes)));
        let swap = if memory.swap_total_bytes == 0 { "none".to_string() } else { both(memory.swap_total_bytes) };
        row("Swap", swap);
    }

    if let Some(gpu) = snapshot.gpu.as_ref().filter(|gpu| gpu.available) {
        let mut value = gpu.device_name.clone();
        if gpu.vram_total_bytes > 0 {
            value += &format!(" · {} VRAM", both(gpu.vram_total_bytes));
        }
        row("GPU", value);
    }

    if let Some(board) = &host.board {
        row("Board", board.clone());
    }

    if let Some(disks) = &snapshot.disks {
        for (index, disk) in disks.iter().enumerate() {
            row(
                if index == 0 { "Disks" } else { "" },
                format!(
                    "{} {} ({}) {}",
                    disk.mount_point,
                    format_size(disk.total_bytes, Units::Decimal),
                    disk.fs_type,
                    disk.device
                ),
            );
        }
    }

    let mut text = lines.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use sysmon_core::snapshot::{
        CpuSnapshot, DiskSnapshot, GpuSnapshot, MemoryModule, MemorySnapshot, SystemInfo,
    };

    fn snapshot() -> SystemSnapshot {
        let module = |locator: &str| MemoryModule {
            locator: locator.to_string(),
            size_bytes: 32 << 30,
            kind: "DDR4".to_string(),
            configured_speed_mts: Some(3200),
            rated_speed_mts: Some(3200),
            part_number: Some("CMK128GX4M4E3200C16".to_string()),
        };
        SystemSnapshot {
            system: Some(SystemInfo {
                hostname: "box".to_string(),
                kernel: "7.2.9".to_string(),
                ..Default::default()
            }),
            cpu: Some(CpuSnapshot { core_count: 32, frequency_max_mhz: Some(5086.0), ..Default::default() }),
            memory: Some(MemorySnapshot {
                total_bytes: 134_973_063_168,
                installed_bytes: Some(128 << 30),
                modules: vec![module("DIMM 0"), module("DIMM 1"), module("DIMM 2"), module("DIMM 3")],
                ..Default::default()
            }),
            gpu: Some(GpuSnapshot {
                available: true,
                device_name: "Radeon AI PRO R9700".to_string(),
                vram_total_bytes: 34_208_743_424,
                ..Default::default()
            }),
            disks: Some(vec![DiskSnapshot {
                device: "/dev/nvme0n1p2".to_string(),
                mount_point: "/".to_string(),
                fs_type: "btrfs".to_string(),
                total_bytes: 1_999_324_123_136,
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn specs_name_the_sticks_and_both_ram_totals_in_both_units() {
        let host = HostFacts {
            cpu_model: Some("AMD Ryzen 9 5950X 16-Core Processor".to_string()),
            board: Some("ASRock X570 Phantom Gaming 4".to_string()),
            os: Some("Linux Mint 22.3".to_string()),
        };
        let text = specs_text(&snapshot(), &host);
        for expected in [
            "Host     box",
            "OS       Linux Mint 22.3",
            "CPU      AMD Ryzen 9 5950X 16-Core Processor · 32 threads · up to 5.09 GHz",
            "RAM      4 × 32 GB DDR4 @ 3200 MT/s (CMK128GX4M4E3200C16)",
            "installed 137.4 GB (128.0 GiB)",
            "usable    135.0 GB (125.7 GiB)",
            "Swap     none",
            "GPU      Radeon AI PRO R9700 · 34.2 GB (31.9 GiB) VRAM",
            "Board    ASRock X570 Phantom Gaming 4",
            "Disks    / 2.0 TB (btrfs) /dev/nvme0n1p2",
        ] {
            assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
        }
    }

    #[test]
    fn missing_sources_drop_their_lines_instead_of_guessing() {
        let text = specs_text(&SystemSnapshot::default(), &HostFacts::default());
        assert_eq!(text, "\n");
    }

    #[test]
    fn host_fact_parsers_read_the_real_file_shapes() {
        assert_eq!(
            cpu_model("processor\t: 0\nmodel name\t: AMD Ryzen 9 5950X 16-Core Processor\n").as_deref(),
            Some("AMD Ryzen 9 5950X 16-Core Processor")
        );
        assert_eq!(
            board("E:ID_SYS_VENDOR_IS_RUBBISH=1\nE:ID_MODEL=X570 Phantom Gaming 4\nE:ID_VENDOR=ASRock\n").as_deref(),
            Some("ASRock X570 Phantom Gaming 4")
        );
        assert_eq!(board("E:ID_VENDOR=To Be Filled By O.E.M.\n"), None);
        assert_eq!(
            os_name("NAME=\"Linux Mint\"\nPRETTY_NAME=\"Linux Mint 22.3\"\n").as_deref(),
            Some("Linux Mint 22.3")
        );
    }
}
