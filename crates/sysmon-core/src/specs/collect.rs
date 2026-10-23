// SPDX-License-Identifier: GPL-3.0-or-later
//! Reads the machine into [`SystemSpecs`]. sysfs and udev first; the
//! optional helpers (vulkaninfo, glxinfo, swaymsg, pactl) only for
//! what sysfs cannot say, each bounded by [`run::HELPER_DEADLINE`]
//! and run in parallel so the slowest one sets the wait, not the sum.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::parse::*;
use super::run::{HELPER_DEADLINE, run_bounded};
use super::*;
use crate::collect::disk::parse_mountinfo;
use crate::collect::gpu::query_marketing_name;
use crate::collect::memory::{parse_dmi_memory, parse_meminfo};
use crate::collect::read::{read_trimmed, read_u64};

const UDEV_DMI: &str = "/run/udev/data/+dmi:id";
const CPU_ROOT: &str = "/sys/devices/system/cpu";

/// Read the whole machine. Never fails: what can't be read is absent,
/// and a section that can't be read at all is listed in `unavailable`.
pub fn collect() -> SystemSpecs {
    // Helpers first, in parallel, while sysfs is read on this thread.
    let vulkan = std::thread::spawn(|| run_bounded("vulkaninfo", &["--summary"], HELPER_DEADLINE));
    let glx = std::thread::spawn(|| {
        // glxinfo needs an X display (Xwayland counts); without one it
        // would only fail slowly.
        std::env::var_os("DISPLAY").and_then(|_| run_bounded("glxinfo", &["-B"], HELPER_DEADLINE))
    });
    let desktop = std::thread::spawn(desktop_and_outputs);
    let pactl = std::thread::spawn(|| run_bounded("pactl", &["info"], HELPER_DEADLINE));

    let dmi = fs::read_to_string(UDEV_DMI).unwrap_or_default();
    let mut specs = SystemSpecs { ts: crate::collect::unix_now(), ..Default::default() };

    specs.cpu = cpu_spec();
    if specs.cpu.is_none() {
        specs.unavailable.push(Unavailable {
            section: "cpu".into(),
            reason: "/proc/cpuinfo and /sys/devices/system/cpu are unreadable".into(),
        });
    }
    specs.memory = memory_spec(&dmi);
    if specs.memory.is_none() {
        specs.unavailable.push(Unavailable { section: "memory".into(), reason: "/proc/meminfo is unreadable".into() });
    }
    specs.drives = drives();
    if specs.drives.is_empty() {
        specs.unavailable.push(Unavailable {
            section: "storage".into(),
            reason: "no block devices under /sys/block (a container?)".into(),
        });
    }
    specs.network = nics();

    let vulkan = vulkan.join().ok().flatten().map(|s| parse_vulkan_summary(&s)).unwrap_or_default();
    let glx = glx.join().ok().flatten().as_deref().and_then(parse_glxinfo);
    specs.gpus = gpus(&vulkan, glx.as_ref());

    let (desktop, outputs) = desktop.join().unwrap_or_default();
    specs.system = Some(system_spec(&dmi, desktop));
    specs.displays = displays(outputs);

    let server = pactl.join().ok().flatten().as_deref().and_then(audio_server);
    let devices = fs::read_to_string("/proc/asound/cards").map(|c| alsa_cards(&c)).unwrap_or_default();
    if server.is_some() || !devices.is_empty() {
        specs.audio = Some(AudioSpec { server, devices });
    }
    specs
}

// ------------------------------------------------------------------ system

fn system_spec(dmi: &str, desktop: Option<String>) -> SystemSpec {
    let os_release = fs::read_to_string("/etc/os-release")
        .or_else(|_| fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    let dmi_field = |name: &str| {
        read_trimmed(format!("/sys/class/dmi/id/{name}"))
            .or_else(|| udev_value(dmi, &format!("ID_{}", name.to_ascii_uppercase())))
            .filter(|value| !dmi_placeholder(value))
    };
    let vendor_model = |vendor: Option<String>, model: Option<String>| match (vendor, model) {
        (Some(vendor), Some(model)) if model.starts_with(&vendor) => Some(model),
        (Some(vendor), Some(model)) => Some(format!("{vendor} {model}")),
        (None, model) => model,
        (Some(_), None) => None,
    };
    let board = vendor_model(dmi_field("board_vendor"), dmi_field("board_name"));
    let mut model = vendor_model(dmi_field("sys_vendor"), {
        // ThinkPads put the marketing name in product_version.
        let version = dmi_field("product_version").filter(|v| v.chars().any(|c| c.is_ascii_alphabetic()));
        match (dmi_field("product_name"), version) {
            (Some(name), Some(version)) if !version.contains(&name) => Some(format!("{version} ({name})")),
            (Some(name), _) => Some(name),
            (None, version) => version,
        }
    });
    // A self-built desktop's "product" is just the board again.
    if let (Some(model_name), Some(board_name)) = (&model, &board)
        && (model_name == board_name || board_name.ends_with(model_name.as_str()))
    {
        model = None;
    }
    let efi = Path::new("/sys/firmware/efi").exists();
    SystemSpec {
        os: shell_value(&os_release, "PRETTY_NAME"),
        os_base: fs::read_to_string("/etc/upstream-release/lsb-release").ok().as_deref().and_then(upstream_release),
        kernel: read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_default(),
        architecture: read_trimmed("/proc/sys/kernel/arch").unwrap_or_else(|| std::env::consts::ARCH.to_string()),
        hostname: read_trimmed("/proc/sys/kernel/hostname").unwrap_or_default(),
        session_type: std::env::var("XDG_SESSION_TYPE").ok().filter(|s| !s.is_empty()),
        desktop,
        model,
        board,
        firmware: match (dmi_field("bios_vendor"), dmi_field("bios_version")) {
            (Some(vendor), Some(version)) => Some(format!("{vendor} {version}")),
            (None, Some(version)) => Some(version),
            _ => None,
        },
        firmware_date: dmi_field("bios_date").map(|date| iso_date(&date)),
        boot_mode: if efi { "UEFI" } else { "BIOS" }.to_string(),
        secure_boot: efi.then(secure_boot).flatten(),
    }
}

/// The SecureBoot EFI variable: 4 attribute bytes, then 1 = on.
fn secure_boot() -> Option<bool> {
    let entries = fs::read_dir("/sys/firmware/efi/efivars").ok()?;
    let variable = entries
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().starts_with("SecureBoot-8be4df61"))?;
    let bytes = fs::read(variable.path()).ok()?;
    bytes.get(4).map(|&state| state == 1)
}

/// The running desktop, and the outputs as the compositor sees them.
/// Sway (and SwayFX) answer over IPC with the active modes; anything
/// else is named from XDG_CURRENT_DESKTOP, and displays fall back to
/// EDID (marked as the preferred, not necessarily active, mode).
fn desktop_and_outputs() -> (Option<String>, Option<Vec<DisplaySpec>>) {
    let current = std::env::var("XDG_CURRENT_DESKTOP").ok().filter(|s| !s.is_empty());
    if std::env::var_os("SWAYSOCK").is_some() {
        let version = run_bounded("swaymsg", &["-r", "-t", "get_version"], HELPER_DEADLINE)
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        let outputs = run_bounded("swaymsg", &["-r", "-t", "get_outputs"], HELPER_DEADLINE)
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .map(|value| sway_outputs(&value));
        let desktop = version.as_ref().map(sway_desktop).or(current);
        return (desktop, outputs);
    }
    let desktop = current.map(|name| {
        let version = match name.as_str() {
            "X-Cinnamon" | "Cinnamon" => run_bounded("cinnamon", &["--version"], HELPER_DEADLINE),
            "GNOME" | "ubuntu:GNOME" => run_bounded("gnome-shell", &["--version"], HELPER_DEADLINE),
            "KDE" => run_bounded("plasmashell", &["--version"], HELPER_DEADLINE),
            "XFCE" => None,
            _ => None,
        };
        version.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).unwrap_or(name)
    });
    (desktop, None)
}

/// get_version → "SwayFX 0.6-c4c8d51b (Sway 1.12.0)" or "Sway 1.10".
/// The build date and branch that follow the version are dropped:
/// they describe a build, not the desktop.
pub(crate) fn sway_desktop(version: &serde_json::Value) -> String {
    let human = version["human_readable"].as_str().unwrap_or("").split_whitespace().next().unwrap_or("");
    match version["sway_original_version"].as_str() {
        // SwayFX reports its own version and the Sway it is built on.
        Some(original) => format!("SwayFX {human} (Sway {original})"),
        None => format!("Sway {human}"),
    }
}

pub(crate) fn sway_outputs(outputs: &serde_json::Value) -> Vec<DisplaySpec> {
    outputs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|output| output["active"].as_bool() == Some(true))
        .map(|output| {
            let text = |key: &str| {
                output[key].as_str().map(str::trim).filter(|s| !s.is_empty() && *s != "Unknown").map(String::from)
            };
            let mode = &output["current_mode"];
            DisplaySpec {
                connector: text("name").unwrap_or_default(),
                make: text("make"),
                model: text("model"),
                diagonal_inches: None,
                width_px: mode["width"].as_u64().map(|v| v as u32),
                height_px: mode["height"].as_u64().map(|v| v as u32),
                refresh_hz: mode["refresh"].as_f64().map(|mhz| (mhz / 1000.0) as f32),
                scale: output["scale"].as_f64().map(|s| s as f32),
                mode_source: "active".to_string(),
            }
        })
        .collect()
}

/// Compositor outputs when there are any, else every connected DRM
/// connector's EDID. Either way the EDID adds the panel's size.
fn displays(from_compositor: Option<Vec<DisplaySpec>>) -> Vec<DisplaySpec> {
    let pnp = fs::read_to_string("/usr/share/hwdata/pnp.ids")
        .or_else(|_| fs::read_to_string("/usr/share/misc/pnp.ids"))
        .unwrap_or_default();
    let mut edids: BTreeMap<String, Edid> = BTreeMap::new();
    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // card1-DP-1 → DP-1
            let Some((_, connector)) = name.split_once('-').filter(|(card, _)| card.starts_with("card")) else {
                continue;
            };
            if read_trimmed(entry.path().join("status")).as_deref() != Some("connected") {
                continue;
            }
            if let Some(edid) = fs::read(entry.path().join("edid")).ok().as_deref().and_then(parse_edid) {
                edids.insert(connector.to_string(), edid);
            }
        }
    }
    match from_compositor {
        Some(mut outputs) if !outputs.is_empty() => {
            for output in &mut outputs {
                if let Some(edid) = edids.get(&output.connector) {
                    output.diagonal_inches = diagonal_inches(edid.width_cm, edid.height_cm);
                }
            }
            outputs
        }
        _ => edids
            .into_iter()
            .map(|(connector, edid)| DisplaySpec {
                connector,
                make: pnp_vendor(&pnp, &edid.manufacturer_id).or(Some(edid.manufacturer_id.clone())),
                model: edid.name.clone(),
                diagonal_inches: diagonal_inches(edid.width_cm, edid.height_cm),
                width_px: edid.preferred.map(|p| p.0),
                height_px: edid.preferred.map(|p| p.1),
                refresh_hz: edid.preferred.map(|p| p.2),
                scale: None,
                mode_source: "preferred".to_string(),
            })
            .collect(),
    }
}

// --------------------------------------------------------------------- cpu

fn cpu_spec() -> Option<CpuSpec> {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok();
    let field = |key: &str| {
        cpuinfo.as_deref()?.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == key).then(|| value.trim().to_string())
        })
    };
    let online = cpu_list(&read_trimmed(format!("{CPU_ROOT}/online")).unwrap_or_default());
    let threads = if online.is_empty() {
        cpuinfo.as_deref().map(|c| c.lines().filter(|l| l.starts_with("processor")).count()).unwrap_or(0)
    } else {
        online.len()
    };
    if cpuinfo.is_none() && threads == 0 {
        return None;
    }

    let mut packages = BTreeSet::new();
    let mut cores = BTreeSet::new();
    let mut max_khz: Option<u64> = None;
    for cpu in &online {
        let topology = format!("{CPU_ROOT}/cpu{cpu}/topology");
        if let (Some(package), Some(core)) =
            (read_u64(format!("{topology}/physical_package_id")), read_u64(format!("{topology}/core_id")))
        {
            packages.insert(package);
            cores.insert((package, core));
        }
        if let Some(khz) = read_u64(format!("{CPU_ROOT}/cpu{cpu}/cpufreq/cpuinfo_max_freq")) {
            max_khz = Some(max_khz.map_or(khz, |m| m.max(khz)));
        }
    }

    let caches = cache_totals(&online);
    let cpufreq = |name: &str| read_trimmed(format!("{CPU_ROOT}/cpu0/cpufreq/{name}"));
    let boost = read_trimmed(format!("{CPU_ROOT}/cpufreq/boost"))
        .or_else(|| cpufreq("boost"))
        .and_then(|v| v.parse::<u8>().ok())
        .map(|v| v == 1)
        .or_else(|| read_trimmed(format!("{CPU_ROOT}/intel_pstate/no_turbo")).map(|v| v == "0"));
    let base_mhz = read_u64(format!("{CPU_ROOT}/cpu0/acpi_cppc/nominal_freq"))
        .filter(|&mhz| mhz > 0)
        .map(|mhz| mhz as f64)
        .or_else(|| read_u64(format!("{CPU_ROOT}/cpu0/cpufreq/base_frequency")).map(|khz| khz as f64 / 1000.0));

    Some(CpuSpec {
        model: field("model name").or_else(|| field("Model")).filter(|m| !m.is_empty()),
        sockets: (!packages.is_empty()).then_some(packages.len()),
        cores: (!cores.is_empty()).then_some(cores.len()),
        threads,
        base_mhz,
        max_mhz: max_khz.map(|khz| khz as f64 / 1000.0),
        boost_enabled: boost,
        cache_l1d_bytes: caches.get(&(1, "Data")).copied(),
        cache_l1i_bytes: caches.get(&(1, "Instruction")).copied(),
        cache_l2_bytes: caches.get(&(2, "Unified")).copied(),
        cache_l3_bytes: caches.get(&(3, "Unified")).copied(),
        frequency_driver: cpufreq("scaling_driver"),
        governor: cpufreq("scaling_governor"),
        energy_preference: cpufreq("energy_performance_preference"),
        features: field("flags").map(|flags| cpu_features(&flags)).unwrap_or_default(),
    })
}

/// "0-7,16-23" → [0..=7, 16..=23].
pub(crate) fn cpu_list(list: &str) -> Vec<u32> {
    let mut cpus = Vec::new();
    for range in list.trim().split(',').filter(|r| !r.is_empty()) {
        match range.split_once('-') {
            Some((start, end)) => {
                if let (Ok(start), Ok(end)) = (start.parse::<u32>(), end.parse::<u32>()) {
                    cpus.extend(start..=end);
                }
            }
            None => cpus.extend(range.parse::<u32>().ok()),
        }
    }
    cpus
}

/// Machine-wide size of each cache level, every physical cache counted
/// once: caches are deduplicated by the set of CPUs that share them.
fn cache_totals(online: &[u32]) -> BTreeMap<(u32, &'static str), u64> {
    let mut seen: BTreeSet<(u32, String, String)> = BTreeSet::new();
    let mut totals: BTreeMap<(u32, &'static str), u64> = BTreeMap::new();
    for cpu in online {
        let Ok(entries) = fs::read_dir(format!("{CPU_ROOT}/cpu{cpu}/cache")) else { continue };
        for entry in entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("index")) {
            let path = entry.path();
            let (Some(level), Some(kind), Some(size), Some(shared)) = (
                read_u64(path.join("level")),
                read_trimmed(path.join("type")),
                read_trimmed(path.join("size")).as_deref().and_then(cache_size_bytes),
                read_trimmed(path.join("shared_cpu_list")),
            ) else {
                continue;
            };
            let kind: &'static str = match kind.as_str() {
                "Data" => "Data",
                "Instruction" => "Instruction",
                _ => "Unified",
            };
            if seen.insert((level as u32, kind.to_string(), shared)) {
                *totals.entry((level as u32, kind)).or_default() += size;
            }
        }
    }
    totals
}

// ------------------------------------------------------------------ memory

fn memory_spec(dmi: &str) -> Option<MemorySpec> {
    let meminfo = parse_meminfo(&fs::read_to_string("/proc/meminfo").ok()?);
    let modules = parse_dmi_memory(dmi);
    let installed = (!modules.is_empty()).then(|| modules.iter().map(|m| m.size_bytes).sum());
    Some(MemorySpec {
        modules,
        slots: dmi_memory_slots(dmi),
        installed_bytes: installed,
        usable_bytes: meminfo.total_bytes,
        swap_bytes: meminfo.swap_total_bytes,
    })
}

// ---------------------------------------------------------------- graphics

fn gpus(vulkan: &[parse::VulkanDevice], glx: Option<&(u32, u32, ApiDriver)>) -> Vec<GpuSpec> {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let mut cards: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let digits = name.strip_prefix("card")?;
            (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then_some(name)
        })
        .collect();
    cards.sort_by_key(|name| name[4..].parse::<u32>().unwrap_or(0));
    let kfd = kfd_nodes();
    let rocm = rocm_version();
    // NVML knows what the PCI tree can't about an NVIDIA card (VRAM,
    // CUDA cores, the power limit, the CUDA stack). One short session.
    let nvidia = crate::collect::nvidia::spec();

    let mut gpus = Vec::new();
    let mut seen = BTreeSet::new();
    let mut add = |device: PathBuf| {
        let Ok(real) = fs::canonicalize(&device) else { return };
        let pci_address = real.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !seen.insert(pci_address.clone()) {
            return;
        }
        let hex = |name: &str| {
            read_trimmed(device.join(name)).and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        };
        let (Some(vendor), Some(device_id)) = (hex("vendor"), hex("device")) else { return };
        let driver = fs::read_link(device.join("driver"))
            .ok()
            .and_then(|link| link.file_name().map(|n| n.to_string_lossy().to_string()));
        let amd = vendor == 0x1002;
        let nv = nvidia.as_ref().filter(|spec| vendor == 0x10de && spec.pci_address == pci_address);
        let name = if amd {
            // pci.ids brackets the marketing name without the brand
            // ("Radeon AI PRO R9700"); a spec sheet wants it whole.
            let name = query_marketing_name(&pci_address, &device);
            if name.starts_with("AMD") { name } else { format!("AMD {name}") }
        } else if let Some(nv) = nv {
            nv.name.clone() // NVML's own name: "NVIDIA GeForce RTX 3070"
        } else {
            crate::collect::gpu::lspci_name(&pci_address).unwrap_or_else(|| format!("GPU {vendor:04x}:{device_id:04x}"))
        };
        let hwmon = fs::read_dir(device.join("hwmon"))
            .ok()
            .and_then(|mut entries| entries.next())
            .and_then(|entry| entry.ok())
            .map(|entry| entry.path());
        let watts = |file: &str| {
            hwmon.as_ref().and_then(|h| read_u64(h.join(file))).filter(|&uw| uw > 0).map(|uw| uw as f32 / 1e6)
        };
        let node = kfd.iter().find(|node| node.location == pci_location(&pci_address));
        let architecture = node.and_then(|node| gfx_target(node.gfx_target_version));
        let vulkan_driver = vulkan
            .iter()
            .find(|d| d.physical && d.vendor_id == vendor && d.device_id == device_id)
            .map(|d| ApiDriver { version: d.api_version.clone(), driver: d.driver.clone() });
        let opengl = glx
            .filter(|(v, d, _)| *v == vendor && *d == device_id)
            .map(|(_, _, api)| api.clone());
        gpus.push(GpuSpec {
            name,
            pci_id: format!("{vendor:04x}:{device_id:04x}"),
            pci_address,
            driver,
            vram_bytes: if amd {
                read_u64(device.join("mem_info_vram_total")).filter(|&b| b > 0)
            } else {
                nv.and_then(|nv| nv.vram_bytes)
            },
            family: architecture
                .as_deref()
                .and_then(amd_family)
                .map(String::from)
                .or_else(|| nv.and_then(|nv| nv.family.clone())),
            architecture: architecture.or_else(|| nv.and_then(|nv| nv.architecture.clone())),
            compute_units: node.and_then(|node| node.compute_units),
            cuda_cores: nv.and_then(|nv| nv.cuda_cores),
            pcie_link: pcie_link(&real).map(|link| match nv.and_then(|nv| nv.link_max_generation) {
                // Idle, the card has dropped to Gen 1: the slot's
                // generation is NVML's, the width stays the trained one.
                Some(slot) if slot > link.generation => PcieLink {
                    generation: slot,
                    device_max_generation: link.device_max_generation.filter(|&card| card > slot),
                    ..link
                },
                _ => link,
            }),
            power_cap_watts: watts("power1_cap").or_else(|| nv.and_then(|nv| nv.power_cap_watts)),
            power_cap_max_watts: watts("power1_cap_max").or_else(|| nv.and_then(|nv| nv.power_cap_max_watts)),
            vulkan: vulkan_driver,
            opengl,
            compute_stack: if amd && node.is_some() {
                rocm.clone()
            } else {
                nv.and_then(|nv| nv.compute_stack.clone())
            },
        });
    };
    for card in cards {
        add(PathBuf::from(format!("/sys/class/drm/{card}/device")));
    }
    // Display controllers with no DRM node: a compute-only NVIDIA card
    // (nvidia-drm not loaded) is on the PCI bus and nowhere under
    // /sys/class/drm. PCI class 0x03 is every display controller.
    if let Ok(entries) = fs::read_dir("/sys/bus/pci/devices") {
        let mut addresses: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        addresses.sort();
        for device in addresses {
            let class = read_trimmed(device.join("class")).and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
            if class.is_some_and(|class| class >> 16 == 0x03) {
                add(device);
            }
        }
    }
    gpus
}

struct KfdNode {
    location: u32,
    gfx_target_version: u32,
    compute_units: Option<u32>,
}

/// KFD topology: one node per GPU agent ROCm can see.
fn kfd_nodes() -> Vec<KfdNode> {
    let Ok(entries) = fs::read_dir("/sys/class/kfd/kfd/topology/nodes") else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|entry| {
            let properties = fs::read_to_string(entry.path().join("properties")).ok()?;
            let value = |key: &str| {
                properties.lines().find_map(|line| {
                    let (name, value) = line.split_once(' ')?;
                    (name == key).then(|| value.trim().parse::<u32>().ok()).flatten()
                })
            };
            let simds = value("simd_count")?;
            if simds == 0 {
                return None; // the CPU agent
            }
            Some(KfdNode {
                location: value("location_id")?,
                gfx_target_version: value("gfx_target_version").unwrap_or(0),
                compute_units: value("simd_per_cu").filter(|&n| n > 0).map(|per_cu| simds / per_cu),
            })
        })
        .collect()
}

/// "0000:0a:00.0" → KFD's location_id (bus << 8 | device << 3 | fn).
fn pci_location(address: &str) -> u32 {
    let mut parts = address.rsplit(':');
    let device_function = parts.next().unwrap_or("");
    let bus = parts.next().and_then(|b| u32::from_str_radix(b, 16).ok()).unwrap_or(0);
    let (device, function) = device_function.split_once('.').unwrap_or(("0", "0"));
    let device = u32::from_str_radix(device, 16).unwrap_or(0);
    let function = function.parse::<u32>().unwrap_or(0);
    (bus << 8) | (device << 3) | function
}

/// The installed ROCm release, from rocm-core's own version file.
fn rocm_version() -> Option<String> {
    let direct = read_trimmed("/opt/rocm/.info/version");
    let versioned = || {
        let entries = fs::read_dir("/opt/rocm").ok()?;
        entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("core-"))
            .filter_map(|e| read_trimmed(e.path().join(".info/version")))
            .max()
    };
    direct
        .or_else(versioned)
        .map(|version| version.split(['-', '_']).next().unwrap_or(&version).to_string())
        .map(|version| format!("ROCm {version}"))
}

/// The link a PCIe device actually gets: walk from the device toward
/// the root port; the narrowest, slowest hop is the effective link.
pub(crate) fn pcie_link(device: &Path) -> Option<PcieLink> {
    let read_link = |path: &Path| {
        let generation = read_trimmed(path.join("current_link_speed")).as_deref().and_then(pcie_generation)?;
        let width = read_u64(path.join("current_link_width")).filter(|&w| w > 0)? as u32;
        Some((generation, width))
    };
    let (own_generation, own_width) = read_link(device)?;
    let device_max_generation = read_trimmed(device.join("max_link_speed")).as_deref().and_then(pcie_generation);
    let device_max_width = read_u64(device.join("max_link_width")).map(|w| w as u32);
    let (mut generation, mut width) = (own_generation, own_width);
    let mut cursor = device.parent();
    while let Some(path) = cursor {
        match read_link(path) {
            Some((hop_generation, hop_width)) => {
                generation = generation.min(hop_generation);
                width = width.min(hop_width);
            }
            None => break,
        }
        cursor = path.parent();
    }
    let better = |max: Option<u32>, effective: u32| max.filter(|&m| m > effective);
    Some(PcieLink {
        generation,
        width,
        device_max_generation: better(device_max_generation, generation),
        device_max_width: better(device_max_width, width),
    })
}

// ----------------------------------------------------------------- storage

fn drives() -> Vec<DriveSpec> {
    let Ok(entries) = fs::read_dir("/sys/block") else { return Vec::new() };
    let mounts = fs::read_to_string("/proc/self/mountinfo").map(|m| parse_mountinfo(&m)).unwrap_or_default();
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| !["loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd"].iter().any(|p| name.starts_with(p)))
        .collect();
    names.sort();
    let mut drives = Vec::new();
    for name in names {
        let root = PathBuf::from(format!("/sys/block/{name}"));
        let size_bytes = read_u64(root.join("size")).unwrap_or(0) * 512;
        if size_bytes == 0 {
            continue; // an empty card reader slot
        }
        let udev = read_trimmed(root.join("dev"))
            .and_then(|dev| fs::read_to_string(format!("/run/udev/data/b{dev}")).ok())
            .unwrap_or_default();
        let model = udev_value(&udev, "ID_MODEL_ENC")
            .map(|encoded| unescape_udev(&encoded))
            .or_else(|| read_trimmed(root.join("device/model")))
            .map(|m| m.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|m| !m.is_empty());
        let rotational = read_trimmed(root.join("queue/rotational")).as_deref() == Some("1");
        let removable = read_trimmed(root.join("removable")).as_deref() == Some("1");
        let bus = udev_value(&udev, "ID_BUS");
        let rpm = udev_value(&udev, "ID_ATA_ROTATION_RATE_RPM").and_then(|r| r.parse().ok()).filter(|&r: &u32| r > 0);
        let (kind, link) = if name.starts_with("nvme") {
            let controller = fs::canonicalize(root.join("device/device")).ok();
            let link = controller.as_deref().and_then(pcie_link).map(|link| format!("PCIe {}.0 x{}", link.generation, link.width));
            ("NVMe SSD".to_string(), link)
        } else if name.starts_with("mmcblk") {
            ("SD/eMMC".to_string(), None)
        } else if bus.as_deref() == Some("usb") {
            ("USB".to_string(), None)
        } else {
            let kind = if rotational { "SATA HDD" } else { "SATA SSD" };
            (kind.to_string(), sata_link(&root))
        };
        drives.push(DriveSpec {
            name: name.clone(),
            model,
            kind,
            size_bytes,
            rpm,
            link,
            removable,
            filesystems: filesystems(&name, &root, &mounts),
        });
    }
    drives
}

/// udev's ID_MODEL_ENC escapes spaces as \x20.
fn unescape_udev(encoded: &str) -> String {
    let mut out = String::new();
    let mut rest = encoded;
    while let Some(index) = rest.find("\\x") {
        out.push_str(&rest[..index]);
        match rest.get(index + 2..index + 4).and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
            Some(byte) => {
                out.push(byte as char);
                rest = &rest[index + 4..];
            }
            None => {
                out.push_str("\\x");
                rest = &rest[index + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The SATA link a drive negotiated: its ata port's link speed.
fn sata_link(root: &Path) -> Option<String> {
    let device = fs::canonicalize(root.join("device")).ok()?;
    let ata = device.ancestors().find_map(|p| {
        let name = p.file_name()?.to_string_lossy().to_string();
        name.strip_prefix("ata").filter(|n| n.chars().all(|c| c.is_ascii_digit())).map(String::from)
    })?;
    let speed = read_trimmed(format!("/sys/class/ata_link/link{ata}/sata_spd"))?;
    speed.chars().next().is_some_and(|c| c.is_ascii_digit()).then(|| format!("SATA {speed}").replace("Gbps", "Gb/s"))
}

fn filesystems(disk: &str, root: &Path, mounts: &[crate::collect::disk::MountEntry]) -> Vec<FilesystemSpec> {
    let mut partitions: Vec<String> = fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with(disk) && root.join(n).join("partition").exists())
                .collect()
        })
        .unwrap_or_default();
    partitions.sort_by_key(|p| p.trim_start_matches(disk).trim_start_matches('p').parse::<u32>().unwrap_or(0));
    if partitions.is_empty() {
        partitions.push(disk.to_string());
    }
    partitions
        .into_iter()
        .filter_map(|partition| {
            let dev = read_trimmed(if partition == disk { root.join("dev") } else { root.join(&partition).join("dev") });
            let fs_type = dev
                .and_then(|dev| fs::read_to_string(format!("/run/udev/data/b{dev}")).ok())
                .and_then(|udev| udev_value(&udev, "ID_FS_TYPE"));
            let device_path = format!("/dev/{partition}");
            let mut mount_points: Vec<String> =
                mounts.iter().filter(|m| m.device == device_path).map(|m| m.mount_point.clone()).collect();
            mount_points.dedup();
            let fs_type = fs_type.or_else(|| mounts.iter().find(|m| m.device == device_path).map(|m| m.fs_type.clone()));
            (fs_type.is_some() || !mount_points.is_empty()).then_some(FilesystemSpec {
                device: partition,
                fs_type,
                mount_points,
            })
        })
        .collect()
}

// ----------------------------------------------------------------- network

fn nics() -> Vec<NicSpec> {
    let Ok(entries) = fs::read_dir("/sys/class/net") else { return Vec::new() };
    let mut nics: Vec<NicSpec> = entries
        .flatten()
        .filter(|e| e.path().join("device").exists())
        .map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let udev = read_trimmed(path.join("ifindex"))
                .and_then(|index| fs::read_to_string(format!("/run/udev/data/n{index}")).ok())
                .unwrap_or_default();
            let wireless = path.join("wireless").exists() || path.join("phy80211").exists();
            let up = read_trimmed(path.join("operstate")).as_deref() == Some("up");
            let speed = read_trimmed(path.join("speed")).and_then(|s| s.parse::<i64>().ok()).filter(|&s| s > 0);
            let model = udev_value(&udev, "ID_MODEL_FROM_DATABASE")
                .or_else(|| {
                    let device = fs::canonicalize(path.join("device")).ok()?;
                    let address = device.file_name()?.to_string_lossy().to_string();
                    crate::collect::gpu::lspci_name(&address)
                })
                .filter(|m| !m.is_empty());
            NicSpec {
                name,
                kind: if wireless { "Wi-Fi" } else { "Ethernet" }.to_string(),
                model,
                driver: fs::read_link(path.join("device/driver"))
                    .ok()
                    .and_then(|l| l.file_name().map(|n| n.to_string_lossy().to_string())),
                link_up: up,
                speed_mbps: speed.filter(|_| up).map(|s| s as u32),
                duplex: read_trimmed(path.join("duplex")).filter(|d| up && d != "unknown"),
            }
        })
        .collect();
    nics.sort_by(|a, b| a.name.cmp(&b.name));
    nics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists_expand() {
        assert_eq!(cpu_list("0-3,8"), vec![0, 1, 2, 3, 8]);
        assert_eq!(cpu_list("0"), vec![0]);
        assert!(cpu_list("").is_empty());
    }

    #[test]
    fn pci_location_matches_kfd() {
        // The R9700 at 0a:00.0 is KFD location_id 2560.
        assert_eq!(pci_location("0000:0a:00.0"), 2560);
        assert_eq!(pci_location("0000:03:00.1"), (3 << 8) | 1);
    }

    #[test]
    fn udev_model_escapes_unescape() {
        assert_eq!(unescape_udev("KINGSTON\\x20SA400S37960G\\x20\\x20"), "KINGSTON SA400S37960G  ");
        assert_eq!(unescape_udev("plain"), "plain");
    }

    #[test]
    fn sway_names_swayfx_and_its_base() {
        let fx = serde_json::json!({"human_readable": "0.6-c4c8d51b (Oct  8 2026, branch 'nexus')", "sway_original_version": "1.12.0"});
        assert_eq!(sway_desktop(&fx), "SwayFX 0.6-c4c8d51b (Sway 1.12.0)");
        let plain = serde_json::json!({"human_readable": "1.10"});
        assert_eq!(sway_desktop(&plain), "Sway 1.10");
    }

    #[test]
    fn sway_outputs_keep_only_active_modes() {
        let outputs = serde_json::json!([
            {"name": "DP-1", "make": "HP Inc.", "model": "HP X32", "active": true, "scale": 1.0,
             "current_mode": {"width": 2560, "height": 1440, "refresh": 164834}},
            {"name": "HDMI-A-1", "make": "Unknown", "active": false}
        ]);
        let displays = sway_outputs(&outputs);
        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].connector, "DP-1");
        assert_eq!(displays[0].make.as_deref(), Some("HP Inc."));
        assert_eq!((displays[0].width_px, displays[0].height_px), (Some(2560), Some(1440)));
        assert!((displays[0].refresh_hz.unwrap() - 164.834).abs() < 0.01);
        assert_eq!(displays[0].mode_source, "active");
    }
}
