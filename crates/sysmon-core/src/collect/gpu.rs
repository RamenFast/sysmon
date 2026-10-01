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

pub struct GpuCollector {
    window: SelfInterval,
    device_path: Option<PathBuf>,
    hwmon_path: Option<PathBuf>,
    pci_address: String,
    device_name: String,
    previous_engine_ns: HashMap<i32, u64>,
    pids_without_drm: HashMap<i32, u8>,
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
        };
        collector.discover();
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
