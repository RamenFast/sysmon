// SPDX-License-Identifier: GPL-3.0-or-later
//! AMD GPU via the amdgpu driver's sysfs, per-process usage via DRM
//! fdinfo — the same sources nvtop and amdgpu_top read (and the same
//! logic v1 shipped: engine-time deltas, max across engines per DRM
//! client so graphics and compute both register without double
//! counting; processes with no DRM fds go into a negative cache and
//! are only re-checked every few samples).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::snapshot::{GpuProcessUsage, GpuSnapshot};

use super::read::{SelfInterval, read_trimmed, read_u64};

const AMD_PCI_VENDOR_ID: &str = "0x1002";
const DRM_CHAR_MAJOR: u32 = 226;
const NEGATIVE_CACHE_SAMPLES: u8 = 5;

const ENGINE_TIME_KEYS: [&str; 4] = [
    "drm-engine-gfx",
    "drm-engine-compute",
    "drm-engine-enc",
    "drm-engine-dec",
];

/// Parsed fdinfo: numeric first tokens become integers ("123 ns" →
/// 123), the rest stay strings.
pub fn parse_fdinfo(content: &str) -> HashMap<&str, FdinfoValue<'_>> {
    let mut fields = HashMap::new();
    for line in content.lines() {
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        let value = raw.trim();
        let first = value.split(' ').next().unwrap_or("");
        let parsed = match first.parse::<u64>() {
            Ok(number) => FdinfoValue::Number(number),
            Err(_) => FdinfoValue::Text(value),
        };
        fields.insert(key, parsed);
    }
    fields
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FdinfoValue<'a> {
    Number(u64),
    Text(&'a str),
}

impl FdinfoValue<'_> {
    fn number(&self) -> Option<u64> {
        match self {
            FdinfoValue::Number(n) => Some(*n),
            FdinfoValue::Text(_) => None,
        }
    }
    fn text(&self) -> Option<&str> {
        match self {
            FdinfoValue::Text(t) => Some(t),
            FdinfoValue::Number(_) => None,
        }
    }
}

/// (max engine-time ns across engines, vram bytes) for one fdinfo
/// blob on the given card, or None when it isn't a DRM client there.
pub fn drm_usage_from_fdinfo(
    content: &str,
    pci_address: &str,
    seen_client_ids: &mut HashSet<u64>,
) -> Option<(u64, u64)> {
    let fields = parse_fdinfo(content);
    if fields.get("drm-pdev").and_then(|v| v.text()) != Some(pci_address) {
        return None;
    }
    if let Some(client_id) = fields.get("drm-client-id").and_then(|v| v.number())
        && !seen_client_ids.insert(client_id) {
            return None; // duplicate fd for the same client (dup/fork)
        }
    let engine_ns = ENGINE_TIME_KEYS
        .iter()
        .filter_map(|key| fields.get(key).and_then(|v| v.number()))
        .max()
        .unwrap_or(0);
    let vram_kib = fields
        .get("drm-memory-vram")
        .or_else(|| fields.get("drm-total-vram"))
        .and_then(|v| v.number())
        .unwrap_or(0);
    Some((engine_ns, vram_kib * 1024))
}

/// What `gpu_metrics` adds over hwmon: the firmware's own *averaged*
/// clocks (hwmon's freq1_input is one instantaneous read that swings
/// 0..1558 MHz between reads — 3.1 audit F15) and the VRM temperatures
/// hwmon doesn't expose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuMetrics {
    pub average_gfxclk_mhz: Option<u16>,
    pub average_uclk_mhz: Option<u16>,
    pub temperature_vrgfx_celsius: Option<u16>,
    pub temperature_vrsoc_celsius: Option<u16>,
    pub temperature_vrmem_celsius: Option<u16>,
}

/// Parse a discrete-GPU `gpu_metrics` table, format 1 content 1–3
/// (`struct gpu_metrics_v1_1..v1_3`, kgd_pp_interface.h). They share
/// the prefix this reads: header (4), six u16 temperatures at 0x04,
/// four u16 activity/power, u64 energy, u64 clock counter, then the
/// u16 average clocks at 0x28. v1.0 orders that prefix differently,
/// and format 2/3 are APU tables; every unknown layout is refused
/// rather than guessed. 0xFFFF is the firmware's "not supported".
pub fn parse_gpu_metrics(blob: &[u8]) -> Option<GpuMetrics> {
    let u16_at = |offset: usize| -> Option<u16> {
        let bytes = blob.get(offset..offset + 2)?;
        let value = u16::from_le_bytes([bytes[0], bytes[1]]);
        (value != u16::MAX).then_some(value)
    };
    let structure_size = u16::from_le_bytes([*blob.first()?, *blob.get(1)?]) as usize;
    let (format, content) = (*blob.get(2)?, *blob.get(3)?);
    if format != 1 || !(1..=3).contains(&content) || structure_size > blob.len() {
        return None;
    }
    // Everything read below ends at 0x2e; a table shorter than that is
    // not one of these layouts.
    if blob.len() < 0x2e {
        return None;
    }
    Some(GpuMetrics {
        temperature_vrgfx_celsius: u16_at(0x0a),
        temperature_vrsoc_celsius: u16_at(0x0c),
        temperature_vrmem_celsius: u16_at(0x0e),
        average_gfxclk_mhz: u16_at(0x28),
        average_uclk_mhz: u16_at(0x2c),
    })
}

/// Averages the GPU clocks across each sample window (G8).
///
/// RDNA4's `gpu_metrics` "average" clock is the current clock, and it
/// moves every few milliseconds between 0 (gated) and boost, so any
/// single read, hwmon's included, is a coin toss. A thread reads the
/// table 5×/s and keeps running sums; `take` hands back the window's
/// mean and how many reads made it (a 2 s GUI tick gets 10).
///
/// Why 5/s: every amdgpu sysfs read wakes the SMU firmware once it has
/// idled, ~0.8 ms each (measured 2026-10-01; 16 µs only when reads come
/// back to back). Against an independent 100 Hz poll, 20/s reached
/// 0.09 σ RMS error at 1.6% of a core; 5/s reaches 0.27 σ at 0.4%.
/// One read alone is ~1 σ off. The thread only runs while someone
/// samples: no `take` for 5 s and it parks, with no timer, until the
/// next `take` or the collector's drop, so an idle `sysmon serve`
/// costs nothing. A window that outlived the park is refused (W3):
/// its reads would cover only its first 5 s.
///
/// The last parsed table is kept too, so the VRM temperatures cost no
/// second SMU wake on the sampling path.
struct ClockPoller {
    shared: std::sync::Arc<(std::sync::Mutex<ClockSums>, std::sync::Condvar)>,
    idle_after: std::time::Duration,
}

#[derive(Default)]
struct ClockSums {
    gfx_sum: f64,
    uclk_sum: f64,
    gfx_reads: u32,
    uclk_reads: u32,
    last_take: Option<std::time::Instant>,
    latest: Option<GpuMetrics>,
    closed: bool,
    /// Loop turns of the poller thread, for the idle-cost test.
    turns: u64,
}

impl ClockPoller {
    const PERIOD: std::time::Duration = std::time::Duration::from_millis(200);
    const IDLE_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

    fn spawn(metrics_path: PathBuf) -> Option<ClockPoller> {
        Self::spawn_with(metrics_path, Self::PERIOD, Self::IDLE_AFTER)
    }

    fn spawn_with(metrics_path: PathBuf, period: std::time::Duration, idle_after: std::time::Duration) -> Option<ClockPoller> {
        let shared = std::sync::Arc::new((std::sync::Mutex::new(ClockSums::default()), std::sync::Condvar::new()));
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("sysmon-gpu-clock".to_string())
            .spawn(move || {
                let (lock, wake) = &*worker;
                loop {
                    let mut sums = lock.lock().unwrap_or_else(|p| p.into_inner());
                    sums.turns += 1;
                    if sums.closed {
                        return; // the collector dropped
                    }
                    if sums.last_take.is_none_or(|at| at.elapsed() > idle_after) {
                        // Park until a take or the drop notifies.
                        drop(wake.wait(sums));
                        continue;
                    }
                    if let Some(metrics) = fs::read(&metrics_path).ok().as_deref().and_then(parse_gpu_metrics) {
                        if let Some(mhz) = metrics.average_gfxclk_mhz {
                            sums.gfx_sum += f64::from(mhz);
                            sums.gfx_reads += 1;
                        }
                        if let Some(mhz) = metrics.average_uclk_mhz {
                            sums.uclk_sum += f64::from(mhz);
                            sums.uclk_reads += 1;
                        }
                        sums.latest = Some(metrics);
                    }
                    // Sleep on the condvar, not the clock: a drop ends
                    // the thread at once.
                    drop(wake.wait_timeout(sums, period));
                }
            })
            .ok()?;
        Some(ClockPoller { shared, idle_after })
    }

    /// The last table the poller parsed (None before its first read).
    fn latest(&self) -> Option<GpuMetrics> {
        self.shared.0.lock().unwrap_or_else(|p| p.into_inner()).latest
    }

    #[cfg(test)]
    fn turns(&self) -> u64 {
        self.shared.0.lock().unwrap_or_else(|p| p.into_inner()).turns
    }

    /// The window's mean clocks and read count; resets the window. A
    /// window longer than the idle limit reports no reads: the poller
    /// parked partway through it.
    fn take(&self) -> (Option<f64>, Option<f64>, u32) {
        let (lock, wake) = &*self.shared;
        let mut sums = lock.lock().unwrap_or_else(|p| p.into_inner());
        let was_idle = sums.last_take.is_none_or(|at| at.elapsed() > self.idle_after);
        let mean = |sum: f64, reads: u32| (reads > 0).then(|| (sum / f64::from(reads)).round());
        let result = if was_idle {
            (None, None, 0)
        } else {
            (mean(sums.gfx_sum, sums.gfx_reads), mean(sums.uclk_sum, sums.uclk_reads), sums.gfx_reads)
        };
        *sums = ClockSums {
            last_take: Some(std::time::Instant::now()),
            latest: sums.latest,
            turns: sums.turns,
            ..Default::default()
        };
        if was_idle {
            wake.notify_one();
        }
        result
    }
}

impl Drop for ClockPoller {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        lock.lock().unwrap_or_else(|p| p.into_inner()).closed = true;
        wake.notify_one();
    }
}

pub struct GpuCollector {
    window: SelfInterval,
    device_path: Option<PathBuf>,
    hwmon_path: Option<PathBuf>,
    pci_address: String,
    device_name: String,
    previous_engine_ns: HashMap<i32, u64>,
    pids_without_drm: HashMap<i32, u8>,
    clocks: Option<ClockPoller>,
}

impl Default for GpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuCollector {
    pub fn new() -> Self {
        let mut collector = GpuCollector {
            window: SelfInterval::default(),
            device_path: None,
            hwmon_path: None,
            pci_address: String::new(),
            device_name: "GPU".to_string(),
            previous_engine_ns: HashMap::new(),
            pids_without_drm: HashMap::new(),
            clocks: None,
        };
        collector.discover();
        // Only for a table we can parse; otherwise hwmon's instant
        // read stays, and clock_source says so.
        collector.clocks = collector
            .device_path
            .as_ref()
            .map(|device| device.join("gpu_metrics"))
            .filter(|path| fs::read(path).ok().as_deref().and_then(parse_gpu_metrics).is_some())
            .and_then(ClockPoller::spawn);
        collector
    }

    pub fn is_available(&self) -> bool {
        self.device_path.is_some()
    }

    fn discover(&mut self) {
        let Ok(entries) = fs::read_dir("/sys/class/drm") else {
            return;
        };
        let mut cards: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let digits = name.strip_prefix("card")?;
                (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
                    .then_some(name)
            })
            .collect();
        cards.sort();
        for card in cards {
            let device = PathBuf::from(format!("/sys/class/drm/{card}/device"));
            let vendor = read_trimmed(device.join("vendor"));
            if vendor.as_deref() == Some(AMD_PCI_VENDOR_ID)
                && device.join("gpu_busy_percent").exists()
            {
                self.pci_address = fs::canonicalize(&device)
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                    .unwrap_or_default();
                self.hwmon_path = find_hwmon(&device);
                self.device_name = query_marketing_name(&self.pci_address, &device);
                self.device_path = Some(device);
                return;
            }
        }
    }

    pub fn collect(&mut self, now: std::time::Instant) -> GpuSnapshot {
        let interval_seconds = self.window.tick(now);
        let Some(device) = self.device_path.clone() else {
            return GpuSnapshot::default(); // available: false
        };
        let mut snapshot = GpuSnapshot {
            available: true,
            device_name: self.device_name.clone(),
            ..Default::default()
        };

        snapshot.busy_percent = read_u64(device.join("gpu_busy_percent")).unwrap_or(0) as f32;
        snapshot.vram_used_bytes = read_u64(device.join("mem_info_vram_used")).unwrap_or(0);
        snapshot.vram_total_bytes = read_u64(device.join("mem_info_vram_total")).unwrap_or(0);
        snapshot.gtt_used_bytes = read_u64(device.join("mem_info_gtt_used")).unwrap_or(0);
        snapshot.gtt_total_bytes = read_u64(device.join("mem_info_gtt_total")).unwrap_or(0);

        if let Some(hwmon) = &self.hwmon_path {
            // Temperatures by their driver label: the channel order
            // is not an ABI (an APU has only "edge"; some boards
            // expose junction first).
            let milli = |name: &str| read_u64(hwmon.join(name)).map(|v| v as f32 / 1000.0);
            let by_label = |wanted: &str, fallback: &str| {
                (1..=8)
                    .find(|index| {
                        read_trimmed(hwmon.join(format!("temp{index}_label"))).as_deref()
                            == Some(wanted)
                    })
                    .map(|index| format!("temp{index}_input"))
                    .or_else(|| {
                        // No labels at all (old kernels): the
                        // documented amdgpu order.
                        (!hwmon.join("temp1_label").exists()).then(|| fallback.to_string())
                    })
                    .and_then(|file| milli(&file))
            };
            snapshot.temperature_edge_celsius = by_label("edge", "temp1_input");
            snapshot.temperature_junction_celsius = by_label("junction", "temp2_input");
            snapshot.temperature_memory_celsius = by_label("mem", "temp3_input");
            let micro = |name: &str| read_u64(hwmon.join(name)).map(|v| v as f32 / 1e6);
            snapshot.power_draw_watts = micro("power1_average").or_else(|| micro("power1_input"));
            snapshot.power_cap_watts = micro("power1_cap");
            snapshot.core_clock_mhz = read_u64(hwmon.join("freq1_input")).map(|hz| hz as f64 / 1e6);
            snapshot.memory_clock_mhz =
                read_u64(hwmon.join("freq2_input")).map(|hz| hz as f64 / 1e6);
            snapshot.fan_rpm = read_u64(hwmon.join("fan1_input")).map(|v| v as u32);
            snapshot.fan_max_rpm = read_u64(hwmon.join("fan1_max")).map(|v| v as u32);
        }

        // Clocks: the window's mean from the poller (G8). The first
        // sample of a window (or one after a park) has no reads and
        // keeps hwmon's instant value, labelled as such.
        let (core, memory, reads) = self.clocks.as_ref().map_or((None, None, 0), ClockPoller::take);
        if reads > 0 {
            snapshot.core_clock_mhz = core.or(snapshot.core_clock_mhz);
            snapshot.memory_clock_mhz = memory.or(snapshot.memory_clock_mhz);
            snapshot.clock_source = Some(format!("mean of {reads} reads"));
        } else if snapshot.core_clock_mhz.is_some() {
            snapshot.clock_source = Some("instant read".to_string());
        }
        // The VRM temperatures move slowly; one read is honest (G12).
        // A warm poller read the table within the last 200 ms, so use
        // its copy; only a cold window costs its own SMU wake.
        let table = self.clocks.as_ref().and_then(|clocks| {
            if reads > 0 {
                clocks.latest()
            } else {
                fs::read(device.join("gpu_metrics")).ok().as_deref().and_then(parse_gpu_metrics)
            }
        });
        if let Some(metrics) = table {
            snapshot.temperature_vrm_gfx_celsius = metrics.temperature_vrgfx_celsius.map(f32::from);
            snapshot.temperature_vrm_soc_celsius = metrics.temperature_vrsoc_celsius.map(f32::from);
            snapshot.temperature_vrm_mem_celsius = metrics.temperature_vrmem_celsius.map(f32::from);
        }

        snapshot.processes = self.per_process_usage(interval_seconds);
        snapshot
    }

    fn per_process_usage(&mut self, interval_seconds: f64) -> Vec<GpuProcessUsage> {
        let mut usages = Vec::new();
        let mut seen_client_ids = HashSet::new();
        let mut live_pids = HashSet::new();

        let Ok(entries) = fs::read_dir("/proc") else {
            return usages;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<i32>().ok())
            else {
                continue;
            };
            live_pids.insert(pid);

            if let Some(remaining) = self.pids_without_drm.get_mut(&pid)
                && *remaining > 0
            {
                *remaining -= 1;
                continue;
            }

            match self.read_process_drm(pid, &mut seen_client_ids) {
                None => {
                    self.pids_without_drm.insert(pid, NEGATIVE_CACHE_SAMPLES);
                    self.previous_engine_ns.remove(&pid);
                }
                Some((engine_ns, vram_bytes)) => {
                    self.pids_without_drm.remove(&pid);
                    let mut busy_percent = 0.0f32;
                    if interval_seconds > 0.0
                        && let Some(previous_ns) = self.previous_engine_ns.get(&pid)
                    {
                        busy_percent = (engine_ns.saturating_sub(*previous_ns) as f64
                            / (interval_seconds * 1e9)
                            * 100.0)
                            .clamp(0.0, 100.0) as f32;
                    }
                    self.previous_engine_ns.insert(pid, engine_ns);
                    if busy_percent > 0.05 || vram_bytes > 0 {
                        usages.push(GpuProcessUsage {
                            pid,
                            busy_percent,
                            vram_bytes,
                        });
                    }
                }
            }
        }

        self.previous_engine_ns.retain(|pid, _| live_pids.contains(pid));
        self.pids_without_drm.retain(|pid, _| live_pids.contains(pid));

        usages.sort_by(|a, b| {
            (b.busy_percent, b.vram_bytes)
                .partial_cmp(&(a.busy_percent, a.vram_bytes))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        usages
    }

    /// Sum engine time + VRAM over the process's DRM clients on this
    /// card; None when it holds no DRM files at all.
    fn read_process_drm(
        &self,
        pid: i32,
        seen_client_ids: &mut HashSet<u64>,
    ) -> Option<(u64, u64)> {
        let fd_dir = format!("/proc/{pid}/fd");
        let entries = fs::read_dir(&fd_dir).ok()?;

        let mut total_engine_ns = 0u64;
        let mut total_vram_bytes = 0u64;
        let mut found_drm_file = false;

        for entry in entries.flatten() {
            let Ok(metadata) = fs::metadata(entry.path()) else {
                continue;
            };
            let file_type = metadata.mode() & libc::S_IFMT;
            if file_type != libc::S_IFCHR {
                continue;
            }
            let major = libc::major(metadata.rdev());
            if major != DRM_CHAR_MAJOR {
                continue;
            }
            found_drm_file = true;

            let fd_name = entry.file_name();
            let fdinfo_path = format!("/proc/{pid}/fdinfo/{}", fd_name.to_string_lossy());
            let Ok(content) = fs::read_to_string(&fdinfo_path) else {
                continue;
            };
            if let Some((engine_ns, vram_bytes)) =
                drm_usage_from_fdinfo(&content, &self.pci_address, seen_client_ids)
            {
                total_engine_ns += engine_ns;
                total_vram_bytes += vram_bytes;
            }
        }

        found_drm_file.then_some((total_engine_ns, total_vram_bytes))
    }
}

fn find_hwmon(device: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(device.join("hwmon"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("hwmon"))
                .unwrap_or(false)
        })
        .collect();
    entries.sort();
    entries.into_iter().next()
}

/// The card's marketing name, best source first:
///   1. lspci's pci.ids name ("Navi 48 [Radeon AI PRO R9700]" →
///      the bracketed part) — skipped when pci.ids predates the card
///      and lspci can only say "Device 7551";
///   2. libdrm's amdgpu.ids (device id + revision → name), which the
///      Mesa stack updates on its own schedule;
///   3. "AMD GPU 1002:7551" — honest about what is known.
///
/// Runs once at discovery.
fn query_marketing_name(pci_address: &str, device: &Path) -> String {
    if let Some(name) = lspci_name(pci_address) {
        return name;
    }
    let device_id = read_trimmed(device.join("device")).unwrap_or_default();
    let revision = read_trimmed(device.join("revision")).unwrap_or_default();
    if let Some(name) = fs::read_to_string(AMDGPU_IDS)
        .ok()
        .and_then(|ids| amdgpu_ids_name(&ids, &device_id, &revision))
    {
        return name;
    }
    let id = device_id.trim_start_matches("0x");
    if id.is_empty() {
        "AMD GPU".to_string()
    } else {
        format!("AMD GPU 1002:{id}")
    }
}

const AMDGPU_IDS: &str = "/usr/share/libdrm/amdgpu.ids";

fn lspci_name(pci_address: &str) -> Option<String> {
    let slot = pci_address.split_once(':').map(|(_, rest)| rest)?;
    let output = Command::new("lspci").args(["-mm", "-s", slot]).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // split('"') alternates unquoted/quoted — quoted fields are the
    // odd indices: class, vendor, device, subvendor, …
    let device_field = text.split('"').skip(1).step_by(2).nth(2)?;
    let name = match (device_field.find('['), device_field.rfind(']')) {
        (Some(open), Some(close)) if open < close => &device_field[open + 1..close],
        _ => device_field.trim(),
    };
    // A stale pci.ids knows the vendor but not the card.
    let unknown = name.is_empty()
        || name
            .strip_prefix("Device ")
            .is_some_and(|id| id.chars().all(|c| c.is_ascii_hexdigit()));
    (!unknown).then(|| name.to_string())
}

/// amdgpu.ids rows: `7550,\tC0,\tAMD Radeon RX 9070 XT`. Exact
/// device+revision first, else the first row for the device.
pub fn amdgpu_ids_name(ids: &str, device_id: &str, revision: &str) -> Option<String> {
    let device = device_id.trim_start_matches("0x").to_ascii_uppercase();
    let revision = revision.trim_start_matches("0x").to_ascii_uppercase();
    let rows: Vec<(String, String, String)> = ids
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split(',').map(str::trim);
            Some((
                fields.next()?.to_ascii_uppercase(),
                fields.next()?.to_ascii_uppercase(),
                fields.next()?.to_string(),
            ))
        })
        .filter(|(id, _, _)| *id == device)
        .collect();
    rows.iter()
        .find(|(_, rev, _)| *rev == revision)
        .or_else(|| rows.first())
        .map(|(_, _, name)| name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `gpu_metrics` blob from this machine's Radeon AI PRO
    /// R9700 (Navi 48, format 1 content 3, 120 bytes). Expected values
    /// decoded by hand against `struct gpu_metrics_v1_3`
    /// (kgd_pp_interface.h): temps at 0x04.., averages at 0x28..
    const NAVI48_V1_3: &[u8] = include_bytes!("fixtures/gpu_metrics_v1_3_navi48.bin");

    fn poller_on_fixture(tag: &str, period_ms: u64, idle_after_ms: u64) -> (ClockPoller, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sysmon-poller-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gpu_metrics");
        fs::write(&path, NAVI48_V1_3).unwrap();
        let poller = ClockPoller::spawn_with(
            path,
            std::time::Duration::from_millis(period_ms),
            std::time::Duration::from_millis(idle_after_ms),
        )
        .expect("poller");
        (poller, dir)
    }

    #[test]
    fn a_window_after_a_park_is_not_a_partial_mean() {
        // W3: after IDLE_AFTER with no take the poller parks. The next
        // take spans the whole quiet stretch, but its reads cover only
        // the first IDLE_AFTER of it: a mean of part of the window,
        // labelled as the window's. It must be refused (None reads).
        let (poller, dir) = poller_on_fixture("park", 10, 150);
        let _ = poller.take();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (_, _, reads) = poller.take();
        let _ = fs::remove_dir_all(dir);
        assert_eq!(reads, 0, "a window that outlived the poller's idle limit reported {reads} reads");
    }

    #[test]
    fn a_window_while_warm_is_the_mean_of_its_reads() {
        let (poller, dir) = poller_on_fixture("warm", 10, 5_000);
        let _ = poller.take();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let (core, _, reads) = poller.take();
        let _ = fs::remove_dir_all(dir);
        assert!(reads >= 5, "only {reads} reads in 120 ms at 10 ms");
        assert_eq!(core, Some(725.0), "the fixture's average_gfxclk");
    }

    #[test]
    fn the_poller_shares_its_last_table_and_sleeps_when_parked() {
        // VRM temps come from the poller's last parse, not a second
        // SMU wake on the sampling path (Reviewer B #9); a parked
        // poller does no periodic reads at all (#10).
        let (poller, dir) = poller_on_fixture("share", 10, 50);
        let _ = poller.take();
        std::thread::sleep(std::time::Duration::from_millis(40));
        let latest = poller.latest().expect("a parsed table");
        assert_eq!(latest.temperature_vrmem_celsius, Some(40));
        std::thread::sleep(std::time::Duration::from_millis(150)); // parked now
        let parked_turns = poller.turns();
        std::thread::sleep(std::time::Duration::from_millis(2_300));
        let after = poller.turns();
        let _ = fs::remove_dir_all(dir);
        assert_eq!(after, parked_turns, "a parked poller kept waking");
    }

    #[test]
    fn gpu_metrics_v1_3_reads_the_firmware_averages() {
        let metrics = parse_gpu_metrics(NAVI48_V1_3).expect("v1.3 parses");
        assert_eq!(metrics.average_gfxclk_mhz, Some(725));
        assert_eq!(metrics.average_uclk_mhz, Some(239));
        assert_eq!(metrics.temperature_vrgfx_celsius, Some(40));
        assert_eq!(metrics.temperature_vrsoc_celsius, Some(40));
        assert_eq!(metrics.temperature_vrmem_celsius, Some(40));
    }

    #[test]
    fn gpu_metrics_unsupported_fields_are_none_not_65535() {
        // G10: socclk is 0xFFFF in the fixture ("not supported").
        let mut blob = NAVI48_V1_3.to_vec();
        blob[0x28..0x2a].copy_from_slice(&0xffffu16.to_le_bytes());
        blob[0x0a..0x0c].copy_from_slice(&0xffffu16.to_le_bytes());
        let metrics = parse_gpu_metrics(&blob).expect("still parses");
        assert_eq!(metrics.average_gfxclk_mhz, None);
        assert_eq!(metrics.temperature_vrgfx_celsius, None);
    }

    #[test]
    fn gpu_metrics_unknown_or_short_layouts_are_refused() {
        // G9: v1.0 puts system_clock_counter first — never guess.
        let mut v1_0 = NAVI48_V1_3.to_vec();
        v1_0[3] = 0;
        assert!(parse_gpu_metrics(&v1_0).is_none(), "v1.0 layout differs");
        let mut v2 = NAVI48_V1_3.to_vec();
        v2[2] = 2;
        assert!(parse_gpu_metrics(&v2).is_none(), "format 2 is the APU table");
        // G11: truncated blobs.
        assert!(parse_gpu_metrics(&NAVI48_V1_3[..0x2c]).is_none());
        assert!(parse_gpu_metrics(&[]).is_none());
        // A header that claims more than was read.
        let mut lying = NAVI48_V1_3.to_vec();
        lying[0..2].copy_from_slice(&200u16.to_le_bytes());
        assert!(parse_gpu_metrics(&lying).is_none());
    }

    #[test]
    fn gpu_metrics_v1_1_and_v1_2_share_the_prefix() {
        for content in [1u8, 2] {
            let mut blob = NAVI48_V1_3.to_vec();
            blob[3] = content;
            assert_eq!(parse_gpu_metrics(&blob).and_then(|m| m.average_gfxclk_mhz), Some(725));
        }
    }

    const FDINFO_FIXTURE: &str = "\
pos:	0
flags:	02100002
mnt_id:	24
ino:	1059
drm-driver:	amdgpu
drm-client-id:	42
drm-pdev:	0000:09:00.0
drm-memory-vram:	204800 KiB
drm-memory-gtt:	1024 KiB
drm-engine-gfx:	123456789 ns
drm-engine-compute:	23456789 ns
";

    #[test]
    fn fdinfo_engine_time_takes_max_across_engines() {
        let mut seen = HashSet::new();
        let (engine_ns, vram) =
            drm_usage_from_fdinfo(FDINFO_FIXTURE, "0000:09:00.0", &mut seen).expect("drm client");
        assert_eq!(engine_ns, 123_456_789);
        assert_eq!(vram, 204_800 * 1024);
    }

    #[test]
    fn fdinfo_other_card_and_duplicate_clients_are_skipped() {
        let mut seen = HashSet::new();
        assert!(drm_usage_from_fdinfo(FDINFO_FIXTURE, "0000:0a:00.0", &mut seen).is_none());
        assert!(drm_usage_from_fdinfo(FDINFO_FIXTURE, "0000:09:00.0", &mut seen).is_some());
        // Same client id again — a dup'd fd must not double count.
        assert!(drm_usage_from_fdinfo(FDINFO_FIXTURE, "0000:09:00.0", &mut seen).is_none());
    }

    #[test]
    fn amdgpu_ids_match_device_and_revision() {
        let ids = "# List of AMDGPU IDs\n1.0.0\n7550,\tC0,\tAMD Radeon RX 9070 XT\n\
                   7550,\tC3,\tAMD Radeon RX 9070\n7551,\tC0,\tAMD Radeon AI PRO R9700\n";
        assert_eq!(amdgpu_ids_name(ids, "0x7550", "0xc3").as_deref(), Some("AMD Radeon RX 9070"));
        assert_eq!(amdgpu_ids_name(ids, "0x7551", "0xc0").as_deref(), Some("AMD Radeon AI PRO R9700"));
        // Unknown revision: the device's first row beats nothing.
        assert_eq!(amdgpu_ids_name(ids, "0x7550", "0xff").as_deref(), Some("AMD Radeon RX 9070 XT"));
        assert_eq!(amdgpu_ids_name(ids, "0x1234", "0x00"), None);
    }
}
