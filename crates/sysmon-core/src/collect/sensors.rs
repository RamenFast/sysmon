// SPDX-License-Identifier: GPL-3.0-or-later
//! Sensors: a walk over /sys/class/hwmon (temperatures with their
//! labels and limits, fans) plus /sys/class/power_supply batteries.
//! The amdgpu chip shows up here too — the API reports everything;
//! the GUI's sensors card just skips what the GPU card already shows.

use std::fs;
use std::path::Path;

use crate::snapshot::{BatterySnapshot, FanReading, SensorChip, SensorsSnapshot, TempReading};

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
            if chip.name == "coretemp" {
                if let Some(reading) = chip.temps.iter().find(|t| t.label.starts_with("Package")) {
                    return Some(reading.celsius);
                }
            }
        }
        None
    }
}

fn read_chip(path: &Path) -> Option<SensorChip> {
    let name = read_trimmed(path.join("name"))?;
    let mut chip = SensorChip {
        name,
        ..Default::default()
    };

    for index in 1..=16u8 {
        let input = path.join(format!("temp{index}_input"));
        let Some(millicelsius) = read_f64(&input) else {
            if index > 1 {
                break; // temp channels are contiguous
            }
            continue;
        };
        let milli = |suffix: &str| read_f64(path.join(format!("temp{index}_{suffix}")));
        chip.temps.push(TempReading {
            label: read_trimmed(path.join(format!("temp{index}_label")))
                .unwrap_or_else(|| format!("temp{index}")),
            celsius: (millicelsius / 1000.0) as f32,
            max_celsius: milli("max").map(|v| (v / 1000.0) as f32),
            crit_celsius: milli("crit").map(|v| (v / 1000.0) as f32),
        });
    }

    for index in 1..=8u8 {
        let Some(rpm) = read_u64(path.join(format!("fan{index}_input"))) else {
            if index > 1 {
                break;
            }
            continue;
        };
        chip.fans.push(FanReading {
            label: read_trimmed(path.join(format!("fan{index}_label")))
                .unwrap_or_else(|| format!("fan{index}")),
            rpm: rpm as u32,
        });
    }

    (!chip.temps.is_empty() || !chip.fans.is_empty()).then_some(chip)
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
