// SPDX-License-Identifier: GPL-3.0-or-later
//! Usage history: what the machine did over the last minutes, hours or
//! days, kept on disk so a copy button or an agent can ask "how much
//! GPU did the game use while the model stayed loaded?" after the fact.
//!
//! One compact JSON line per sample ([`HistoryRecord`]), only the
//! readings worth a timeline: CPU, memory, GPU (and the processes
//! holding it), network, disks, temperatures and fans. Files are one
//! UTC hour each under `$XDG_STATE_HOME/sysmon/history/`; a finished
//! hour is gzipped, and the store is pruned by age *and* by a byte cap
//! (`store.rs`), so it can never grow past what the user allowed.
//!
//! One writer at a time: the recorder holds an exclusive `flock` on
//! the directory's `.lock`, so two SysMon windows never interleave
//! lines. Readers need no lock (they skip a torn last line).
//!
//! `summary.rs` turns a time range into min/avg/p95/max plus a compact
//! timeline per reading; `text.rs` writes that as the plain text the
//! copy buttons and `sysmon history` share.

pub mod store;
pub mod summary;
pub mod text;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::snapshot::{SensorKind, SystemSnapshot};

pub use store::{Limits, OpenError, Recorder, Recording, RecordingStatus, StoreInfo};
pub use summary::{GpuProcessSummary, MetricSummary, Summary, summarize};

/// Section names `sysmon history` and the copy buttons accept. They are
/// the pop-out sections, so each pop-out copies its own.
pub const HISTORY_SECTIONS: [&str; 6] = ["cpu", "memory", "gpu", "network", "disks", "sensors"];

/// GPU processes kept per sample: the biggest VRAM holders and anything
/// busy. A game, the model and the compositor fit with room to spare.
pub const GPU_PROCESSES_PER_SAMPLE: usize = 10;

/// One sample, as written to disk. Field names carry units like the
/// snapshot's; numbers are rounded to what a person reads (0.1 % / °C /
/// W, whole bytes and bytes per second). A line is ~3.4 KB on a machine
/// with 24 temperature channels; a sealed hour stores it in ~160 B.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecord {
    /// Unix time of the sample, seconds.
    pub ts: f64,
    /// The window every rate in this record spans (0 on a sampler's
    /// first sample).
    pub interval_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu: Option<CpuRecord>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory: Option<MemoryRecord>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gpu: Option<GpuRecord>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub network: Option<NetworkRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub disks: Vec<DiskRecord>,
    /// Plausible temperatures only (unconnected inputs are left out).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub temps: Vec<TempRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub fans: Vec<FanRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuRecord {
    pub busy_percent: f32,
    pub kernel_percent: f32,
    pub iowait_percent: f32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub busy_clock_mhz: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub temperature_celsius: Option<f32>,
    pub load_1m: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub cached_bytes: u64,
    pub swap_used_bytes: u64,
    pub committed_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuRecord {
    pub busy_percent: f32,
    pub vram_used_bytes: u64,
    pub vram_total_bytes: u64,
    pub gtt_used_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub power_watts: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub edge_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub junction_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory_celsius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub core_clock_mhz: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory_clock_mhz: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fan_rpm: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub processes: Vec<GpuProcessRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuProcessRecord {
    pub pid: i32,
    /// What a person calls the process (the table's display name, else
    /// its comm).
    pub name: String,
    pub busy_percent: f32,
    pub vram_bytes: u64,
    #[serde(skip_serializing_if = "is_zero", default)]
    pub gtt_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkRecord {
    pub download_bps: f64,
    pub upload_bps: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskRecord {
    /// Device basename ("nvme0n1p2"), never the mount path.
    pub device: String,
    pub read_bps: f64,
    pub write_bps: f64,
    pub util_percent: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TempRecord {
    /// "k10temp Tctl", "drivetemp sda temp1", "amdgpu junction".
    pub sensor: String,
    pub celsius: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FanRecord {
    /// "nct6798 fan2".
    pub sensor: String,
    pub rpm: u32,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn tenth(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

impl HistoryRecord {
    /// The record for one snapshot. GPU process names come from the
    /// snapshot's process table when it has one, else from
    /// `/proc/<pid>/comm` (a recorder that samples no process table).
    pub fn from_snapshot(snapshot: &SystemSnapshot) -> HistoryRecord {
        let mut record = HistoryRecord {
            ts: (snapshot.ts * 1000.0).round() / 1000.0,
            interval_seconds: (snapshot.interval_seconds * 1000.0).round() / 1000.0,
            ..Default::default()
        };
        if let Some(cpu) = &snapshot.cpu {
            record.cpu = Some(CpuRecord {
                busy_percent: tenth(cpu.overall_percent),
                kernel_percent: tenth(cpu.kernel_percent),
                iowait_percent: tenth(cpu.iowait_percent),
                busy_clock_mhz: cpu.frequency_busy_mhz.or(cpu.frequency_mhz).map(|mhz| mhz.round() as f32),
                temperature_celsius: cpu.temperature_celsius.map(tenth),
                load_1m: (cpu.load_1m as f32 * 100.0).round() / 100.0,
            });
        }
        if let Some(memory) = &snapshot.memory {
            record.memory = Some(MemoryRecord {
                used_bytes: memory.used_bytes,
                total_bytes: memory.total_bytes,
                cached_bytes: memory.cached_bytes,
                swap_used_bytes: memory.swap_used_bytes,
                committed_bytes: memory.committed_bytes,
            });
        }
        if let Some(gpu) = snapshot.gpu.as_ref().filter(|gpu| gpu.available) {
            let name_of = |pid: i32| -> String {
                snapshot
                    .processes
                    .as_ref()
                    .and_then(|records| records.iter().find(|r| r.pid == pid))
                    .map(|r| if r.display_name.is_empty() { r.name.clone() } else { r.display_name.clone() })
                    .or_else(|| process_display_name(pid))
                    .unwrap_or_else(|| format!("pid {pid}"))
            };
            // Biggest VRAM holders first, then anything busy that the
            // cut would have dropped.
            let mut by_vram: Vec<_> = gpu.processes.iter().filter(|p| p.vram_bytes >= 1 << 20 || p.busy_percent >= 0.5).collect();
            by_vram.sort_by_key(|p| std::cmp::Reverse(p.vram_bytes));
            let mut kept: Vec<_> = by_vram.iter().take(GPU_PROCESSES_PER_SAMPLE).copied().collect();
            for busy in by_vram.iter().skip(GPU_PROCESSES_PER_SAMPLE).filter(|p| p.busy_percent >= 1.0) {
                kept.push(busy);
            }
            record.gpu = Some(GpuRecord {
                busy_percent: tenth(gpu.busy_percent),
                vram_used_bytes: gpu.vram_used_bytes,
                vram_total_bytes: gpu.vram_total_bytes,
                gtt_used_bytes: gpu.gtt_used_bytes,
                power_watts: gpu.power_draw_watts.map(tenth),
                edge_celsius: gpu.temperature_edge_celsius.map(tenth),
                junction_celsius: gpu.temperature_junction_celsius.map(tenth),
                memory_celsius: gpu.temperature_memory_celsius.map(tenth),
                core_clock_mhz: gpu.core_clock_mhz.map(|mhz| mhz.round() as f32),
                memory_clock_mhz: gpu.memory_clock_mhz.map(|mhz| mhz.round() as f32),
                fan_rpm: gpu.fan_rpm,
                processes: kept
                    .into_iter()
                    .map(|p| GpuProcessRecord {
                        pid: p.pid,
                        name: name_of(p.pid),
                        busy_percent: tenth(p.busy_percent),
                        vram_bytes: p.vram_bytes,
                        gtt_bytes: p.gtt_bytes,
                    })
                    .collect(),
            });
        }
        if let Some(network) = &snapshot.network {
            record.network = Some(NetworkRecord {
                download_bps: network.download_bps.round(),
                upload_bps: network.upload_bps.round(),
            });
        }
        if let Some(disks) = &snapshot.disks {
            record.disks = disks
                .iter()
                .map(|disk| DiskRecord {
                    device: disk.device.rsplit('/').next().unwrap_or(&disk.device).to_string(),
                    read_bps: disk.read_bps.round(),
                    write_bps: disk.write_bps.round(),
                    util_percent: tenth(disk.util_percent),
                })
                .collect();
        }
        if let Some(sensors) = &snapshot.sensors {
            for chip in &sensors.chips {
                // A drive chip names its drive (four drivetemp chips
                // would otherwise read the same).
                let chip_name = match (&chip.device, chip.kind) {
                    (Some(device), SensorKind::Drive) => format!("{} {device}", chip.name),
                    _ => chip.name.clone(),
                };
                for temp in chip.temps.iter().filter(|t| t.plausible) {
                    record.temps.push(TempRecord { sensor: format!("{chip_name} {}", temp.label), celsius: tenth(temp.celsius) });
                }
                for fan in &chip.fans {
                    record.fans.push(FanRecord { sensor: format!("{chip_name} {}", fan.label), rpm: fan.rpm });
                }
            }
        }
        record
    }
}

/// The process table's name for a pid, read straight from /proc (a
/// recorder that samples no process table still names a "python3"
/// by its script and a "MainThread" by its program).
fn process_display_name(pid: i32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let raw = String::from_utf8_lossy(&raw);
    let argv: Vec<&str> = raw.split('\0').filter(|a| !a.is_empty()).collect();
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|path| path.file_name().map(|n| n.to_string_lossy().trim_end_matches(" (deleted)").to_string()));
    Some(crate::collect::process::display_name(comm.trim(), &argv, exe.as_deref()))
}

/// `$XDG_STATE_HOME/sysmon/history`, else `~/.local/state/sysmon/history`.
pub fn default_dir() -> PathBuf {
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    state_home.join("sysmon").join("history")
}

/// Unix seconds → (year, month, day, hour) in UTC, for file names.
/// Howard Hinnant's days-to-civil; no time crate needed.
pub fn utc_civil(ts: f64) -> (i64, u32, u32, u32) {
    let seconds = ts.floor() as i64;
    let days = seconds.div_euclid(86_400);
    let hour = (seconds.rem_euclid(86_400) / 3_600) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, hour)
}

/// (year, month, day, hour) in UTC → unix seconds at the hour's start.
pub fn utc_hour_start(year: i64, month: u32, day: u32, hour: u32) -> f64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + i64::from(hour) * 3_600) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{
        CpuSnapshot, GpuProcessUsage, GpuSnapshot, ProcessRecord, SensorChip, SensorsSnapshot, TempReading,
    };

    #[test]
    fn civil_dates_round_trip_through_hour_starts() {
        // 2026-10-10 23:00 UTC is 1791673200 (date -ud @1791673200).
        assert_eq!(utc_civil(1_791_673_200.0), (2026, 10, 10, 23));
        assert_eq!(utc_hour_start(2026, 10, 10, 23), 1_791_673_200.0);
        assert_eq!(utc_civil(0.0), (1970, 1, 1, 0));
        // A leap day and a year boundary.
        for (y, m, d, h) in [(2028, 2, 29, 5), (2026, 12, 31, 23), (2027, 1, 1, 0), (2000, 3, 1, 12)] {
            let start = utc_hour_start(y, m, d, h);
            assert_eq!(utc_civil(start), (y, m, d, h));
            assert_eq!(utc_civil(start + 3_599.9), (y, m, d, h), "same hour to its last instant");
        }
    }

    #[test]
    fn a_record_keeps_the_readings_and_names_gpu_processes() {
        let snapshot = SystemSnapshot {
            ts: 1_791_673_200.123_456,
            interval_seconds: 2.0004,
            cpu: Some(CpuSnapshot { overall_percent: 12.345, load_1m: 1.234_5, ..Default::default() }),
            gpu: Some(GpuSnapshot {
                available: true,
                busy_percent: 97.04,
                vram_used_bytes: 30 << 30,
                vram_total_bytes: 32 << 30,
                processes: vec![
                    GpuProcessUsage { pid: 7, busy_percent: 0.0, vram_bytes: 24 << 30, ..Default::default() },
                    GpuProcessUsage { pid: 8, busy_percent: 96.0, vram_bytes: 5 << 30, ..Default::default() },
                    // Below both cuts: not kept.
                    GpuProcessUsage { pid: 9, busy_percent: 0.1, vram_bytes: 12 << 10, ..Default::default() },
                ],
                ..Default::default()
            }),
            processes: Some(vec![ProcessRecord {
                pid: 8,
                name: "ArcRaiders-Win6".into(),
                display_name: "ArcRaiders".into(),
                ..Default::default()
            }]),
            sensors: Some(SensorsSnapshot {
                chips: vec![SensorChip {
                    name: "drivetemp".into(),
                    kind: SensorKind::Drive,
                    device: Some("sda".into()),
                    temps: vec![
                        TempReading { label: "temp1".into(), celsius: 33.04, plausible: true, ..Default::default() },
                        TempReading { label: "temp2".into(), celsius: -62.0, plausible: false, ..Default::default() },
                    ],
                    ..Default::default()
                }],
                battery: None,
            }),
            ..Default::default()
        };
        let record = HistoryRecord::from_snapshot(&snapshot);
        assert_eq!(record.ts, 1_791_673_200.123);
        assert_eq!(record.cpu.as_ref().unwrap().busy_percent, 12.3);
        assert_eq!(record.cpu.as_ref().unwrap().load_1m, 1.23);
        let gpu = record.gpu.as_ref().unwrap();
        assert_eq!(gpu.processes.len(), 2, "the 12 KiB idle client is not worth a line");
        assert_eq!(gpu.processes[0].pid, 7, "biggest VRAM holder first");
        assert_eq!(gpu.processes[1].name, "ArcRaiders", "named by the table's display name");
        assert_eq!(record.temps, vec![TempRecord { sensor: "drivetemp sda temp1".into(), celsius: 33.0 }]);
        // Round trip through the wire form.
        let line = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<HistoryRecord>(&line).unwrap(), record);
        assert!(!line.contains("\"disks\""), "empty sections stay off the line");
    }
}
