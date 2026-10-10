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

use sysmon_core::snapshot::{InterfaceKind, SystemSnapshot};
use sysmon_core::units::{TemperatureScale, Units, format_size, format_temperature_in};

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

/// Rows of "Label    value", one label column wide.
#[derive(Default)]
struct Sheet(Vec<String>);

impl Sheet {
    fn row(&mut self, label: &str, value: String) {
        self.0.push(format!("{label:<8} {value}"));
    }

    fn text(self) -> String {
        let mut text = self.0.join("\n");
        text.push('\n');
        text
    }
}

/// The spec sheet. Lines whose source is missing are omitted.
pub fn specs_text(snapshot: &SystemSnapshot, host: &HostFacts) -> String {
    let mut sheet = Sheet::default();
    if let Some(system) = &snapshot.system {
        sheet.row("Host", system.hostname.clone());
    }
    if let Some(os) = &host.os {
        sheet.row("OS", os.clone());
    }
    if let Some(system) = &snapshot.system {
        sheet.row("Kernel", system.kernel.clone());
    }
    cpu_rows(&mut sheet, snapshot, host);
    memory_rows(&mut sheet, snapshot);
    gpu_rows(&mut sheet, snapshot);
    if let Some(board) = &host.board {
        sheet.row("Board", board.clone());
    }
    disk_rows(&mut sheet, snapshot);
    sheet.text()
}

/// One pop-out's worth: the same rows, scoped to that section
/// (`SECTION_KEYS`), so a popped GPU window copies the GPU.
pub fn section_text(section: &str, snapshot: &SystemSnapshot, host: &HostFacts) -> String {
    let mut sheet = Sheet::default();
    match section {
        "cpu" => cpu_rows(&mut sheet, snapshot, host),
        "memory" => memory_rows(&mut sheet, snapshot),
        "gpu" => gpu_rows(&mut sheet, snapshot),
        "disks" => disk_rows(&mut sheet, snapshot),
        "network" => network_rows(&mut sheet, snapshot),
        "sensors" => sensor_rows(&mut sheet, snapshot),
        _ => return specs_text(snapshot, host),
    }
    sheet.text()
}

fn cpu_rows(sheet: &mut Sheet, snapshot: &SystemSnapshot, host: &HostFacts) {
    let threads = snapshot.cpu.as_ref().map(|cpu| cpu.core_count).filter(|&n| n > 0);
    let max_clock = snapshot.cpu.as_ref().and_then(|cpu| cpu.frequency_max_mhz);
    if host.cpu_model.is_none() && threads.is_none() {
        return;
    }
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
    sheet.row("CPU", parts.join(" · "));
}

fn memory_rows(sheet: &mut Sheet, snapshot: &SystemSnapshot) {
    let Some(memory) = &snapshot.memory else {
        return;
    };
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
        sheet.row("RAM", sticks);
    }
    if let Some(installed) = memory.installed_bytes {
        sheet.row("", format!("installed {}", both(installed)));
    }
    sheet.row("", format!("usable    {}  (after firmware + kernel reservations)", both(memory.total_bytes)));
    let swap = if memory.swap_total_bytes == 0 { "none".to_string() } else { both(memory.swap_total_bytes) };
    sheet.row("Swap", swap);
}

fn gpu_rows(sheet: &mut Sheet, snapshot: &SystemSnapshot) {
    let Some(gpu) = snapshot.gpu.as_ref().filter(|gpu| gpu.available) else {
        return;
    };
    let mut value = gpu.device_name.clone();
    if gpu.vram_total_bytes > 0 {
        value += &format!(" · {} VRAM", both(gpu.vram_total_bytes));
    }
    sheet.row("GPU", value);
    if let Some(cap) = gpu.power_cap_watts {
        sheet.row("", format!("power cap {cap:.0} W"));
    }
}

fn disk_rows(sheet: &mut Sheet, snapshot: &SystemSnapshot) {
    let Some(disks) = &snapshot.disks else {
        return;
    };
    for (index, disk) in disks.iter().enumerate() {
        sheet.row(
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

/// Physical links only, and no addresses: a spec paste goes to forums,
/// and an IP has no business riding along with it.
fn network_rows(sheet: &mut Sheet, snapshot: &SystemSnapshot) {
    let Some(network) = &snapshot.network else {
        return;
    };
    let links = network.interfaces.iter().filter(|i| i.kind == InterfaceKind::Physical);
    for (index, link) in links.enumerate() {
        let state = if link.is_up { "up" } else { "down" };
        let speed = link.speed_mbps.map(|mbps| {
            if mbps >= 1000 && mbps % 1000 == 0 { format!(" · {} Gb/s", mbps / 1000) } else { format!(" · {mbps} Mb/s") }
        });
        sheet.row(if index == 0 { "Network" } else { "" }, format!("{} {state}{}", link.name, speed.unwrap_or_default()));
    }
}

/// Each chip by what it watches, with its plausible readings now.
fn sensor_rows(sheet: &mut Sheet, snapshot: &SystemSnapshot) {
    let Some(sensors) = &snapshot.sensors else {
        return;
    };
    for chip in &sensors.chips {
        let readings: Vec<String> = chip
            .temps
            .iter()
            .filter(|t| t.plausible)
            .map(|t| format!("{} {}", t.label, format_temperature_in(t.celsius, TemperatureScale::Both)))
            .chain(chip.fans.iter().map(|f| format!("{} {} rpm", f.label, f.rpm)))
            .collect();
        if readings.is_empty() {
            continue;
        }
        let name = match (&chip.device_model, &chip.device) {
            (Some(model), _) => format!("{} ({model})", chip.name),
            (None, Some(device)) => format!("{} ({device})", chip.name),
            _ => chip.name.clone(),
        };
        sheet.row("Sensor", format!("{name}: {}", readings.join(", ")));
    }
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
    fn a_popped_section_copies_only_its_own_rows() {
        let host = HostFacts { cpu_model: Some("AMD Ryzen 9 5950X 16-Core Processor".to_string()), ..Default::default() };
        let gpu = section_text("gpu", &snapshot(), &host);
        assert_eq!(gpu, "GPU      Radeon AI PRO R9700 · 34.2 GB (31.9 GiB) VRAM\n");
        let memory = section_text("memory", &snapshot(), &host);
        assert!(memory.starts_with("RAM      4 × 32 GB DDR4"), "{memory}");
        assert!(memory.contains("usable    135.0 GB (125.7 GiB)") && !memory.contains("GPU"), "{memory}");
        assert!(section_text("cpu", &snapshot(), &host).starts_with("CPU      AMD Ryzen 9 5950X"));
        assert!(section_text("disks", &snapshot(), &host).starts_with("Disks    / 2.0 TB"));
    }

    #[test]
    fn network_copy_names_physical_links_and_never_addresses() {
        use sysmon_core::snapshot::{InterfaceSnapshot, NetworkSnapshot};
        let link = |name: &str, kind| InterfaceSnapshot {
            name: name.to_string(),
            kind,
            is_up: true,
            ipv4: vec!["192.168.1.20".to_string()],
            speed_mbps: Some(1000),
            ..Default::default()
        };
        let snapshot = SystemSnapshot {
            network: Some(NetworkSnapshot {
                interfaces: vec![link("lo", InterfaceKind::Loopback), link("enp3s0", InterfaceKind::Physical)],
                ..Default::default()
            }),
            ..Default::default()
        };
        let text = section_text("network", &snapshot, &HostFacts::default());
        assert_eq!(text, "Network  enp3s0 up · 1 Gb/s\n");
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
