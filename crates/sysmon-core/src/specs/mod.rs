// SPDX-License-Identifier: GPL-3.0-or-later
//! System specs: what this machine *is*, as opposed to what it is
//! doing. The live snapshot answers "how busy"; this answers "which
//! CPU, which board, which drives, which driver stack" — the sheet a
//! person pastes into a forum post or a bug report.
//!
//! Collected on demand (a click, `sysmon specs`), never per frame:
//! a few optional helpers (vulkaninfo, glxinfo, the compositor's IPC,
//! pactl) are run with a deadline off the UI thread. Everything is
//! read-only and unprivileged. A source that is missing leaves its
//! field `None` — never a guess, never a zero standing in for
//! "unknown". A whole section that cannot be read is named in
//! `unavailable` with the reason, so a short sheet is never mistaken
//! for a complete one.
//!
//! Privacy is a formatting decision, not a collection one: the JSON
//! carries mount points and the hostname for local agents; the shared
//! text (`text.rs`) leaves out the hostname, serials, addresses and
//! private mount paths.

mod collect;
pub mod parse;
pub mod run;
pub mod text;

use serde::{Deserialize, Serialize};

use crate::snapshot::MemoryModule;

pub use collect::collect;

/// Section names `sysmon specs` and the copy buttons accept, in sheet
/// order.
pub const SPEC_SECTIONS: [&str; 7] =
    ["system", "cpu", "memory", "graphics", "storage", "network", "audio"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SystemSpecs {
    /// Unix time the specs were read.
    pub ts: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<CpuSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySpec>,
    pub gpus: Vec<GpuSpec>,
    pub displays: Vec<DisplaySpec>,
    pub drives: Vec<DriveSpec>,
    pub network: Vec<NicSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioSpec>,
    /// Sections that could not be read at all, with why.
    pub unavailable: Vec<Unavailable>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Unavailable {
    pub section: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SystemSpec {
    /// os-release PRETTY_NAME ("Linux Mint 22.3").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// The distribution this one is built on, when it says
    /// ("Ubuntu 24.04 LTS" for Mint, from /etc/upstream-release).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_base: Option<String>,
    pub kernel: String,
    pub architecture: String,
    pub hostname: String,
    /// XDG_SESSION_TYPE: "wayland", "x11", "tty".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_type: Option<String>,
    /// The running desktop or compositor with its version, when it
    /// can be asked ("SwayFX 0.6-c4c8d51b (Sway 1.12.0)").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop: Option<String>,
    /// Prebuilt machines name themselves here ("LENOVO ThinkPad X1
    /// Carbon Gen 11"); a self-built desktop usually says nothing
    /// useful and the field is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware_date: Option<String>,
    /// "UEFI" or "BIOS".
    pub boot_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure_boot: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CpuSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Absent when sysfs topology is unreadable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sockets: Option<usize>,
    /// Physical cores (unique socket/core pairs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cores: Option<usize>,
    /// Logical CPUs online.
    pub threads: usize,
    /// The guaranteed clock (ACPI CPPC nominal frequency), when the
    /// firmware reports one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_mhz: Option<f64>,
    /// The highest clock cpufreq reports for any core.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_mhz: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boost_enabled: Option<bool>,
    /// Machine-wide totals per cache, each shared cache counted once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_l1d_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_l1i_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_l2_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_l3_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_driver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub governor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_preference: Option<String>,
    /// Instruction-set features a reader asks about ("AVX2", "AMD-V").
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemorySpec {
    pub modules: Vec<MemoryModule>,
    /// Slots the board has, when the firmware says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slots: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_bytes: Option<u64>,
    /// MemTotal: what the kernel can use.
    pub usable_bytes: u64,
    pub swap_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GpuSpec {
    pub name: String,
    /// "1002:7551".
    pub pci_id: String,
    pub pci_address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    /// amdgpu only: dedicated VRAM the driver reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_bytes: Option<u64>,
    /// AMD shader ISA ("gfx1201"), from the KFD topology.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    /// The family that ISA belongs to ("RDNA 4").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_units: Option<u32>,
    /// The link the card actually gets: the slowest hop between the
    /// card and the CPU. A PCIe 5.0 card in a 4.0 slot reports 5.0
    /// on its own port; the slot is what it runs at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcie_link: Option<PcieLink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_cap_watts: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_cap_max_watts: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vulkan: Option<ApiDriver>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opengl: Option<ApiDriver>,
    /// "ROCm 7.14.1" / "CUDA driver 560.35" — the compute stack
    /// installed for this vendor, when one is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_stack: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PcieLink {
    /// PCIe generation of the effective link (4 for 16 GT/s).
    pub generation: u32,
    pub width: u32,
    /// The device's own best link, when better than what it gets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_max_generation: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_max_width: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiDriver {
    /// The API version the device exposes ("1.4.318", "4.6").
    pub version: String,
    /// The driver behind it ("Mesa 25.2.8 (RADV)").
    pub driver: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DisplaySpec {
    /// Connector ("DP-1").
    pub connector: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Diagonal from the EDID's physical size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagonal_inches: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width_px: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_px: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_hz: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// "active" when the compositor reported the running mode;
    /// "preferred" when only the monitor's EDID could be read (the
    /// mode the panel prefers, which is not necessarily the one in
    /// use).
    pub mode_source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DriveSpec {
    /// Kernel name ("nvme0n1", "sda").
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// "NVMe SSD", "SATA SSD", "SATA HDD", "USB", "SD/eMMC", …
    pub kind: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    /// "PCIe 4.0 x4", "SATA 6.0 Gb/s".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub removable: bool,
    /// Filesystems on the drive (partitions, or the whole disk).
    pub filesystems: Vec<FilesystemSpec>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FilesystemSpec {
    /// Partition (or disk) kernel name.
    pub device: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fs_type: Option<String>,
    /// Every place it is mounted (btrfs subvolumes mount one device
    /// many times).
    pub mount_points: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NicSpec {
    pub name: String,
    /// "Ethernet", "Wi-Fi".
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    pub link_up: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_mbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplex: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AudioSpec {
    /// "PipeWire 1.0.5 (PulseAudio 15.0.0 protocol)".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// ALSA card names ("HDA ATI HDMI", "Samson Q2U Microphone").
    pub devices: Vec<String>,
}

impl SystemSpecs {
    pub fn to_json_value(&self) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(self)
    }
}
