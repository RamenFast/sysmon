// SPDX-License-Identifier: GPL-3.0-or-later
//! Sensors: a walk over /sys/class/hwmon (temperatures with their
//! labels and limits, fans, voltage and power rails) plus
//! /sys/class/power_supply batteries. The amdgpu chip shows up here
//! too — the API reports everything; the GUI's sensors card leaves
//! the GPU's readings to the GPU card.
//!
//! Cost model (measured on this machine, kernel 7.0): a chip's
//! *layout* — which channels exist, their labels and limits, the drive
//! it measures — is a dozen directory scans and label reads, and it
//! doesn't change while the machine runs. It is discovered once and
//! re-checked only when the set of hwmon chips changes (a driver
//! loaded) or once a minute. Each sample then reads only the `_input`
//! files. Drive chips (drivetemp, nvme) are the expensive ones — every
//! read is a SMART/log-page command to the drive, 2–15 ms apiece — so
//! their readings refresh every DRIVE_REFRESH instead of every sample.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::snapshot::{
    BatterySnapshot, FanReading, PowerReading, SensorChip, SensorKind, SensorsSnapshot,
    TempReading, VoltageReading,
};

use super::read::{read_f64, read_trimmed, read_u64};

/// Drive temperatures move over minutes; asking the drive every
/// second costs milliseconds and can keep a sleeping disk awake.
const DRIVE_REFRESH: Duration = Duration::from_secs(10);
/// Layout re-discovery even when the chip set looks unchanged
/// (labels/limits rarely change, but a cap can be retuned).
const LAYOUT_REFRESH: Duration = Duration::from_secs(60);

/// What a driver name says the chip watches.
pub fn sensor_kind(chip_name: &str) -> SensorKind {
    match chip_name {
        "k10temp" | "coretemp" | "zenpower" | "cpu_thermal" | "fam15h_power" => SensorKind::Cpu,
        "amdgpu" | "radeon" | "nouveau" | "i915" | "xe" => SensorKind::Gpu,
        "drivetemp" | "nvme" => SensorKind::Drive,
        name if ["nct", "it8", "w83", "f718", "asus", "gigabyte", "dell_smm", "thinkpad", "acpitz"]
            .iter()
            .any(|prefix| name.starts_with(prefix)) =>
        {
            SensorKind::Board
        }
        _ => SensorKind::Other,
    }
}

/// A reading no working sensor produces: an unconnected thermistor
/// input floats far below zero (nct6798 AUXTIN1 reads −62 °C), a
/// channel the board wires to nothing reads exactly 0.000 °C (the
/// PCH_* channels on AMD boards), and nothing in a running PC is past
/// 150 °C.
pub fn temperature_is_plausible(celsius: f32) -> bool {
    (-30.0..=150.0).contains(&celsius) && celsius != 0.0
}

#[derive(Clone, Debug)]
struct TempChannel {
    input: PathBuf,
    label: String,
    max_celsius: Option<f32>,
    crit_celsius: Option<f32>,
}

#[derive(Clone, Debug)]
struct FanChannel {
    input: PathBuf,
    label: String,
    max_rpm: Option<u32>,
    /// pwmN (0–255) when the header is PWM-driven.
    duty: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct VoltageChannel {
    input: PathBuf,
    label: String,
}

#[derive(Clone, Debug)]
struct PowerChannel {
    input: PathBuf,
    label: String,
    cap_watts: Option<f32>,
}

/// Everything about a chip except its live readings.
#[derive(Clone, Debug)]
struct ChipLayout {
    name: String,
    kind: SensorKind,
    device: Option<String>,
    device_model: Option<String>,
    temps: Vec<TempChannel>,
    fans: Vec<FanChannel>,
    voltages: Vec<VoltageChannel>,
    power: Vec<PowerChannel>,
}

impl ChipLayout {
    fn discover(path: &Path) -> Option<ChipLayout> {
        let name = read_trimmed(path.join("name"))?;
        let label_or = |kind: &str, index: u32| {
            read_trimmed(path.join(format!("{kind}{index}_label")))
                .unwrap_or_else(|| format!("{kind}{index}"))
        };
        // Limits outside anything physical are "not set" sentinels
        // (an NVMe's unset threshold reads 65261.85 °C).
        let limit = |file: String| {
            read_f64(path.join(file))
                .map(|v| (v / 1000.0) as f32)
                .filter(|celsius| (-40.0..200.0).contains(celsius))
        };
        let micro = |file: String| read_f64(path.join(file)).map(|v| (v / 1e6) as f32);

        let temps = channel_indices(path, "temp", "_input")
            .into_iter()
            .map(|index| TempChannel {
                input: path.join(format!("temp{index}_input")),
                label: label_or("temp", index),
                max_celsius: limit(format!("temp{index}_max")),
                crit_celsius: limit(format!("temp{index}_crit")),
            })
            .collect();
        let fans = channel_indices(path, "fan", "_input")
            .into_iter()
            .map(|index| FanChannel {
                input: path.join(format!("fan{index}_input")),
                label: label_or("fan", index),
                max_rpm: read_u64(path.join(format!("fan{index}_max")))
                    .filter(|&max| max > 0)
                    .map(|max| max as u32),
                duty: Some(path.join(format!("pwm{index}"))).filter(|pwm| pwm.exists()),
            })
            .collect();
        let voltages = channel_indices(path, "in", "_input")
            .into_iter()
            .map(|index| VoltageChannel {
                input: path.join(format!("in{index}_input")),
                label: label_or("in", index),
            })
            .collect();
        // A power channel's data file is `_average` OR `_input`
        // (amdgpu's PPT is averaged) — prefer the average.
        let mut power_indices = channel_indices(path, "power", "_average");
        power_indices.extend(channel_indices(path, "power", "_input"));
        power_indices.sort_unstable();
        power_indices.dedup();
        let power = power_indices
            .into_iter()
            .map(|index| {
                let average = path.join(format!("power{index}_average"));
                PowerChannel {
                    input: if average.exists() {
                        average
                    } else {
                        path.join(format!("power{index}_input"))
                    },
                    label: label_or("power", index),
                    cap_watts: micro(format!("power{index}_cap")),
                }
            })
            .collect();

        Some(ChipLayout {
            kind: sensor_kind(&name),
            device: measured_drive(path),
            device_model: read_trimmed(path.join("device/model")),
            name,
            temps,
            fans,
            voltages,
            power,
        })
    }

    /// One reading of every channel. A channel whose file stops being
    /// readable is skipped (the census test counts readable files).
    fn read(&self) -> SensorChip {
        let milli = |path: &Path| read_f64(path).map(|v| (v / 1000.0) as f32);
        SensorChip {
            name: self.name.clone(),
            kind: self.kind,
            device: self.device.clone(),
            device_model: self.device_model.clone(),
            temps: self
                .temps
                .iter()
                .filter_map(|channel| {
                    let celsius = milli(&channel.input)?;
                    Some(TempReading {
                        label: channel.label.clone(),
                        celsius,
                        max_celsius: channel.max_celsius,
                        crit_celsius: channel.crit_celsius,
                        plausible: temperature_is_plausible(celsius),
                    })
                })
                .collect(),
            fans: self
                .fans
                .iter()
                .filter_map(|channel| {
                    Some(FanReading {
                        label: channel.label.clone(),
                        rpm: read_u64(&channel.input)? as u32,
                        max_rpm: channel.max_rpm,
                        duty_percent: channel
                            .duty
                            .as_ref()
                            .and_then(read_u64)
                            .map(|pwm| (pwm.min(255) as f32 / 255.0 * 100.0).round()),
                    })
                })
                .collect(),
            voltages: self
                .voltages
                .iter()
                .filter_map(|channel| {
                    Some(VoltageReading {
                        label: channel.label.clone(),
                        volts: milli(&channel.input)?,
                    })
                })
                .collect(),
            power: self
                .power
                .iter()
                .filter_map(|channel| {
                    Some(PowerReading {
                        label: channel.label.clone(),
                        watts: (read_f64(&channel.input)? / 1e6) as f32,
                        cap_watts: channel.cap_watts,
                    })
                })
                .collect(),
        }
    }

    fn has_channels(&self) -> bool {
        !(self.temps.is_empty()
            && self.fans.is_empty()
            && self.voltages.is_empty()
            && self.power.is_empty())
    }
}

/// Channel indices present for a `<prefix>N<suffix>` family, sorted.
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

/// The block device a drive chip measures: SATA (drivetemp) under
/// device/block/<sda>; NVMe namespaces ("nvme0n1") directly under
/// device/. Without it four SATA drives all read "drivetemp · temp1".
fn measured_drive(path: &Path) -> Option<String> {
    fs::read_dir(path.join("device/block"))
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
                        file.strip_prefix("nvme")
                            .is_some_and(|rest| rest.contains('n'))
                    })
            })
        })
}

/// A drive chip's last reading and when it was taken.
struct CachedReading {
    chip: SensorChip,
    read_at: Instant,
}

#[derive(Default)]
pub struct SensorsCollector {
    hwmon_dirs: Vec<PathBuf>,
    layouts: Vec<ChipLayout>,
    discovered_at: Option<Instant>,
    /// Indexed like `layouts`; Some only for drive chips.
    drive_cache: Vec<Option<CachedReading>>,
}

impl SensorsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    fn refresh_layouts(&mut self, now: Instant) {
        let mut dirs: Vec<PathBuf> = fs::read_dir("/sys/class/hwmon")
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default();
        dirs.sort();
        let stale = self
            .discovered_at
            .is_none_or(|at| now.duration_since(at) > LAYOUT_REFRESH);
        if dirs == self.hwmon_dirs && !stale {
            return;
        }
        self.layouts = dirs
            .iter()
            .filter_map(|path| ChipLayout::discover(path))
            .filter(ChipLayout::has_channels)
            .collect();
        self.drive_cache = self.layouts.iter().map(|_| None).collect();
        self.hwmon_dirs = dirs;
        self.discovered_at = Some(now);
    }

    pub fn collect(&mut self, now: Instant) -> SensorsSnapshot {
        self.refresh_layouts(now);
        let mut snapshot = SensorsSnapshot::default();
        for (layout, cache) in self.layouts.iter().zip(self.drive_cache.iter_mut()) {
            if layout.kind != SensorKind::Drive {
                snapshot.chips.push(layout.read());
                continue;
            }
            let fresh = cache
                .as_ref()
                .is_some_and(|cached| now.duration_since(cached.read_at) < DRIVE_REFRESH);
            if !fresh {
                *cache = Some(CachedReading {
                    chip: layout.read(),
                    read_at: now,
                });
            }
            if let Some(cached) = cache {
                snapshot.chips.push(cached.chip.clone());
            }
        }
        snapshot.battery = read_battery();
        snapshot
    }

    /// Only the CPU package temperature — what the CPU section needs
    /// when the full sensor sweep wasn't asked for.
    pub fn cpu_temperature_only(&mut self, now: Instant) -> Option<(f32, String)> {
        self.refresh_layouts(now);
        let chips: Vec<SensorChip> = self
            .layouts
            .iter()
            .filter(|layout| layout.kind == SensorKind::Cpu)
            .map(ChipLayout::read)
            .collect();
        cpu_temperature_in(&chips)
    }

    /// The CPU package temperature and its source, for the CPU card
    /// headline: k10temp's Tdie (Zen 1/+, where Tctl carries a fan
    /// offset) else Tctl (Zen 2+: Tctl == die temperature), or
    /// coretemp's Package (Intel).
    pub fn cpu_temperature(snapshot: &SensorsSnapshot) -> Option<(f32, String)> {
        cpu_temperature_in(&snapshot.chips)
    }
}

fn cpu_temperature_in(chips: &[SensorChip]) -> Option<(f32, String)> {
    for chip in chips {
        let pick = |reading: &TempReading| {
            Some((reading.celsius, format!("{} {}", chip.name, reading.label)))
        };
        if chip.name == "k10temp" || chip.name == "zenpower" {
            for wanted in ["Tdie", "Tctl"] {
                if let Some(reading) = chip.temps.iter().find(|t| t.label == wanted) {
                    return pick(reading);
                }
            }
            if let Some(reading) = chip.temps.first() {
                return pick(reading);
            }
        }
        if chip.name == "coretemp"
            && let Some(reading) = chip.temps.iter().find(|t| t.label.starts_with("Package"))
        {
            return pick(reading);
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floating_and_unwired_channels_are_not_plausible() {
        assert!(!temperature_is_plausible(-62.0), "nct6798 AUXTIN1, unconnected");
        assert!(!temperature_is_plausible(0.0), "PCH_* channels wired to nothing");
        assert!(!temperature_is_plausible(255.5), "nothing in a PC is past 150 °C");
        assert!(temperature_is_plausible(39.0));
        assert!(temperature_is_plausible(-5.0), "sub-ambient cooling is real");
    }

    #[test]
    fn chip_names_classify() {
        assert_eq!(sensor_kind("k10temp"), SensorKind::Cpu);
        assert_eq!(sensor_kind("amdgpu"), SensorKind::Gpu);
        assert_eq!(sensor_kind("nvme"), SensorKind::Drive);
        assert_eq!(sensor_kind("drivetemp"), SensorKind::Drive);
        assert_eq!(sensor_kind("nct6798"), SensorKind::Board);
        assert_eq!(sensor_kind("it8688"), SensorKind::Board);
        assert_eq!(sensor_kind("iwlwifi_1"), SensorKind::Other);
    }

    #[test]
    fn cpu_temperature_prefers_tdie_and_names_its_source() {
        let chip = |labels: &[(&str, f32)]| SensorChip {
            name: "k10temp".to_string(),
            temps: labels
                .iter()
                .map(|(label, celsius)| TempReading {
                    label: label.to_string(),
                    celsius: *celsius,
                    plausible: true,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        // Zen 1: Tctl = Tdie + 20 (fan-control offset) — Tdie is the truth.
        let zen1 = [chip(&[("Tctl", 70.0), ("Tdie", 50.0)])];
        assert_eq!(cpu_temperature_in(&zen1), Some((50.0, "k10temp Tdie".to_string())));
        // Zen 3: only Tctl (== die) plus per-CCD readings.
        let zen3 = [chip(&[("Tctl", 54.9), ("Tccd1", 46.5)])];
        assert_eq!(cpu_temperature_in(&zen3), Some((54.9, "k10temp Tctl".to_string())));
    }
}
