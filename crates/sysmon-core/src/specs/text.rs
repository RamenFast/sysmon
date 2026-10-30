// SPDX-License-Identifier: GPL-3.0-or-later
//! The shareable spec sheet: [`SystemSpecs`] as plain text for a
//! forum post, a chat or a bug report.
//!
//! What it leaves out, on purpose: the hostname, serial numbers,
//! addresses, and private mount paths (a drive shows what it holds —
//! system, home, EFI, data — not where you mounted it). What it keeps:
//! model names, part numbers and PCI ids, which identify a *kind* of
//! machine, not *this* one. Live readings (load, temperatures, usage)
//! are not specs and never appear here.
//!
//! Sizes: memory in both GB and GiB (the 128 GiB kit reads 137.4 GB
//! and people paste both kinds of number); drives in decimal, like the
//! sticker on the drive.

use crate::units::{Units, format_size};

use super::*;

/// The full sheet.
pub fn specs_text(specs: &SystemSpecs) -> String {
    let mut sheet = Sheet::default();
    sheet.line(format!("SysMon {} · system specs", crate::VERSION));
    for section in SPEC_SECTIONS {
        let mut part = Sheet::default();
        section_rows(&mut part, section, specs);
        if !part.0.is_empty() {
            sheet.blank();
            sheet.0.extend(part.0);
        }
    }
    if !specs.unavailable.is_empty() {
        sheet.blank();
        for missing in &specs.unavailable {
            sheet.row("Missing", format!("{}: {}", missing.section, missing.reason));
        }
    }
    sheet.text()
}

/// One section's rows (a pop-out's copy button). An unknown section
/// is the full sheet.
pub fn section_text(section: &str, specs: &SystemSpecs) -> String {
    if !SPEC_SECTIONS.contains(&section) {
        return specs_text(specs);
    }
    let mut sheet = Sheet::default();
    section_rows(&mut sheet, section, specs);
    if let Some(missing) = specs.unavailable.iter().find(|m| m.section == section) {
        sheet.row("Missing", format!("{}: {}", missing.section, missing.reason));
    }
    sheet.text()
}

fn section_rows(sheet: &mut Sheet, section: &str, specs: &SystemSpecs) {
    match section {
        "system" => system_rows(sheet, specs),
        "cpu" => cpu_rows(sheet, specs),
        "memory" => memory_rows(sheet, specs),
        "graphics" => graphics_rows(sheet, specs),
        "storage" => storage_rows(sheet, specs),
        "network" => network_rows(sheet, specs),
        "audio" => audio_rows(sheet, specs),
        _ => {}
    }
}

/// Rows of "Label     value"; a continuation row has an empty label.
#[derive(Default)]
struct Sheet(Vec<String>);

impl Sheet {
    fn row(&mut self, label: &str, value: String) {
        self.0.push(format!("{label:<9} {value}").trim_end().to_string());
    }
    fn more(&mut self, value: String) {
        self.row("", value);
    }
    fn line(&mut self, text: String) {
        self.0.push(text);
    }
    fn blank(&mut self) {
        self.0.push(String::new());
    }
    fn text(self) -> String {
        let mut text = self.0.join("\n");
        text.push('\n');
        text
    }
}

fn join(parts: impl IntoIterator<Item = Option<String>>) -> String {
    parts.into_iter().flatten().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// "137.4 GB (128.0 GiB)".
fn both(bytes: u64) -> String {
    format!("{} ({})", format_size(bytes, Units::Decimal), format_size(bytes, Units::Binary))
}

fn ghz(mhz: f64) -> String {
    format!("{:.2} GHz", mhz / 1000.0)
}

/// Cache sizes are binary by nature; whole MiB read cleanest.
fn cache(bytes: u64) -> String {
    if bytes >= 1 << 20 && bytes.is_multiple_of(1 << 20) {
        format!("{} MiB", bytes >> 20)
    } else {
        format!("{} KiB", bytes >> 10)
    }
}

fn hz(refresh: f32) -> String {
    if (refresh - refresh.round()).abs() < 0.005 {
        format!("{:.0} Hz", refresh)
    } else {
        format!("{refresh:.2} Hz")
    }
}

// ------------------------------------------------------------------ system

fn system_rows(sheet: &mut Sheet, specs: &SystemSpecs) {
    let Some(system) = &specs.system else { return };
    if let Some(os) = &system.os {
        let base = system.os_base.as_ref().map(|base| format!(" ({base} base)")).unwrap_or_default();
        sheet.row("OS", format!("{os}{base}"));
    }
    if !system.kernel.is_empty() {
        sheet.row("Kernel", join([Some(system.kernel.clone()), Some(system.architecture.clone())]));
    }
    let session = system.session_type.as_deref().map(|session| match session {
        "wayland" => "Wayland".to_string(),
        "x11" => "X11".to_string(),
        other => other.to_string(),
    });
    if system.desktop.is_some() || session.is_some() {
        sheet.row("Desktop", join([system.desktop.clone(), session]));
    }
    if let Some(model) = &system.model {
        sheet.row("Model", model.clone());
    }
    if let Some(board) = &system.board {
        sheet.row("Board", board.clone());
    }
    let secure_boot = system.secure_boot.map(|on| format!("Secure Boot {}", if on { "on" } else { "off" }));
    let firmware = system.firmware.as_ref().map(|firmware| match &system.firmware_date {
        Some(date) => format!("{firmware} ({date})"),
        None => firmware.clone(),
    });
    sheet.row("Firmware", join([firmware, Some(system.boot_mode.clone()), secure_boot]));
}

// --------------------------------------------------------------------- cpu

fn cpu_rows(sheet: &mut Sheet, specs: &SystemSpecs) {
    let Some(cpu) = &specs.cpu else { return };
    let mut first = true;
    let mut row = |sheet: &mut Sheet, value: String| {
        if value.is_empty() {
            return;
        }
        sheet.row(if first { "CPU" } else { "" }, value);
        first = false;
    };
    if let Some(model) = &cpu.model {
        row(sheet, model.clone());
    }
    let topology = match (cpu.sockets, cpu.cores) {
        (Some(sockets), Some(cores)) if sockets > 1 => {
            Some(format!("{sockets} sockets · {cores} cores / {} threads", cpu.threads))
        }
        (_, Some(cores)) => Some(format!("{cores} cores / {} threads", cpu.threads)),
        _ => Some(format!("{} threads", cpu.threads)),
    };
    let clocks = match (cpu.base_mhz, cpu.max_mhz) {
        (Some(base), Some(max)) if max > base + 50.0 => Some(format!("{} base · up to {}", ghz(base), ghz(max))),
        (_, Some(max)) => Some(format!("up to {}", ghz(max))),
        (Some(base), None) => Some(format!("{} base", ghz(base))),
        _ => None,
    };
    let boost = match cpu.boost_enabled {
        Some(false) => Some("boost off".to_string()),
        _ => None,
    };
    row(sheet, join([topology, clocks, boost]));
    row(
        sheet,
        join([
            cpu.cache_l2_bytes.map(|b| format!("L2 {}", cache(b))),
            cpu.cache_l3_bytes.map(|b| format!("L3 {}", cache(b))),
        ]),
    );
    row(sheet, cpu.features.join(" · "));
    let governor = cpu.governor.as_ref().map(|g| match &cpu.energy_preference {
        Some(epp) if epp != g => format!("{g} governor ({epp} EPP)"),
        _ => format!("{g} governor"),
    });
    row(sheet, join([cpu.frequency_driver.clone(), governor]));
}

// ------------------------------------------------------------------ memory

fn memory_rows(sheet: &mut Sheet, specs: &SystemSpecs) {
    let Some(memory) = &specs.memory else { return };
    let mut first = true;
    // Identical sticks are one group; a mixed kit lists each kind.
    let mut groups: Vec<(&crate::snapshot::MemoryModule, usize)> = Vec::new();
    for module in &memory.modules {
        let same = |other: &crate::snapshot::MemoryModule| {
            other.size_bytes == module.size_bytes
                && other.kind == module.kind
                && other.configured_speed_mts == module.configured_speed_mts
                && other.part_number == module.part_number
                && other.manufacturer == module.manufacturer
                && other.rank == module.rank
        };
        match groups.iter_mut().find(|(other, _)| same(other)) {
            Some((_, count)) => *count += 1,
            None => groups.push((module, 1)),
        }
    }
    for (module, count) in &groups {
        // Sticks are sold in binary sizes under decimal names: a
        // "32 GB" DIMM is 32 GiB, so the line says GiB (the box's "GB"
        // beside this sheet's decimal GB would be two units, one name).
        let mut kind = format!("{count} × {} GiB {}", module.size_bytes >> 30, module.kind);
        if let Some(speed) = module.configured_speed_mts {
            kind += &format!("-{speed}");
        }
        let rated = module
            .rated_speed_mts
            .filter(|rated| module.configured_speed_mts.is_some_and(|configured| rated > &configured))
            .map(|rated| format!("rated {rated} MT/s"));
        let part = match (&module.manufacturer, &module.part_number) {
            (Some(maker), Some(part)) => Some(format!("{maker} {part}")),
            (None, Some(part)) => Some(part.clone()),
            (Some(maker), None) => Some(maker.clone()),
            (None, None) => None,
        };
        let rank = module.rank.map(|rank| match rank {
            1 => "single-rank".to_string(),
            2 => "dual-rank".to_string(),
            n => format!("{n} ranks"),
        });
        sheet.row(if first { "Memory" } else { "" }, join([Some(kind), part, rank, rated]));
        first = false;
    }
    let slots = memory.slots.map(|slots| format!("{} of {slots} slots used", memory.modules.len()));
    if let Some(installed) = memory.installed_bytes {
        sheet.row(if first { "Memory" } else { "" }, join([Some(format!("installed {}", both(installed))), slots]));
        first = false;
    }
    let swap = if memory.swap_bytes == 0 { "no swap".to_string() } else { format!("swap {}", both(memory.swap_bytes)) };
    sheet.row(if first { "Memory" } else { "" }, format!("usable {} · {swap}", both(memory.usable_bytes)));
}

// ---------------------------------------------------------------- graphics

fn graphics_rows(sheet: &mut Sheet, specs: &SystemSpecs) {
    for gpu in &specs.gpus {
        let vram = gpu.vram_bytes.map(|bytes| format!("{} VRAM", both(bytes)));
        sheet.row("GPU", join([Some(gpu.name.clone()), vram]));
        let architecture = match (&gpu.family, &gpu.architecture) {
            (Some(family), Some(isa)) => Some(format!("{family} ({isa})")),
            (None, Some(isa)) => Some(isa.clone()),
            (Some(family), None) => Some(family.clone()),
            (None, None) => None,
        };
        let link = gpu.pcie_link.as_ref().map(|link| {
            let mut text = format!("PCIe {}.0 x{}", link.generation, link.width);
            let card = match (link.device_max_generation, link.device_max_width) {
                (Some(generation), Some(width)) => Some(format!("{generation}.0 x{width}")),
                (Some(generation), None) => Some(format!("{generation}.0")),
                (None, Some(width)) => Some(format!("x{width}")),
                (None, None) => None,
            };
            if let Some(card) = card {
                text += &format!(" (card supports {card})");
            }
            text
        });
        let power = gpu.power_cap_watts.map(|cap| match gpu.power_cap_max_watts {
            Some(max) if max > cap + 0.5 => format!("{cap:.0} W cap (max {max:.0} W)"),
            _ => format!("{cap:.0} W cap"),
        });
        sheet.more(join([
            architecture,
            gpu.compute_units.map(|cu| format!("{cu} CUs")),
            gpu.cuda_cores.map(|cores| format!("{cores} CUDA cores")),
            link,
            power,
        ]));
        sheet.more(join([
            gpu.driver.as_ref().map(|driver| format!("{driver} driver")),
            Some(format!("PCI {}", gpu.pci_id)),
        ]));
        if let Some(vulkan) = &gpu.vulkan {
            sheet.more(format!("Vulkan {} · {}", vulkan.version, vulkan.driver));
        }
        if let Some(gl) = &gpu.opengl {
            sheet.more(format!("OpenGL {} · {}", gl.version, gl.driver));
        }
        if let Some(stack) = &gpu.compute_stack {
            sheet.more(stack.clone());
        }
    }
    for display in &specs.displays {
        // "HP Inc." + "HP X32" reads as "HP X32": the model already
        // carries the brand.
        let brand = |make: &str| make.split_whitespace().next().unwrap_or("").to_string();
        let name = match (&display.make, &display.model) {
            (Some(make), Some(model)) if model.starts_with(&brand(make)) => Some(model.clone()),
            (Some(make), Some(model)) => Some(format!("{make} {model}")),
            (None, Some(model)) => Some(model.clone()),
            (Some(make), None) => Some(make.clone()),
            (None, None) => Some(display.connector.clone()),
        };
        let size = display.diagonal_inches.map(|inches| format!("{inches:.1}\""));
        let mode = match (display.width_px, display.height_px) {
            (Some(width), Some(height)) => {
                let mut mode = format!("{width}×{height}");
                if let Some(refresh) = display.refresh_hz {
                    mode += &format!(" @ {}", hz(refresh));
                }
                if display.mode_source == "preferred" {
                    mode += " (panel's preferred mode)";
                }
                Some(mode)
            }
            _ => None,
        };
        let scale = display.scale.filter(|&s| (s - 1.0).abs() > 0.01).map(|s| format!("scale {s}"));
        sheet.row("Display", join([name, size, mode, scale]));
    }
}

// ----------------------------------------------------------------- storage

/// Roles in the order a reader looks for them.
const ROLE_ORDER: [&str; 5] = ["system", "home", "boot", "EFI", "data"];

/// What a mount holds, without saying where it lives.
fn role(mount_point: &str) -> &'static str {
    match mount_point {
        "/" => "system",
        "/home" => "home",
        "/boot/efi" | "/efi" => "EFI",
        "/boot" => "boot",
        _ => "data",
    }
}

fn storage_rows(sheet: &mut Sheet, specs: &SystemSpecs) {
    let mut first = true;
    for drive in &specs.drives {
        let rpm = drive.rpm.map(|rpm| format!("{rpm} rpm"));
        let removable = drive.removable.then(|| "removable".to_string());
        sheet.row(
            if first { "Storage" } else { "" },
            join([
                drive.model.clone(),
                Some(drive.kind.clone()),
                Some(format_size(drive.size_bytes, Units::Decimal)),
                rpm,
                drive.link.clone(),
                removable,
            ]),
        );
        first = false;
        let filesystems: Vec<String> = drive
            .filesystems
            .iter()
            .map(|filesystem| {
                let kind = filesystem.fs_type.clone().unwrap_or_else(|| "unknown".to_string());
                if filesystem.mount_points.is_empty() {
                    return format!("{kind} (not mounted)");
                }
                let held: Vec<&str> = filesystem.mount_points.iter().map(|m| role(m)).collect();
                let roles: Vec<&str> =
                    ROLE_ORDER.iter().copied().filter(|role| held.contains(role)).collect();
                format!("{kind} ({})", roles.join(", "))
            })
            .collect();
        if !filesystems.is_empty() {
            sheet.more(format!("  {}", filesystems.join(" · ")));
        }
    }
}

// ----------------------------------------------------------------- network

fn network_rows(sheet: &mut Sheet, specs: &SystemSpecs) {
    for nic in &specs.network {
        let model = match (&nic.model, &nic.driver) {
            (Some(model), Some(driver)) => Some(format!("{model} ({driver})")),
            (Some(model), None) => Some(model.clone()),
            (None, Some(driver)) => Some(driver.clone()),
            (None, None) => Some(nic.name.clone()),
        };
        let link = if nic.link_up {
            nic.speed_mbps.map(|mbps| {
                let speed = if mbps >= 1000 && mbps % 1000 == 0 {
                    format!("{} Gb/s", mbps / 1000)
                } else if mbps >= 1000 {
                    format!("{:.1} Gb/s", mbps as f64 / 1000.0)
                } else {
                    format!("{mbps} Mb/s")
                };
                match &nic.duplex {
                    Some(duplex) => format!("{speed} {duplex} duplex"),
                    None => speed,
                }
            })
        } else {
            Some("link down".to_string())
        };
        sheet.row(&nic.kind, join([model, link]));
    }
}

// ------------------------------------------------------------------- audio

fn audio_rows(sheet: &mut Sheet, specs: &SystemSpecs) {
    let Some(audio) = &specs.audio else { return };
    let mut first = true;
    if let Some(server) = &audio.server {
        sheet.row("Audio", server.clone());
        first = false;
    }
    if !audio.devices.is_empty() {
        sheet.row(if first { "Audio" } else { "" }, audio.devices.join(" · "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::MemoryModule;

    /// The machine this was written on, as `collect()` read it.
    pub(crate) fn fixture() -> SystemSpecs {
        let stick = || MemoryModule {
            locator: "DIMM 0".to_string(),
            size_bytes: 32 << 30,
            kind: "DDR4".to_string(),
            configured_speed_mts: Some(3200),
            rated_speed_mts: Some(3200),
            part_number: Some("CMK128GX4M4E3200C16".to_string()),
            form_factor: Some("DIMM".to_string()),
            rank: Some(2),
            manufacturer: Some("Corsair".to_string()),
        };
        SystemSpecs {
            ts: 1.79e9,
            system: Some(SystemSpec {
                os: Some("Linux Mint 22.3".to_string()),
                os_base: Some("Ubuntu 24.04".to_string()),
                kernel: "7.2.9-cachyos-nexus".to_string(),
                architecture: "x86_64".to_string(),
                hostname: "interserve-linux".to_string(),
                session_type: Some("wayland".to_string()),
                desktop: Some("SwayFX 0.6-c4c8d51b (Sway 1.12.0)".to_string()),
                model: None,
                board: Some("ASRock X570 Phantom Gaming 4".to_string()),
                firmware: Some("American Megatrends Inc. P5.90".to_string()),
                firmware_date: Some("2026-09-07".to_string()),
                boot_mode: "UEFI".to_string(),
                secure_boot: Some(false),
            }),
            cpu: Some(CpuSpec {
                model: Some("AMD Ryzen 9 5950X 16-Core Processor".to_string()),
                sockets: Some(1),
                cores: Some(16),
                threads: 32,
                base_mhz: Some(3401.0),
                max_mhz: Some(5086.181),
                boost_enabled: Some(true),
                cache_l1d_bytes: Some(512 << 10),
                cache_l1i_bytes: Some(512 << 10),
                cache_l2_bytes: Some(8 << 20),
                cache_l3_bytes: Some(64 << 20),
                frequency_driver: Some("amd-pstate-epp".to_string()),
                governor: Some("performance".to_string()),
                energy_preference: Some("performance".to_string()),
                features: vec!["AVX2".into(), "FMA3".into(), "AES-NI".into(), "SHA".into(), "AMD-V".into()],
            }),
            memory: Some(MemorySpec {
                modules: vec![stick(), stick(), stick(), stick()],
                slots: Some(4),
                installed_bytes: Some(128 << 30),
                usable_bytes: 134_973_063_168,
                swap_bytes: 0,
            }),
            gpus: vec![GpuSpec {
                name: "AMD Radeon AI PRO R9700".to_string(),
                pci_id: "1002:7551".to_string(),
                pci_address: "0000:0a:00.0".to_string(),
                driver: Some("amdgpu".to_string()),
                vram_bytes: Some(34_208_743_424),
                architecture: Some("gfx1201".to_string()),
                family: Some("RDNA 4".to_string()),
                compute_units: Some(64),
                cuda_cores: None,
                pcie_link: Some(PcieLink {
                    generation: 4,
                    width: 16,
                    device_max_generation: Some(5),
                    device_max_width: None,
                }),
                power_cap_watts: Some(300.0),
                power_cap_max_watts: Some(330.0),
                vulkan: Some(ApiDriver {
                    version: "1.4.318".to_string(),
                    driver: "Mesa 25.2.8-0ubuntu0.24.04.2 (RADV)".to_string(),
                }),
                opengl: Some(ApiDriver {
                    version: "4.6".to_string(),
                    driver: "Mesa 25.2.8-0ubuntu0.24.04.2 (radeonsi)".to_string(),
                }),
                compute_stack: Some("ROCm 7.14.1".to_string()),
            }, GpuSpec {
                // The compute-only card beside it: no DRM node, no
                // Vulkan ICD, everything else from NVML (3.7).
                name: "NVIDIA GeForce RTX 3070".to_string(),
                pci_id: "10de:2484".to_string(),
                pci_address: "0000:03:00.0".to_string(),
                driver: Some("nvidia".to_string()),
                vram_bytes: Some(8_589_934_592),
                architecture: Some("sm_86".to_string()),
                family: Some("Ampere".to_string()),
                cuda_cores: Some(5888),
                pcie_link: Some(PcieLink { generation: 4, width: 4, device_max_generation: None, device_max_width: Some(16) }),
                power_cap_watts: Some(220.0),
                power_cap_max_watts: Some(240.0),
                compute_stack: Some("CUDA 13.4 · driver 615.71.09".to_string()),
                ..Default::default()
            }],
            displays: vec![DisplaySpec {
                connector: "DP-1".to_string(),
                make: Some("HP Inc.".to_string()),
                model: Some("HP X32".to_string()),
                diagonal_inches: Some(31.6),
                width_px: Some(2560),
                height_px: Some(1440),
                refresh_hz: Some(164.834),
                scale: Some(1.0),
                mode_source: "active".to_string(),
            }],
            drives: vec![
                DriveSpec {
                    name: "nvme0n1".to_string(),
                    model: Some("CT2000P3PSSD8".to_string()),
                    kind: "NVMe SSD".to_string(),
                    size_bytes: 2_000_398_934_016,
                    rpm: None,
                    link: Some("PCIe 4.0 x4".to_string()),
                    removable: false,
                    filesystems: vec![
                        FilesystemSpec {
                            device: "nvme0n1p1".to_string(),
                            fs_type: Some("vfat".to_string()),
                            mount_points: vec!["/boot/efi".to_string()],
                        },
                        FilesystemSpec {
                            device: "nvme0n1p2".to_string(),
                            fs_type: Some("btrfs".to_string()),
                            mount_points: vec!["/".to_string(), "/srv/fast".to_string(), "/home".to_string()],
                        },
                    ],
                },
                DriveSpec {
                    name: "sdc".to_string(),
                    model: Some("KINGSTON SA400S37240G".to_string()),
                    kind: "SATA SSD".to_string(),
                    size_bytes: 240_057_409_536,
                    rpm: None,
                    link: Some("SATA 6.0 Gb/s".to_string()),
                    removable: false,
                    filesystems: vec![FilesystemSpec {
                        device: "sdc2".to_string(),
                        fs_type: Some("xfs".to_string()),
                        mount_points: vec![],
                    }],
                },
                DriveSpec {
                    name: "sdb".to_string(),
                    model: Some("Hitachi HUA723030ALA640".to_string()),
                    kind: "SATA HDD".to_string(),
                    size_bytes: 3_000_592_982_016,
                    rpm: Some(7200),
                    link: Some("SATA 6.0 Gb/s".to_string()),
                    removable: false,
                    filesystems: vec![FilesystemSpec {
                        device: "sdb1".to_string(),
                        fs_type: Some("xfs".to_string()),
                        mount_points: vec!["/media/ben/Mass storage".to_string()],
                    }],
                },
            ],
            network: vec![NicSpec {
                name: "enp3s0".to_string(),
                kind: "Ethernet".to_string(),
                model: Some("I211 Gigabit Network Connection".to_string()),
                driver: Some("igb".to_string()),
                link_up: true,
                speed_mbps: Some(1000),
                duplex: Some("full".to_string()),
            }],
            audio: Some(AudioSpec {
                server: Some("PipeWire 1.0.5 (PulseAudio 15.0.0 API)".to_string()),
                devices: vec!["HDA ATI HDMI".to_string(), "Samson Q2U Microphone".to_string()],
            }),
            unavailable: vec![],
        }
    }

    #[test]
    fn the_sheet_reads_like_a_spec_page() {
        let text = specs_text(&fixture());
        for expected in [
            "OS        Linux Mint 22.3 (Ubuntu 24.04 base)",
            "Kernel    7.2.9-cachyos-nexus · x86_64",
            "Desktop   SwayFX 0.6-c4c8d51b (Sway 1.12.0) · Wayland",
            "Board     ASRock X570 Phantom Gaming 4",
            "Firmware  American Megatrends Inc. P5.90 (2026-09-07) · UEFI · Secure Boot off",
            "CPU       AMD Ryzen 9 5950X 16-Core Processor",
            "          16 cores / 32 threads · 3.40 GHz base · up to 5.09 GHz",
            "          L2 8 MiB · L3 64 MiB",
            "          AVX2 · FMA3 · AES-NI · SHA · AMD-V",
            "          amd-pstate-epp · performance governor",
            "Memory    4 × 32 GiB DDR4-3200 · Corsair CMK128GX4M4E3200C16 · dual-rank",
            "          installed 137.4 GB (128.0 GiB) · 4 of 4 slots used",
            "          usable 135.0 GB (125.7 GiB) · no swap",
            "GPU       AMD Radeon AI PRO R9700 · 34.2 GB (31.9 GiB) VRAM",
            "          RDNA 4 (gfx1201) · 64 CUs · PCIe 4.0 x16 (card supports 5.0) · 300 W cap (max 330 W)",
            "          amdgpu driver · PCI 1002:7551",
            "          Vulkan 1.4.318 · Mesa 25.2.8-0ubuntu0.24.04.2 (RADV)",
            "          OpenGL 4.6 · Mesa 25.2.8-0ubuntu0.24.04.2 (radeonsi)",
            "          ROCm 7.14.1",
            "GPU       NVIDIA GeForce RTX 3070 · 8.6 GB (8.0 GiB) VRAM",
            "          Ampere (sm_86) · 5888 CUDA cores · PCIe 4.0 x4 (card supports x16) · 220 W cap (max 240 W)",
            "          nvidia driver · PCI 10de:2484",
            "          CUDA 13.4 · driver 615.71.09",
            "Display   HP X32 · 31.6\" · 2560×1440 @ 164.83 Hz",
            "Storage   CT2000P3PSSD8 · NVMe SSD · 2.0 TB · PCIe 4.0 x4",
            "            vfat (EFI) · btrfs (system, home, data)",
            "          KINGSTON SA400S37240G · SATA SSD · 240.1 GB · SATA 6.0 Gb/s",
            "            xfs (not mounted)",
            "          Hitachi HUA723030ALA640 · SATA HDD · 3.0 TB · 7200 rpm · SATA 6.0 Gb/s",
            "Ethernet  I211 Gigabit Network Connection (igb) · 1 Gb/s full duplex",
            "Audio     PipeWire 1.0.5 (PulseAudio 15.0.0 API)",
            "          HDA ATI HDMI · Samson Q2U Microphone",
        ] {
            assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
        }
        assert!(text.starts_with(&format!("SysMon {} · system specs\n", crate::VERSION)));
        assert!(text.ends_with('\n') && !text.ends_with("\n\n"));
    }

    /// The paste goes to forums: nothing that names *this* machine or
    /// its owner rides along.
    #[test]
    fn the_sheet_never_carries_private_identifiers() {
        let text = specs_text(&fixture());
        for private in ["interserve-linux", "/media/ben", "Mass storage", "/srv/fast", "enp3s0", "0000:0a:00.0", "DIMM 0"] {
            assert!(!text.contains(private), "the sheet leaks `{private}`:\n{text}");
        }
    }

    #[test]
    fn sections_project_the_same_rows() {
        let specs = fixture();
        let gpu = section_text("graphics", &specs);
        assert!(gpu.starts_with("GPU       AMD Radeon AI PRO R9700"), "{gpu}");
        assert!(gpu.contains("Display   HP X32") && !gpu.contains("Storage"), "{gpu}");
        let memory = section_text("memory", &specs);
        assert!(memory.starts_with("Memory    4 × 32 GiB DDR4-3200"), "{memory}");
        assert_eq!(section_text("nonsuch", &specs), specs_text(&specs));
        for section in SPEC_SECTIONS {
            assert!(specs_text(&specs).contains(section_text(section, &specs).trim_end()), "{section}");
        }
    }

    #[test]
    fn mixed_sticks_are_listed_apart_and_a_slow_kit_says_so() {
        let mut specs = fixture();
        let memory = specs.memory.as_mut().unwrap();
        memory.modules[0].configured_speed_mts = Some(2133);
        memory.modules[1].configured_speed_mts = Some(2133);
        memory.modules[2].configured_speed_mts = Some(2133);
        memory.modules[3].size_bytes = 16 << 30;
        memory.modules[3].configured_speed_mts = Some(2133);
        let text = section_text("memory", &specs);
        assert!(text.contains("3 × 32 GiB DDR4-2133 · Corsair CMK128GX4M4E3200C16 · dual-rank · rated 3200 MT/s"), "{text}");
        assert!(text.contains("1 × 16 GiB DDR4-2133"), "{text}");
    }

    /// Unknown is absent, never zero; a missing section says so.
    #[test]
    fn a_thin_machine_gets_a_short_honest_sheet() {
        let specs = SystemSpecs {
            unavailable: vec![Unavailable { section: "storage".into(), reason: "no block devices".into() }],
            ..Default::default()
        };
        let text = specs_text(&specs);
        assert_eq!(
            text,
            format!("SysMon {} · system specs\n\nMissing   storage: no block devices\n", crate::VERSION)
        );
        assert_eq!(section_text("storage", &specs), "Missing   storage: no block devices\n");
    }

    #[test]
    fn edid_only_displays_say_the_mode_is_the_panels_preference() {
        let mut specs = fixture();
        specs.displays[0].mode_source = "preferred".to_string();
        specs.displays[0].refresh_hz = Some(60.0);
        let text = section_text("graphics", &specs);
        assert!(text.contains("2560×1440 @ 60 Hz (panel's preferred mode)"), "{text}");
    }
}
