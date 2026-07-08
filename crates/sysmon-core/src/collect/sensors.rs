// SPDX-License-Identifier: GPL-3.0-or-later
//! Sensors: a walk over /sys/class/hwmon (temperatures with their
//! labels and limits, fans) plus /sys/class/power_supply batteries.
//! The amdgpu chip shows up here too — the API reports everything;
//! the GUI's sensors card just skips what the GPU card already shows.

use std::fs;
use std::path::Path;

use crate::snapshot::{
    BatterySnapshot, FanReading, PowerReading, SensorChip, SensorsSnapshot, TempReading,
    VoltageReading,
};

use super::read::{read_f64, read_trimmed, read_u64};

pub struct SensorsCollector;

impl SensorsCollector {
    pub fn collect(&self) -> SensorsSnapshot {
        let mut snapshot = SensorsSnapshot::default();

        if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
            let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();
            for path in paths {
                if let Some(chip) = read_chip(&path) {
                    snapshot.chips.push(chip);
                }
            }
        }

        snapshot.battery = read_battery();
        snapshot
    }

    /// The CPU package temperature: k10temp's Tctl/Tdie (AMD) or
    /// coretemp's Package (Intel), for the CPU card headline.
    pub fn cpu_temperature(snapshot: &SensorsSnapshot) -> Option<f32> {
        for chip in &snapshot.chips {
            if chip.name == "k10temp" {
                // Prefer Tdie (real silicon temp) over Tctl (offset
                // control value) when both exist.
                for wanted in ["Tdie", "Tctl"] {
                    if let Some(reading) = chip.temps.iter().find(|t| t.label == wanted) {
                        return Some(reading.celsius);
                    }
                }
                return chip.temps.first().map(|t| t.celsius);
            }
            if chip.name == "coretemp"
                && let Some(reading) = chip.temps.iter().find(|t| t.label.starts_with("Package")) {
                    return Some(reading.celsius);
                }
        }
        None
    }
}

/// Channel indices present for a `<prefix>N_input` family, sorted.
/// hwmon numbering has GAPS (k10temp: temp1 Tctl, temp3 Tccd1, no
/// temp2 — counting up from 1 and stopping at the first hole is the
/// bug that hid Tccd1), and `in*` channels start at 0, so the only
/// honest enumeration is a directory scan.
fn channel_indices(path: &Path, prefix: &str, suffix: &str) -> Vec<u32> {
    let mut indices: Vec<u32> = fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let file = entry.file_name();
                    let file = file.to_string_lossy();
                    file.strip_prefix(prefix)?
                        .strip_suffix(suffix)?
                        .parse::<u32>()
                        .ok()
                })
                .collect()
        })
        .unwrap_or_default();
    indices.sort_unstable();
    indices
}

fn read_chip(path: &Path) -> Option<SensorChip> {
    let name = read_trimmed(path.join("name"))?;
    let mut chip = SensorChip {
        name,
        ..Default::default()
    };

    // Drive chips (drivetemp, nvme) measure one specific drive —
    // resolve which, or four SATA drives all present as identical
    // "drivetemp · temp1" rows. SATA: device/block/<sda>; NVMe: the
    // namespace dir sits directly under device/.
    chip.device = fs::read_dir(path.join("device/block"))
        .ok()
        .and_then(|mut entries| {
            entries
                .next()?
                .ok()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
        })
        .or_else(|| {
            fs::read_dir(path.join("device")).ok().and_then(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .find(|file| {
                        // A namespace dir: "nvme0n1", not the plain
                        // "nvme" subsystem entries.
                        file.strip_prefix("nvme")
                            .is_some_and(|rest| rest.contains('n'))
                    })
            })
        });
    chip.device_model = read_trimmed(path.join("device/model"));
    let label_or = |kind: &str, index: u32| {
        read_trimmed(path.join(format!("{kind}{index}_label")))
            .unwrap_or_else(|| format!("{kind}{index}"))
    };

    for index in channel_indices(path, "temp", "_input") {
        let Some(millicelsius) = read_f64(path.join(format!("temp{index}_input"))) else {
            continue;
        };
        let milli = |suffix: &str| read_f64(path.join(format!("temp{index}_{suffix}")));
        chip.temps.push(TempReading {
            label: label_or("temp", index),
            celsius: (millicelsius / 1000.0) as f32,
            max_celsius: milli("max").map(|v| (v / 1000.0) as f32),
            crit_celsius: milli("crit").map(|v| (v / 1000.0) as f32),
        });
    }

    for index in channel_indices(path, "fan", "_input") {
        let Some(rpm) = read_u64(path.join(format!("fan{index}_input"))) else {
            continue;
        };
        chip.fans.push(FanReading {
            label: label_or("fan", index),
            rpm: rpm as u32,
            max_rpm: read_u64(path.join(format!("fan{index}_max")))
                .filter(|&max| max > 0)
                .map(|max| max as u32),
        });
    }

    // Voltage rails: inN_input is millivolts.
    for index in channel_indices(path, "in", "_input") {
        let Some(millivolts) = read_f64(path.join(format!("in{index}_input"))) else {
            continue;
        };
        chip.voltages.push(VoltageReading {
            label: label_or("in", index),
            volts: (millivolts / 1000.0) as f32,
        });
    }

    // Power rails: microwatts, averaged where the driver offers it
    // (amdgpu's PPT), instantaneous otherwise. A channel's data file
    // is `_average` OR `_input` (`_label` is optional) — enumerate
    // both and dedupe.
    let mut power_indices = channel_indices(path, "power", "_average");
    power_indices.extend(channel_indices(path, "power", "_input"));
    power_indices.sort_unstable();
    power_indices.dedup();
    for index in power_indices {
        let micro = |suffix: &str| read_f64(path.join(format!("power{index}_{suffix}")));
        let Some(microwatts) = micro("average").or_else(|| micro("input")) else {
            continue;
        };
        chip.power.push(PowerReading {
            label: label_or("power", index),
            watts: (microwatts / 1e6) as f32,
            cap_watts: micro("cap").map(|v| (v / 1e6) as f32),
        });
    }

    (!chip.temps.is_empty()
        || !chip.fans.is_empty()
        || !chip.voltages.is_empty()
        || !chip.power.is_empty())
    .then_some(chip)
}

fn read_battery() -> Option<BatterySnapshot> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if read_trimmed(path.join("type")).as_deref() != Some("Battery") {
            continue;
        }
        let percent = read_f64(path.join("capacity"))? as f32;
        let status = read_trimmed(path.join("status")).unwrap_or_else(|| "Unknown".to_string());

        // power_now (µW) directly, or current_now (µA) × voltage_now (µV).
        let power_watts = read_f64(path.join("power_now"))
            .map(|micro| micro / 1e6)
            .or_else(|| {
                let amps = read_f64(path.join("current_now"))? / 1e6;
                let volts = read_f64(path.join("voltage_now"))? / 1e6;
                Some(amps * volts)
            });
        // Remaining: energy_now (µWh) / power_now (µW) hours.
        let seconds_remaining = match (read_f64(path.join("energy_now")), power_watts) {
            (Some(energy_uwh), Some(watts)) if watts > 0.5 && status == "Discharging" => {
                Some(((energy_uwh / 1e6) / watts * 3600.0) as u64)
            }
            _ => None,
        };

        return Some(BatterySnapshot {
            name: entry.file_name().to_string_lossy().to_string(),
            percent,
            status,
            power_draw_watts: power_watts.map(|w| w as f32),
            seconds_remaining,
        });
    }
    None
}
