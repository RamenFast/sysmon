// SPDX-License-Identifier: GPL-3.0-or-later
//! The snapshot model — one struct tree for everything SysMon knows.
//!
//! These types ARE the wire contract: `sysmon probe` serializes them
//! verbatim, `sysmon schema` documents them, the GUI renders them.
//! Field names carry their unit (`_bytes`, `_bps`, `_percent`,
//! `_celsius`, `_mhz`, `_rpm`, `_seconds`) so an API consumer never
//! has to guess. Rates are measured over `interval_seconds`; on the
//! very first sample of a fresh sampler every rate is 0 and
//! `interval_seconds` is 0.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Everything, one tick. Sections a caller didn't ask for are `None`
/// (and skipped in JSON) — `sysmon probe network` really only pays
/// for the network.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SystemSnapshot {
    /// Unix time of this sample, seconds.
    pub ts: f64,
    /// Measurement window behind every `*_bps` / `*_percent` rate.
    pub interval_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<CpuSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySnapshot>,
    /// `None` when the GPU section wasn't requested; `Some(None)`
    /// flattens away — absence of an AMD GPU is reported inside.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disks: Option<Vec<DiskSnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<Vec<ProcessRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensors: Option<SensorsSnapshot>,
    /// Socket-level connection table (own-UID processes; TCP + UDP).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connections: Option<Vec<ConnectionRecord>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SystemInfo {
    pub hostname: String,
    pub kernel: String,
    pub uptime_seconds: f64,
    /// Unix time the machine booted.
    pub boot_ts: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CpuSnapshot {
    /// Busy share of the window, 0–100: every tick except idle and
    /// iowait (htop's identity — iowait is waiting, not working).
    pub overall_percent: f32,
    /// Share of the window spent idle *waiting on storage*, 0–100.
    /// mpstat's %iowait; reported apart so "100 − busy" isn't a
    /// mystery when a disk is slow.
    #[serde(default)]
    pub iowait_percent: f32,
    /// Share of the window the kernel itself was working (system +
    /// irq + softirq), 0–100. Inside `overall_percent`, never above
    /// it. mpstat's %sys + %irq + %soft. New in 3.2.
    #[serde(default)]
    pub kernel_percent: f32,
    pub per_core_percent: Vec<f32>,
    pub core_count: usize,
    /// Mean of the per-core current frequencies, idle cores included
    /// (cpufreq reports a parked core at its last requested clock).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_mhz: Option<f64>,
    /// The clock the *working* cores run at: per-core frequencies
    /// weighted by each core's busy share over the window. The number
    /// to compare with turbostat's Bzy_MHz; None until a window exists
    /// or when every core idled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_busy_mhz: Option<f64>,
    /// How the busy clock was measured: "delivered over the window"
    /// (ACPI CPPC counters, the APERF/MPERF ratio turbostat reads) or
    /// "instant read" (scaling_cur_freq; no CPPC, or the first window).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_busy_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_min_mhz: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_max_mhz: Option<f64>,
    pub load_1m: f64,
    pub load_5m: f64,
    pub load_15m: f64,
    /// Processes in R state right now (loadavg field 4 numerator).
    pub tasks_running: u32,
    /// Total kernel task count (loadavg field 4 denominator).
    pub tasks_total: u32,
    /// Package temperature (k10temp Tctl on Ryzen), when a CPU
    /// hwmon chip exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_celsius: Option<f32>,
    /// Which sensor fed `temperature_celsius` ("k10temp Tctl").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_source: Option<String>,
    /// Context switches per second across the machine.
    pub context_switches_per_second: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemorySnapshot {
    /// MemTotal: what the kernel can hand out — installed RAM minus
    /// firmware reservations and the kernel's own image.
    pub total_bytes: u64,
    /// `total - available` — the figure that answers "how much is my
    /// RAM actually committed" (psutil/v1 definition, NOT htop's).
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub free_bytes: u64,
    /// Cached + SReclaimable (what /proc/meminfo calls reclaimable
    /// page cache; matches psutil's `cached + buffers` neighborhood —
    /// see ACCURACY.md for the exact identity).
    pub cached_bytes: u64,
    pub buffers_bytes: u64,
    pub dirty_bytes: u64,
    pub shared_bytes: u64,
    pub used_percent: f32,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_cached_bytes: u64,
    /// Committed_AS: every byte of address space the kernel has
    /// promised to processes. The XP "commit charge". New in 3.2.
    #[serde(default)]
    pub committed_bytes: u64,
    /// CommitLimit: what the overcommit policy would let
    /// `committed_bytes` reach (RAM × ratio + swap). On the default
    /// heuristic policy the kernel does not enforce it, so committed
    /// may legitimately exceed it. New in 3.2.
    #[serde(default)]
    pub commit_limit_bytes: u64,
    /// Slab: every kernel slab cache, reclaimable and not (the
    /// reclaimable part is also inside `cached_bytes`). New in 3.2.
    #[serde(default)]
    pub slab_bytes: u64,
    /// KernelStack. New in 3.2.
    #[serde(default)]
    pub kernel_stack_bytes: u64,
    /// PageTables. New in 3.2.
    #[serde(default)]
    pub page_tables_bytes: u64,
    /// Sum of the installed memory modules (SMBIOS via udev) — the
    /// number on the box, in bytes. None when firmware tables are
    /// unreadable (VMs, containers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_bytes: Option<u64>,
    /// One entry per populated slot, from the same SMBIOS tables.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub modules: Vec<MemoryModule>,
}

/// One installed memory stick (SMBIOS type 17, via udev's DMI data).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryModule {
    /// Slot name the board prints ("DIMM 0").
    pub locator: String,
    pub size_bytes: u64,
    /// "DDR4", "DDR5", …
    pub kind: String,
    /// The speed the module is running at right now, MT/s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_speed_mts: Option<u32>,
    /// The fastest speed the module advertises to firmware, MT/s
    /// (JEDEC; an XMP/EXPO profile can exceed it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rated_speed_mts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_number: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GpuSnapshot {
    /// False on machines without an amdgpu card; every other field is
    /// then zero/None and the UI shows its gentle empty state.
    pub available: bool,
    pub device_name: String,
    pub busy_percent: f32,
    pub vram_used_bytes: u64,
    pub vram_total_bytes: u64,
    /// GTT = system RAM the GPU has mapped (spillover indicator).
    pub gtt_used_bytes: u64,
    pub gtt_total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_edge_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_junction_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_memory_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_draw_watts: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_cap_watts: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_clock_mhz: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_clock_mhz: Option<f64>,
    /// How the clocks were measured: "mean of N reads" (sampled across
    /// the window — the honest figure for a clock that moves every few
    /// ms) or "instant read" (one read; the window's first sample).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock_source: Option<String>,
    /// Voltage-regulator temperatures (graphics, SoC, memory rails),
    /// from gpu_metrics; hwmon doesn't expose them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_vrm_gfx_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_vrm_soc_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_vrm_mem_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_rpm: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_max_rpm: Option<u32>,
    /// Processes holding DRM fds on this card, busiest first.
    pub processes: Vec<GpuProcessUsage>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GpuProcessUsage {
    pub pid: i32,
    pub busy_percent: f32,
    pub vram_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NetworkSnapshot {
    /// Physical interfaces only (v1 semantics): what the WAN sees.
    pub download_bps: f64,
    pub upload_bps: f64,
    pub total_received_bytes: u64,
    pub total_sent_bytes: u64,
    pub interfaces: Vec<InterfaceSnapshot>,
    /// Where per-process rates come from right now.
    pub process_source: ProcessNetSource,
    /// Human hint when the source is degraded ("TCP sockets only —
    /// install nethogs for UDP/QUIC coverage: …").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_source_hint: Option<String>,
    /// Busiest processes on the network right now (rx+tx desc) —
    /// answers the bar's click without a full process scan.
    pub top_processes: Vec<ProcessNetTopEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProcessNetTopEntry {
    /// -1 when the owning process isn't resolvable (another user's,
    /// without nethogs).
    pub pid: i32,
    pub name: String,
    pub rx_bps: f64,
    pub tx_bps: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessNetSource {
    /// nethogs trace merge: packet truth, all protocols, all users.
    Nethogs,
    /// Native netlink sock_diag: TCP byte counters, own-UID sockets.
    #[default]
    TcpDiag,
    /// Neither worked (no CONFIG_INET_DIAG and no nethogs).
    None,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InterfaceSnapshot {
    pub name: String,
    pub kind: InterfaceKind,
    pub is_up: bool,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub rx_total_bytes: u64,
    pub tx_total_bytes: u64,
    /// Link speed when the driver reports one (ethtool sysfs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_mbps: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceKind {
    #[default]
    Physical,
    Loopback,
    Virtual,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiskSnapshot {
    pub device: String,
    pub mount_point: String,
    /// Filesystem label when one exists, else the device basename.
    pub display_name: String,
    pub fs_type: String,
    pub read_bps: f64,
    pub write_bps: f64,
    pub used_bytes: u64,
    pub total_bytes: u64,
    /// Fraction of the window the underlying disk was busy, 0–100.
    pub util_percent: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub pid: i32,
    pub ppid: i32,
    /// comm — kernel-truncated to 15 chars; `command_line` has the
    /// full story for user processes.
    pub name: String,
    /// What a person calls this process. Usually `name`; when comm is
    /// a thread name ("MainThread") or a bare interpreter ("python3",
    /// "node") it names the program instead ("dsh", "tray.py").
    #[serde(default)]
    pub display_name: String,
    pub user: String,
    /// Single-letter kernel state plus the word ("S", "sleeping").
    pub state: String,
    pub state_word: String,
    pub is_kernel_thread: bool,
    pub cpu_percent: f32,
    pub memory_rss_bytes: u64,
    pub memory_virtual_bytes: u64,
    pub threads: u32,
    pub nice: i32,
    /// Unix time the process started.
    pub started_ts: f64,
    /// Cumulative CPU seconds (utime+stime).
    pub cpu_time_seconds: f64,
    /// None when /proc/pid/io is unreadable (other users' processes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_read_bps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_write_bps: Option<f64>,
    pub gpu_busy_percent: f32,
    pub gpu_vram_bytes: u64,
    /// None when no per-process source covers this pid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net_rx_bps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net_tx_bps: Option<f64>,
    pub command_line: String,
    /// Basename of /proc/pid/exe when readable (icon lookup key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe_basename: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SensorsSnapshot {
    pub chips: Vec<SensorChip>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery: Option<BatterySnapshot>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SensorChip {
    /// hwmon driver name (k10temp, nvme, amdgpu, …).
    pub name: String,
    /// What the chip watches: cpu | gpu | drive | board | other.
    #[serde(default)]
    pub kind: SensorKind,
    /// Block device the chip measures (drivetemp/nvme chips), e.g.
    /// "sda" — without it, four SATA drives all read "drivetemp".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The measured device's model string, when it names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    pub temps: Vec<TempReading>,
    pub fans: Vec<FanReading>,
    /// Voltage rails (hwmon `in*` channels), volts.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub voltages: Vec<VoltageReading>,
    /// Power rails (hwmon `power*` channels), watts.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub power: Vec<PowerReading>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorKind {
    Cpu,
    Gpu,
    Drive,
    Board,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TempReading {
    /// The driver's label, or `tempN` when it gives none.
    pub label: String,
    pub celsius: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crit_celsius: Option<f32>,
    /// False for readings no working sensor produces: an unconnected
    /// thermistor input (−62 °C), a channel the board wires to nothing
    /// (exactly 0 °C), or past 150 °C. Kept on the wire, so nothing is
    /// hidden; left out of headlines and "hottest".
    #[serde(default = "default_true")]
    pub plausible: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FanReading {
    pub label: String,
    pub rpm: u32,
    /// The fan's rated maximum, when the driver states one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rpm: Option<u32>,
    /// What the board is driving the header at (pwmN, 0–100 %), when
    /// it exposes one. 0 rpm at a non-zero duty means a header with
    /// no tachometer signal — unplugged, a pump on a fan header, or a
    /// fan that has stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duty_percent: Option<f32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VoltageReading {
    pub label: String,
    pub volts: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PowerReading {
    pub label: String,
    pub watts: f32,
    /// The enforced power limit, when the driver states one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap_watts: Option<f32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BatterySnapshot {
    pub name: String,
    pub percent: f32,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_draw_watts: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds_remaining: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionRecord {
    /// -1 when the owning process couldn't be resolved (other user).
    pub pid: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    pub protocol: String,
    pub local_address: String,
    pub remote_address: String,
    pub state: String,
    /// TCP only — byte-counter deltas over the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rx_bps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_bps: Option<f64>,
}

impl SystemSnapshot {
    /// The one way a snapshot becomes JSON, for every producer (probe,
    /// the socket's snapshot and subscribe, serve, the GUI backend).
    ///
    /// Through the typed serializer to text first, then parsed: going
    /// straight through `serde_json::to_value` widens every f32 to f64,
    /// so a reading of `1.5370705` came out as `1.5370705127716064`
    /// (3.1 audit F19). Parsing the shortest f32 text back as an f64
    /// re-prints the same digits, so every reading keeps its bytes.
    pub fn to_json_value(&self) -> serde_json::Result<serde_json::Value> {
        serde_json::from_str(&serde_json::to_string(self)?)
    }

    /// The busiest processes by a key, ready for the top-3 lists.
    pub fn top_processes_by<F>(&self, count: usize, minimum: f64, value: F) -> Vec<&ProcessRecord>
    where
        F: Fn(&ProcessRecord) -> f64,
    {
        let Some(processes) = &self.processes else {
            return Vec::new();
        };
        let mut candidates: Vec<&ProcessRecord> =
            processes.iter().filter(|p| value(p) > minimum).collect();
        candidates.sort_by(|a, b| {
            value(b)
                .partial_cmp(&value(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(count);
        candidates
    }
}

/// Which sections a sample should collect — `sysmon tap network`
/// never pays for a full process scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wants {
    pub system: bool,
    pub cpu: bool,
    pub memory: bool,
    pub gpu: bool,
    pub network: bool,
    pub disks: bool,
    pub processes: bool,
    pub sensors: bool,
    /// Per-process network attribution (implies a light pid scan).
    pub per_process_net: bool,
    /// The full socket table.
    pub connections: bool,
}

impl Wants {
    pub fn all() -> Self {
        Wants {
            system: true,
            cpu: true,
            memory: true,
            gpu: true,
            network: true,
            disks: true,
            processes: true,
            sensors: true,
            per_process_net: true,
            connections: false, // big; explicitly requested only
        }
    }

    pub fn none() -> Self {
        Wants::default()
    }

    /// Section names accepted by `probe`/`tap`/`subscribe`.
    pub fn from_section_name(name: &str) -> Option<Self> {
        let mut wants = Wants::none();
        match name {
            "system" => wants.system = true,
            "cpu" => wants.cpu = true, // its temperature reads the CPU chip alone
            "memory" => wants.memory = true,
            "gpu" => wants.gpu = true,
            "network" => {
                wants.network = true;
                wants.per_process_net = true;
            }
            "disks" => wants.disks = true,
            "processes" => {
                wants.processes = true;
                wants.gpu = true; // gpu% column
                wants.per_process_net = true;
            }
            "sensors" => wants.sensors = true,
            "connections" => {
                wants.connections = true;
                wants.per_process_net = true;
            }
            "all" => wants = Wants::all(),
            _ => return None,
        }
        Some(wants)
    }

    pub fn union(self, other: Wants) -> Wants {
        Wants {
            system: self.system || other.system,
            cpu: self.cpu || other.cpu,
            memory: self.memory || other.memory,
            gpu: self.gpu || other.gpu,
            network: self.network || other.network,
            disks: self.disks || other.disks,
            processes: self.processes || other.processes,
            sensors: self.sensors || other.sensors,
            per_process_net: self.per_process_net || other.per_process_net,
            connections: self.connections || other.connections,
        }
    }
}

/// pid → (rx_bps, tx_bps) merge map produced by the per-process
/// network sources.
pub type ProcessNetRates = HashMap<i32, (f64, f64)>;
