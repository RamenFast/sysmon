// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure parsers for the spec sheet's sources. Each takes the text a
//! file or helper produced and returns what it means; the collector
//! does the reading. Fixtures in the tests are real output from the
//! machines named beside them.

use super::ApiDriver;

/// `KEY="value"` / `KEY=value` lines (os-release, lsb-release).
pub fn shell_value(content: &str, key: &str) -> Option<String> {
    content
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(|value| value.trim().trim_matches('"').trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The distribution a derivative is built on, from
/// /etc/upstream-release/lsb-release (Mint, elementary, Pop!_OS…):
/// "Ubuntu 24.04".
pub fn upstream_release(lsb_release: &str) -> Option<String> {
    let id = shell_value(lsb_release, "DISTRIB_ID")?;
    let release = shell_value(lsb_release, "DISTRIB_RELEASE");
    Some(match release {
        Some(release) => format!("{id} {release}"),
        None => id,
    })
}

/// sysfs cache sizes: "32K", "32768K", "8M".
pub fn cache_size_bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let (digits, multiplier) = match text.chars().last()? {
        'K' => (&text[..text.len() - 1], 1024),
        'M' => (&text[..text.len() - 1], 1024 * 1024),
        'G' => (&text[..text.len() - 1], 1024 * 1024 * 1024),
        _ => (text, 1),
    };
    digits.parse::<u64>().ok().map(|value| value * multiplier)
}

/// The instruction-set features a reader asks about, named the way
/// spec pages name them, strongest first.
pub fn cpu_features(flags: &str) -> Vec<String> {
    let present: std::collections::HashSet<&str> = flags.split_ascii_whitespace().collect();
    [
        ("avx512f", "AVX-512"),
        ("avx2", "AVX2"),
        ("fma", "FMA3"),
        ("aes", "AES-NI"),
        ("sha_ni", "SHA"),
        ("svm", "AMD-V"),
        ("vmx", "VT-x"),
    ]
    .iter()
    .filter(|(flag, _)| present.contains(flag))
    .map(|(_, name)| name.to_string())
    .collect()
}

/// sysfs link speed ("16.0 GT/s PCIe") → PCIe generation.
pub fn pcie_generation(speed: &str) -> Option<u32> {
    let gigatransfers: f64 = speed.split_ascii_whitespace().next()?.parse().ok()?;
    Some(match gigatransfers {
        rate if rate >= 63.0 => 6,
        rate if rate >= 31.0 => 5,
        rate if rate >= 15.0 => 4,
        rate if rate >= 7.0 => 3,
        rate if rate >= 4.0 => 2,
        rate if rate >= 2.0 => 1,
        _ => return None,
    })
}

/// KFD `gfx_target_version` (major·10000 + minor·100 + stepping) →
/// the ISA name ROCm uses: 120001 → "gfx1201", 90010 → "gfx90a".
pub fn gfx_target(version: u32) -> Option<String> {
    if version == 0 {
        return None;
    }
    let (major, minor, stepping) = (version / 10000, (version / 100) % 100, version % 100);
    (minor < 16 && stepping < 16).then(|| format!("gfx{major}{minor:x}{stepping:x}"))
}

/// The architecture family an AMD ISA belongs to.
pub fn amd_family(gfx: &str) -> Option<&'static str> {
    let id = gfx.strip_prefix("gfx")?;
    Some(match id {
        id if id.starts_with("12") => "RDNA 4",
        id if id.starts_with("11") => "RDNA 3",
        id if id.starts_with("103") => "RDNA 2",
        id if id.starts_with("101") => "RDNA",
        "908" => "CDNA",
        "90a" => "CDNA 2",
        id if id.starts_with("94") || id.starts_with("95") => "CDNA 3",
        "900" | "902" | "904" | "906" | "909" | "90c" => "GCN 5 (Vega)",
        id if id.starts_with('8') => "GCN 3/4",
        _ => return None,
    })
}

/// `E:MEMORY_ARRAY_NUM_DEVICES=4` → slots on the board.
pub fn dmi_memory_slots(dmi: &str) -> Option<u32> {
    dmi.lines()
        .filter_map(|line| line.strip_prefix("E:MEMORY_ARRAY_NUM_DEVICES="))
        .filter_map(|value| value.trim().parse::<u32>().ok())
        .reduce(|a, b| a + b)
        .filter(|&slots| slots > 0)
}

/// One device block of `vulkaninfo --summary`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VulkanDevice {
    pub vendor_id: u32,
    pub device_id: u32,
    pub physical: bool,
    pub api_version: String,
    pub driver: String,
}

pub fn parse_vulkan_summary(summary: &str) -> Vec<VulkanDevice> {
    let mut devices = Vec::new();
    let Some((_, device_part)) = summary.split_once("Devices:") else {
        return devices;
    };
    for block in device_part.split("\nGPU").skip(1) {
        let field = |key: &str| {
            block.lines().find_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name.trim() == key).then(|| value.trim().to_string())
            })
        };
        let hex = |key: &str| field(key).and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
        let (Some(vendor_id), Some(device_id), Some(api_version)) =
            (hex("vendorID"), hex("deviceID"), field("apiVersion"))
        else {
            continue;
        };
        let device_type = field("deviceType").unwrap_or_default();
        let name = field("driverName").unwrap_or_default();
        let info = field("driverInfo").unwrap_or_default();
        devices.push(VulkanDevice {
            vendor_id,
            device_id,
            physical: device_type.contains("DISCRETE_GPU") || device_type.contains("INTEGRATED_GPU"),
            api_version,
            driver: driver_label(&name, &info),
        });
    }
    devices
}

/// "Mesa 25.2.8 (RADV)" from radv + "Mesa 25.2.8"; "NVIDIA 560.35.03"
/// from NVIDIA + "560.35.03".
fn driver_label(name: &str, info: &str) -> String {
    let short = match name {
        "radv" => "RADV".to_string(),
        "" => String::new(),
        other => other.to_string(),
    };
    if info.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("{short} {info}").trim().to_string()
    } else if short.is_empty() || info.contains(&short) {
        info.to_string()
    } else {
        format!("{info} ({short})")
    }
}

/// `glxinfo -B` → (PCI vendor id, PCI device id, the OpenGL version
/// and the driver behind it). The ids come from the
/// GLX_MESA_query_renderer block; without it the context cannot be
/// tied to a card and the result is None.
pub fn parse_glxinfo(text: &str) -> Option<(u32, u32, ApiDriver)> {
    let trailing_hex = |line: &str| {
        let open = line.rfind("(0x")?;
        u32::from_str_radix(line[open + 3..].trim_end_matches(')').trim(), 16).ok()
    };
    let vendor = text.lines().find_map(|l| l.trim().strip_prefix("Vendor:").and_then(trailing_hex))?;
    let device = text.lines().find_map(|l| l.trim().strip_prefix("Device:").and_then(trailing_hex))?;
    let version_line = text
        .lines()
        .find_map(|l| l.strip_prefix("OpenGL core profile version string:"))
        .or_else(|| text.lines().find_map(|l| l.strip_prefix("OpenGL version string:")))?
        .trim();
    let version = version_line.split_ascii_whitespace().next()?.to_string();
    // "4.6 (Core Profile) Mesa 25.2.8-…" → "Mesa 25.2.8-…"
    let driver = version_line
        .rsplit_once(')')
        .map(|(_, rest)| rest.trim())
        .filter(|rest| !rest.is_empty())
        .or_else(|| version_line.split_once(' ').map(|(_, rest)| rest.trim()))
        .unwrap_or("")
        .to_string();
    // "AMD Radeon Graphics (radeonsi, gfx1201, …)" → radeonsi
    let renderer_driver = text
        .lines()
        .find_map(|l| l.strip_prefix("OpenGL renderer string:"))
        .and_then(|renderer| renderer.split_once('(').map(|(_, rest)| rest))
        .and_then(|rest| rest.split([',', ')']).next())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty() && !name.contains(' '));
    let driver = match renderer_driver {
        Some(name) if !driver.contains(&name) => format!("{driver} ({name})").trim().to_string(),
        _ => driver,
    };
    Some((vendor, device, ApiDriver { version, driver }))
}

/// `pactl info` → "PipeWire 1.0.5 (PulseAudio 15.0.0 API)" or
/// "PulseAudio 16.1".
pub fn audio_server(pactl_info: &str) -> Option<String> {
    let name = pactl_info.lines().find_map(|l| l.strip_prefix("Server Name:"))?.trim();
    let version = pactl_info
        .lines()
        .find_map(|l| l.strip_prefix("Server Version:"))
        .map(str::trim)
        .unwrap_or("");
    if let Some(start) = name.find("(on ") {
        let inner = name[start + 4..].trim_end_matches(')').trim();
        let api = if version.is_empty() {
            "PulseAudio API".to_string()
        } else {
            format!("PulseAudio {version} API")
        };
        return Some(format!("{inner} ({api})"));
    }
    Some(format!("{name} {version}").trim().to_string())
}

/// /proc/asound/cards → the card names, in index order.
pub fn alsa_cards(cards: &str) -> Vec<String> {
    cards
        .lines()
        .filter(|line| line.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit()))
        .filter_map(|line| line.split_once("]: ").map(|(_, rest)| rest))
        .filter_map(|rest| rest.split_once(" - ").map(|(_, name)| name.trim().to_string()))
        .filter(|name| !name.is_empty())
        .collect()
}

/// The parts of an EDID a spec sheet uses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edid {
    /// Three-letter PNP manufacturer id ("HWP").
    pub manufacturer_id: String,
    pub name: Option<String>,
    pub width_cm: u32,
    pub height_cm: u32,
    /// The preferred (first detailed) timing: width, height, Hz.
    pub preferred: Option<(u32, u32, f32)>,
}

pub fn parse_edid(bytes: &[u8]) -> Option<Edid> {
    if bytes.len() < 128 || bytes[0..8] != [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00] {
        return None;
    }
    let packed = u16::from_be_bytes([bytes[8], bytes[9]]);
    let letter = |shift: u16| (((packed >> shift) & 0x1f) as u8 + b'A' - 1) as char;
    let manufacturer_id: String = [letter(10), letter(5), letter(0)].iter().collect();
    let mut edid = Edid {
        manufacturer_id,
        width_cm: bytes[21] as u32,
        height_cm: bytes[22] as u32,
        ..Default::default()
    };
    for descriptor in bytes[54..126].chunks(18) {
        let pixel_clock = u16::from_le_bytes([descriptor[0], descriptor[1]]) as u32;
        if pixel_clock != 0 {
            if edid.preferred.is_none() {
                let width = descriptor[2] as u32 | ((descriptor[4] as u32 & 0xf0) << 4);
                let h_blank = descriptor[3] as u32 | ((descriptor[4] as u32 & 0x0f) << 8);
                let height = descriptor[5] as u32 | ((descriptor[7] as u32 & 0xf0) << 4);
                let v_blank = descriptor[6] as u32 | ((descriptor[7] as u32 & 0x0f) << 8);
                let total = (width + h_blank) * (height + v_blank);
                if total > 0 {
                    let hz = pixel_clock as f64 * 10_000.0 / total as f64;
                    edid.preferred = Some((width, height, hz as f32));
                }
            }
        } else if descriptor[3] == 0xfc {
            let name: String = descriptor[5..18]
                .iter()
                .take_while(|&&b| b != 0x0a)
                .map(|&b| b as char)
                .collect();
            let name = name.trim().to_string();
            if !name.is_empty() {
                edid.name = Some(name);
            }
        }
    }
    Some(edid)
}

/// hwdata's pnp.ids (`HWP\tHewlett Packard`) lookup.
pub fn pnp_vendor(pnp_ids: &str, id: &str) -> Option<String> {
    pnp_ids
        .lines()
        .find_map(|line| line.strip_prefix(id)?.strip_prefix('\t'))
        .map(|name| name.trim().to_string())
}

/// Screen diagonal in inches from the EDID's physical size.
pub fn diagonal_inches(width_cm: u32, height_cm: u32) -> Option<f32> {
    (width_cm > 0 && height_cm > 0)
        .then(|| ((width_cm * width_cm + height_cm * height_cm) as f32).sqrt() / 2.54)
}

/// Firmware dates are MM/DD/YYYY; spec sheets read better in ISO.
pub fn iso_date(dmi_date: &str) -> String {
    let parts: Vec<&str> = dmi_date.trim().split('/').collect();
    match parts.as_slice() {
        [month, day, year] if year.len() == 4 => format!("{year}-{month:0>2}-{day:0>2}"),
        _ => dmi_date.trim().to_string(),
    }
}

/// DMI fields boards ship unfilled.
pub fn dmi_placeholder(value: &str) -> bool {
    let value = value.trim();
    value.is_empty()
        || [
            "To Be Filled By O.E.M.",
            "To be filled by O.E.M.",
            "Default string",
            "System Product Name",
            "System manufacturer",
            "System Version",
            "Not Applicable",
            "Not Specified",
            "OEM",
            "O.E.M.",
            "Type1ProductConfigId",
        ]
        .contains(&value)
}

/// The kernel's udev export (`E:KEY=value`).
pub fn udev_value(udev: &str, key: &str) -> Option<String> {
    udev.lines()
        .find_map(|line| line.strip_prefix("E:")?.strip_prefix(key)?.strip_prefix('='))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_and_upstream() {
        let os = "NAME=\"Linux Mint\"\nPRETTY_NAME=\"Linux Mint 22.3\"\nID=linuxmint\n";
        assert_eq!(shell_value(os, "PRETTY_NAME").as_deref(), Some("Linux Mint 22.3"));
        let lsb = "DISTRIB_ID=Ubuntu\nDISTRIB_RELEASE=24.04\nDISTRIB_CODENAME=noble\n";
        assert_eq!(upstream_release(lsb).as_deref(), Some("Ubuntu 24.04"));
        assert_eq!(shell_value("PRETTY_NAME=\"\"\n", "PRETTY_NAME"), None);
    }

    #[test]
    fn cache_sizes_read_the_sysfs_suffixes() {
        assert_eq!(cache_size_bytes("32K"), Some(32 * 1024));
        assert_eq!(cache_size_bytes("32768K"), Some(32 << 20));
        assert_eq!(cache_size_bytes("8M"), Some(8 << 20));
        assert_eq!(cache_size_bytes("junk"), None);
    }

    #[test]
    fn cpu_features_name_what_spec_pages_name() {
        // 5950X flags (excerpt, real order)
        let flags = "fpu vme sse4_2 movbe popcnt aes xsave avx f16c rdrand lahf_lm svm fma bmi1 avx2 sha_ni";
        assert_eq!(cpu_features(flags), ["AVX2", "FMA3", "AES-NI", "SHA", "AMD-V"]);
        assert_eq!(cpu_features("vmx avx512f avx2"), ["AVX-512", "AVX2", "VT-x"]);
    }

    #[test]
    fn pcie_speeds_map_to_generations() {
        assert_eq!(pcie_generation("32.0 GT/s PCIe"), Some(5));
        assert_eq!(pcie_generation("16.0 GT/s PCIe"), Some(4));
        assert_eq!(pcie_generation("8.0 GT/s PCIe"), Some(3));
        assert_eq!(pcie_generation("2.5 GT/s PCIe"), Some(1));
        assert_eq!(pcie_generation("Unknown"), None);
    }

    #[test]
    fn gfx_targets_and_families() {
        assert_eq!(gfx_target(120001).as_deref(), Some("gfx1201"));
        assert_eq!(gfx_target(110000).as_deref(), Some("gfx1100"));
        assert_eq!(gfx_target(90010).as_deref(), Some("gfx90a"));
        assert_eq!(gfx_target(0), None);
        assert_eq!(amd_family("gfx1201"), Some("RDNA 4"));
        assert_eq!(amd_family("gfx1030"), Some("RDNA 2"));
        assert_eq!(amd_family("gfx90a"), Some("CDNA 2"));
        assert!(amd_family("gfx1200").is_some());
        assert_eq!(amd_family("gfx703"), None, "an unlisted ISA claims no family");
    }

    #[test]
    fn memory_slots_come_from_the_array() {
        assert_eq!(dmi_memory_slots("E:MEMORY_ARRAY_NUM_DEVICES=4\n"), Some(4));
        assert_eq!(dmi_memory_slots("E:MEMORY_ARRAY_NUM_DEVICES=0\n"), None);
        assert_eq!(dmi_memory_slots(""), None);
    }

    // Ryzen 9 5950X + Radeon AI PRO R9700, Mesa 25.2.8 (2026-10-09).
    const VULKAN_SUMMARY: &str = "\
==========
VULKANINFO
==========

Vulkan Instance Version: 1.3.275

Devices:
========
GPU0:
\tapiVersion         = 1.4.318
\tdriverVersion      = 25.2.8
\tvendorID           = 0x1002
\tdeviceID           = 0x7551
\tdeviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
\tdeviceName         = AMD Radeon Graphics (RADV GFX1201)
\tdriverID           = DRIVER_ID_MESA_RADV
\tdriverName         = radv
\tdriverInfo         = Mesa 25.2.8-0ubuntu0.24.04.2
GPU1:
\tapiVersion         = 1.4.318
\tdriverVersion      = 25.2.8
\tvendorID           = 0x10005
\tdeviceID           = 0x0000
\tdeviceType         = PHYSICAL_DEVICE_TYPE_CPU
\tdeviceName         = llvmpipe (LLVM 20.1.2, 256 bits)
\tdriverName         = llvmpipe
\tdriverInfo         = Mesa 25.2.8-0ubuntu0.24.04.2 (LLVM 20.1.2)
";

    #[test]
    fn vulkan_devices_keep_the_device_api_not_the_loader() {
        let devices = parse_vulkan_summary(VULKAN_SUMMARY);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].vendor_id, 0x1002);
        assert_eq!(devices[0].device_id, 0x7551);
        assert!(devices[0].physical);
        assert_eq!(devices[0].api_version, "1.4.318", "not the 1.3.275 loader");
        assert_eq!(devices[0].driver, "Mesa 25.2.8-0ubuntu0.24.04.2 (RADV)");
        assert!(!devices[1].physical, "llvmpipe is a software renderer, not a GPU");
        assert_eq!(driver_label("NVIDIA", "560.35.03"), "NVIDIA 560.35.03");
    }

    #[test]
    fn glxinfo_ties_the_context_to_its_card() {
        let text = "\
Extended renderer info (GLX_MESA_query_renderer):
    Vendor: AMD (0x1002)
    Device: AMD Radeon Graphics (radeonsi, gfx1201, LLVM 20.1.2, DRM 3.64, 7.2.9-cachyos-nexus) (0x7551)
    Version: 25.2.8
OpenGL vendor string: AMD
OpenGL renderer string: AMD Radeon Graphics (radeonsi, gfx1201, LLVM 20.1.2, DRM 3.64, 7.2.9-cachyos-nexus)
OpenGL core profile version string: 4.6 (Core Profile) Mesa 25.2.8-0ubuntu0.24.04.2
OpenGL version string: 4.6 (Compatibility Profile) Mesa 25.2.8-0ubuntu0.24.04.2
";
        let (vendor, device, gl) = parse_glxinfo(text).expect("parsed");
        assert_eq!((vendor, device), (0x1002, 0x7551));
        assert_eq!(gl.version, "4.6");
        assert_eq!(gl.driver, "Mesa 25.2.8-0ubuntu0.24.04.2 (radeonsi)");
        assert_eq!(parse_glxinfo("OpenGL version string: 4.6\n"), None, "no ids, no card");
    }

    #[test]
    fn audio_server_and_cards() {
        let pactl = "Server Name: PulseAudio (on PipeWire 1.0.5)\nServer Version: 15.0.0\n";
        assert_eq!(audio_server(pactl).as_deref(), Some("PipeWire 1.0.5 (PulseAudio 15.0.0 API)"));
        assert_eq!(
            audio_server("Server Name: pulseaudio\nServer Version: 16.1\n").as_deref(),
            Some("pulseaudio 16.1")
        );
        let cards = " 0 [HDMI           ]: HDA-Intel - HDA ATI HDMI
                      HDA ATI HDMI at 0xfcda0000 irq 81
 1 [Microphone     ]: USB-Audio - Samson Q2U Microphone
                      Samson Technologies Samson Q2U Microphone at usb-0000:04:00.1-1, full speed
";
        assert_eq!(alsa_cards(cards), ["HDA ATI HDMI", "Samson Q2U Microphone"]);
    }

    #[test]
    fn edid_names_the_panel_and_its_size() {
        // HP X32, read from /sys/class/drm/card1-DP-1/edid (first block).
        let mut edid = vec![0u8; 128];
        edid[0..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        edid[8..10].copy_from_slice(&[0x22, 0x0e]); // HPN, HP Inc.
        edid[21] = 70;
        edid[22] = 39;
        // 2560×1440 detailed timing, pixel clock 0x5e56 ×10 kHz
        edid[54..72].copy_from_slice(&[
            0x56, 0x5e, 0x00, 0xa0, 0xa0, 0xa0, 0x29, 0x50, 0x30, 0x20, 0x35, 0x00, 0xb9, 0x88, 0x21, 0x00, 0x00, 0x1a,
        ]);
        let mut name = [0u8; 18];
        name[3] = 0xfc;
        name[5..18].copy_from_slice(b"HP X32\n      ");
        edid[72..90].copy_from_slice(&name);
        let parsed = parse_edid(&edid).expect("valid edid");
        assert_eq!(parsed.manufacturer_id, "HPN");
        assert_eq!(parsed.name.as_deref(), Some("HP X32"));
        let (width, height, hz) = parsed.preferred.unwrap();
        assert_eq!((width, height), (2560, 1440));
        assert!((hz - 59.95).abs() < 0.1, "{hz}");
        let inches = diagonal_inches(parsed.width_cm, parsed.height_cm).unwrap();
        assert!((inches - 31.5).abs() < 0.2, "{inches}");
        assert_eq!(parse_edid(&[0u8; 64]), None);
        assert_eq!(pnp_vendor("HWP\tHewlett Packard\nHPN\tHP Inc.\n", "HWP").as_deref(), Some("Hewlett Packard"));
    }

    #[test]
    fn dmi_helpers() {
        assert_eq!(iso_date("09/07/2026"), "2026-09-07");
        assert_eq!(iso_date("2026"), "2026");
        assert!(dmi_placeholder("To Be Filled By O.E.M."));
        assert!(!dmi_placeholder("ASRock"));
        let udev = "E:ID_MODEL_FROM_DATABASE=I211 Gigabit Network Connection\nE:ID_BUS=pci\n";
        assert_eq!(udev_value(udev, "ID_MODEL_FROM_DATABASE").as_deref(), Some("I211 Gigabit Network Connection"));
        assert_eq!(udev_value(udev, "ID_MODEL"), None, "prefix of another key is not that key");
    }
}
