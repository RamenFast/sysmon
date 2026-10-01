// SPDX-License-Identifier: GPL-3.0-or-later
//! The Sensors card, redesigned for reading at a glance (Ben, 3.1:
//! "make it easier to read").
//!
//! Every hwmon reading is placed in a group a person thinks in —
//! CPU, each drive by name, Motherboard, Battery — and given a name a
//! person would use ("Die (Tctl)", "Chiplet 1", "Controller").
//! Temperatures wear a heat bar scaled to *their own* limits, so a
//! 45 °C drive and a 90 °C CPU are compared to what each can take,
//! not to each other. Readings no working sensor produces (an
//! unconnected thermistor at −62 °C, an unwired channel at 0 °C) are
//! folded under "unconnected" instead of shouted. The raw driver
//! label is always one hover away, so nothing is renamed out of sight.
//!
//! The amdgpu chip belongs to the GPU card and is not repeated.

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use sysmon_core::snapshot::{FanReading, SensorChip, SensorKind, SensorsSnapshot, TempReading};
use sysmon_core::units::{self, TemperatureScale, format_temperature_in};

use super::glyphs::{self, Glyph};
use super::theme::Palette;

/// One row the card draws.
#[derive(Clone, Debug, PartialEq)]
pub enum Reading {
    Temperature {
        name: String,
        raw: String,
        celsius: f32,
        /// Where the heat bar ends: the sensor's crit, else max, else a
        /// sensible ceiling for its kind.
        limit: f32,
        limit_is_reported: bool,
    },
    Fan {
        name: String,
        raw: String,
        rpm: u32,
        duty_percent: Option<f32>,
        max_rpm: Option<u32>,
    },
    Volts {
        name: String,
        raw: String,
        volts: f32,
    },
    Watts {
        name: String,
        raw: String,
        watts: f32,
        cap: Option<f32>,
    },
}

/// A titled group of readings (one card section).
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// Stable key for fold state ("cpu", "drive:sda", "board").
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub glyph: Glyph,
    pub readings: Vec<Reading>,
    /// Implausible channels kept out of `readings` — listed on hover
    /// of the "unconnected" note so nothing disappears silently.
    pub unconnected: Vec<String>,
}

impl Group {
    /// The hottest plausible temperature in the group; on a tie the
    /// first listed reading wins (Iterator::max_by would take the last).
    pub fn hottest(&self) -> Option<(f32, &str)> {
        self.readings
            .iter()
            .filter_map(|reading| match reading {
                Reading::Temperature { name, celsius, .. } => Some((*celsius, name.as_str())),
                _ => None,
            })
            .fold(None, |best: Option<(f32, &str)>, candidate| match best {
                Some(best) if best.0 >= candidate.0 => Some(best),
                _ => Some(candidate),
            })
    }
}

/// A ceiling for the heat bar when the chip states no limit.
fn default_limit(kind: SensorKind) -> f32 {
    match kind {
        // Zen's Tctl throttles at 90–95 °C (k10temp exposes no crit).
        SensorKind::Cpu => 95.0,
        // SATA/NVMe drives are rated to 70 °C (controllers more).
        SensorKind::Drive => 70.0,
        _ => 90.0,
    }
}

/// A person's name for a k10temp/coretemp/drive/board label.
pub fn friendly_temperature_name(chip: &SensorChip, label: &str) -> String {
    let lower = label.to_ascii_lowercase();
    match chip.kind {
        SensorKind::Cpu => match label {
            "Tctl" => "Die (Tctl)".to_string(),
            "Tdie" => "Die".to_string(),
            l if l.starts_with("Tccd") => format!("Chiplet {}", &l[4..]),
            l if l.starts_with("Package") => "Package".to_string(),
            l if l.starts_with("Core") => l.to_string(),
            other => other.to_string(),
        },
        SensorKind::Drive => match lower.as_str() {
            "composite" => "Drive".to_string(),
            "temp1" => "Drive".to_string(),
            l if l.starts_with("sensor") => format!("Controller {}", &label[6..].trim()),
            _ => label.to_string(),
        },
        SensorKind::Board => match label {
            "SYSTIN" => "System (board)".to_string(),
            "CPUTIN" => "CPU socket".to_string(),
            "PECI Agent 0" | "PECI Agent 0 Calibration" => "CPU (PECI)".to_string(),
            // AMD SB-TSI: the CPU's own die sensor, relayed by the board.
            l if l.starts_with("TSI0") => "CPU via board (TSI)".to_string(),
            l if l.starts_with("TSI1") => "CPU die 2 via board (TSI)".to_string(),
            l if l.starts_with("SMBUSMASTER") => format!("SMBus {}", l.trim_start_matches("SMBUSMASTER").trim()),
            l if l.starts_with("AUXTIN") => format!("Aux {}", l.trim_start_matches("AUXTIN")),
            other => other.to_string(),
        },
        _ => label.to_string(),
    }
}

fn friendly_fan_name(chip: &SensorChip, fan: &FanReading, index: usize) -> String {
    if fan.label.starts_with("fan") && chip.kind == SensorKind::Board {
        // nct/it87 headers carry no labels; the board silkscreen calls
        // them by number (CPU_FAN is usually fan2 on ASRock, but that
        // is board-specific — say the header, never guess the role).
        format!("Fan header {}", fan.label.trim_start_matches("fan"))
    } else if fan.label.starts_with("fan") {
        format!("Fan {}", index + 1)
    } else {
        fan.label.clone()
    }
}

fn temperature(chip: &SensorChip, reading: &TempReading) -> Reading {
    let reported = reading.crit_celsius.or(reading.max_celsius).filter(|limit| *limit > 20.0);
    Reading::Temperature {
        name: friendly_temperature_name(chip, &reading.label),
        raw: format!("{} · {}", chip.name, reading.label),
        celsius: reading.celsius,
        limit: reported.unwrap_or_else(|| default_limit(chip.kind)),
        limit_is_reported: reported.is_some(),
    }
}

/// Snapshot → the card's groups, in reading order: CPU, drives (by
/// device name), motherboard, other, battery.
pub fn group_sensors(sensors: &SensorsSnapshot) -> Vec<Group> {
    let mut cpu = Group {
        key: "cpu".into(),
        title: "CPU".into(),
        subtitle: String::new(),
        glyph: Glyph::Cpu,
        readings: Vec::new(),
        unconnected: Vec::new(),
    };
    let mut board = Group {
        key: "board".into(),
        title: "Motherboard".into(),
        subtitle: String::new(),
        glyph: Glyph::Board,
        readings: Vec::new(),
        unconnected: Vec::new(),
    };
    let mut drives: Vec<Group> = Vec::new();
    let mut other: Vec<Group> = Vec::new();

    for chip in &sensors.chips {
        let target: &mut Group = match chip.kind {
            SensorKind::Gpu => continue, // the GPU card owns these
            SensorKind::Cpu => &mut cpu,
            SensorKind::Board => {
                board.subtitle = chip.name.clone();
                &mut board
            }
            SensorKind::Drive => {
                let device = chip.device.clone().unwrap_or_else(|| chip.name.clone());
                drives.push(Group {
                    key: format!("drive:{device}"),
                    title: chip.device_model.clone().unwrap_or_else(|| device.clone()),
                    subtitle: device,
                    glyph: Glyph::Drive,
                    readings: Vec::new(),
                    unconnected: Vec::new(),
                });
                drives.last_mut().expect("just pushed")
            }
            SensorKind::Other => {
                other.push(Group {
                    key: format!("other:{}", chip.name),
                    title: chip.name.clone(),
                    subtitle: String::new(),
                    glyph: Glyph::Thermometer,
                    readings: Vec::new(),
                    unconnected: Vec::new(),
                });
                other.last_mut().expect("just pushed")
            }
        };
        for reading in &chip.temps {
            if reading.plausible {
                target.readings.push(temperature(chip, reading));
            } else {
                target.unconnected.push(format!(
                    "{} · {} reads {:.1} °C",
                    chip.name, reading.label, reading.celsius
                ));
            }
        }
        for (index, fan) in chip.fans.iter().enumerate() {
            // A header that is neither spinning nor driven has nothing
            // plugged in: unconnected, not a reading.
            let driven = fan.duty_percent.is_some_and(|duty| duty > 0.0);
            if fan.rpm == 0 && !driven {
                target.unconnected.push(format!("{} · {} (no fan)", chip.name, fan.label));
                continue;
            }
            target.readings.push(Reading::Fan {
                name: friendly_fan_name(chip, fan, index),
                raw: format!("{} · {}", chip.name, fan.label),
                rpm: fan.rpm,
                duty_percent: fan.duty_percent,
                max_rpm: fan.max_rpm,
            });
        }
        for power in &chip.power {
            target.readings.push(Reading::Watts {
                name: power.label.clone(),
                raw: format!("{} · {}", chip.name, power.label),
                watts: power.watts,
                cap: power.cap_watts,
            });
        }
        // Super-I/O voltage inputs (in0…in14) measure a pin *before*
        // the board's resistor dividers; without the board's scaling
        // table they're not rail voltages, so they stay in the API and
        // out of the card. Labelled rails (a driver that knows) show.
        for voltage in &chip.voltages {
            let unlabelled = voltage
                .label
                .strip_prefix("in")
                .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
            if chip.kind == SensorKind::Board && unlabelled {
                continue;
            }
            target.readings.push(Reading::Volts {
                name: voltage.label.clone(),
                raw: format!("{} · {}", chip.name, voltage.label),
                volts: voltage.volts,
            });
        }
    }

    drives.sort_by(|a, b| a.subtitle.cmp(&b.subtitle));
    let mut groups = Vec::new();
    for group in std::iter::once(cpu).chain(drives).chain(std::iter::once(board)).chain(other) {
        if !group.readings.is_empty() || !group.unconnected.is_empty() {
            groups.push(group);
        }
    }
    if let Some(battery) = &sensors.battery {
        let mut name = format!("{:.0}% · {}", battery.percent, battery.status.to_lowercase());
        if let Some(seconds) = battery.seconds_remaining {
            name += &format!(" · {} left", units::format_duration_seconds(seconds as f64));
        }
        groups.push(Group {
            key: "battery".into(),
            title: "Battery".into(),
            subtitle: name,
            glyph: Glyph::Battery,
            readings: battery
                .power_draw_watts
                .map(|watts| Reading::Watts {
                    name: "Draw".into(),
                    raw: battery.name.clone(),
                    watts,
                    cap: None,
                })
                .into_iter()
                .collect(),
            unconnected: Vec::new(),
        });
    }
    groups
}

/// How hot, as a share of the reading's own limit, coloured on a
/// calm → warm → accent ramp.
fn heat_color(palette: &Palette, fraction: f32, base: Color32) -> Color32 {
    if fraction >= 0.92 {
        palette.accent
    } else if fraction >= 0.75 {
        base.lerp_to_gamma(palette.accent, (fraction - 0.75) / 0.17)
    } else {
        base
    }
}

/// The card body. `on_toggle(group_key)` fires when a group header is
/// clicked (fold/unfold); fold state lives in Settings.
pub fn sensors_body(
    ui: &mut Ui,
    palette: &Palette,
    scale: TemperatureScale,
    accent_series: Color32,
    groups: &[Group],
    is_folded: &dyn Fn(&str) -> bool,
    on_toggle: &mut dyn FnMut(&str),
) {
    for group in groups {
        let folded = is_folded(&group.key);
        // ── group header: glyph · title · subtitle … hottest ▸
        let header = ui.horizontal(|ui| {
            let tint = palette.ink_2;
            glyphs::show(
                ui,
                if folded { Glyph::Chevron } else { Glyph::ChevronDown },
                9.0,
                palette.muted,
            );
            glyphs::show(ui, group.glyph, 13.0, tint);
            ui.label(RichText::new(&group.title).color(palette.ink).size(12.0).strong());
            if !group.subtitle.is_empty() {
                ui.label(RichText::new(&group.subtitle).color(palette.muted).size(10.5));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some((celsius, _)) = group.hottest() {
                    ui.label(
                        RichText::new(format_temperature_in(celsius, scale))
                            .monospace()
                            .size(11.5)
                            .color(palette.value),
                    );
                }
            });
        });
        let header_response = ui
            .interact(header.response.rect, ui.id().with(("sensor_group", &group.key)), Sense::click())
            .on_hover_text(if folded { "Show readings" } else { "Fold away" });
        header_response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Sensors group {}", group.title))
        });
        if header_response.clicked() {
            on_toggle(&group.key);
        }
        if folded {
            continue;
        }

        ui.indent(("sensor_rows", &group.key), |ui| {
            for reading in &group.readings {
                reading_row(ui, palette, scale, accent_series, reading);
            }
            if !group.unconnected.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(format!(
                            "{} unconnected input{} hidden",
                            group.unconnected.len(),
                            if group.unconnected.len() == 1 { "" } else { "s" }
                        ))
                        .color(palette.muted.gamma_multiply(0.8))
                        .italics()
                        .size(10.0),
                    )
                    .on_hover_text(format!(
                        "Channels the chip reports but nothing is wired to (floating or unused):\n{}",
                        group.unconnected.join("\n")
                    ));
                });
            }
        });
        ui.add_space(3.0);
    }
}

fn reading_row(ui: &mut Ui, palette: &Palette, scale: TemperatureScale, series: Color32, reading: &Reading) {
    let (glyph, name, raw) = match reading {
        Reading::Temperature { name, raw, .. } => (Glyph::Thermometer, name, raw),
        Reading::Fan { name, raw, .. } => (Glyph::Fan, name, raw),
        Reading::Volts { name, raw, .. } => (Glyph::Bolt, name, raw),
        Reading::Watts { name, raw, .. } => (Glyph::Bolt, name, raw),
    };
    let row = ui.horizontal(|ui| {
        glyphs::show(ui, glyph, 11.0, palette.muted);
        ui.label(RichText::new(name).color(palette.ink_2).size(11.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match reading {
            Reading::Temperature { celsius, limit, limit_is_reported, .. } => {
                let fraction = (celsius / limit).clamp(0.0, 1.0);
                let color = heat_color(palette, fraction, palette.ink);
                ui.label(
                    RichText::new(format_temperature_in(*celsius, scale))
                        .monospace()
                        .size(11.5)
                        .color(color),
                );
                heat_bar(ui, palette, fraction, heat_color(palette, fraction, series))
                    .on_hover_text(format!(
                        "{:.0}% of {} ({})",
                        fraction * 100.0,
                        format_temperature_in(*limit, scale),
                        if *limit_is_reported { "the sensor's own limit" } else { "a typical limit for this part" }
                    ));
            }
            Reading::Fan { rpm, duty_percent, max_rpm, .. } => {
                let text = match (rpm, duty_percent) {
                    (0, Some(duty)) => format!("no signal · {duty:.0}% duty"),
                    (rpm, Some(duty)) => format!("{rpm} rpm · {duty:.0}%"),
                    (rpm, None) => match max_rpm {
                        Some(max) => format!("{rpm} / {max} rpm"),
                        None => format!("{rpm} rpm"),
                    },
                };
                let warn = *rpm == 0;
                ui.label(
                    RichText::new(text)
                        .monospace()
                        .size(11.5)
                        .color(if warn { palette.accent } else { palette.ink }),
                )
                .on_hover_text(if warn {
                    "The board is driving this header but reads no tachometer: \
                     a pump or fan without a speed wire, or one that has stopped."
                } else {
                    "rpm · the duty the board is driving it at"
                });
            }
            Reading::Volts { volts, .. } => {
                ui.label(RichText::new(format!("{volts:.3} V")).monospace().size(11.5));
            }
            Reading::Watts { watts, cap, .. } => {
                let text = match cap {
                    Some(cap) => format!("{watts:.1} W · cap {cap:.0} W"),
                    None => format!("{watts:.1} W"),
                };
                ui.label(RichText::new(text).monospace().size(11.5));
            }
        });
    });
    row.response.on_hover_text(format!("driver label: {raw}"));
}

fn heat_bar(ui: &mut Ui, palette: &Palette, fraction: f32, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(46.0, 5.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, palette.ink.gamma_multiply(0.08));
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(rect.width() * fraction, rect.height())),
            0.0,
            color.gamma_multiply(0.9),
        );
        // tick at 75% — "warm from here"
        let x = rect.left() + rect.width() * 0.75;
        painter.line_segment([pos2(x, rect.top() - 1.0), pos2(x, rect.bottom() + 1.0)], Stroke::new(1.0, palette.line_strong));
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use sysmon_core::snapshot::FanReading;

    fn temp(label: &str, celsius: f32) -> TempReading {
        TempReading {
            label: label.into(),
            celsius,
            plausible: sysmon_core::collect::sensors::temperature_is_plausible(celsius),
            ..Default::default()
        }
    }

    /// This machine, 2026-09-30: k10temp, an NVMe with four channels,
    /// a SATA drive, the nct6798 with floating inputs and dead headers,
    /// and amdgpu (which must not appear).
    fn this_machine() -> SensorsSnapshot {
        SensorsSnapshot {
            chips: vec![
                SensorChip {
                    name: "k10temp".into(),
                    kind: SensorKind::Cpu,
                    temps: vec![temp("Tctl", 90.0), temp("Tccd1", 88.0), temp("Tccd2", 85.0)],
                    ..Default::default()
                },
                SensorChip {
                    name: "nvme".into(),
                    kind: SensorKind::Drive,
                    device: Some("nvme0n1".into()),
                    device_model: Some("CT2000P3PSSD8".into()),
                    temps: vec![temp("Composite", 39.8), temp("Sensor 1", 39.8), temp("Sensor 2", 61.8)],
                    ..Default::default()
                },
                SensorChip {
                    name: "drivetemp".into(),
                    kind: SensorKind::Drive,
                    device: Some("sda".into()),
                    device_model: Some("KINGSTON SA400S3".into()),
                    temps: vec![temp("temp1", 28.0)],
                    ..Default::default()
                },
                SensorChip {
                    name: "nct6798".into(),
                    kind: SensorKind::Board,
                    temps: vec![temp("SYSTIN", 39.0), temp("AUXTIN1", -62.0), temp("PCH_CHIP_TEMP", 0.0)],
                    fans: vec![
                        FanReading { label: "fan1".into(), rpm: 0, duty_percent: Some(52.0), ..Default::default() },
                        FanReading { label: "fan2".into(), rpm: 1685, duty_percent: Some(100.0), ..Default::default() },
                        FanReading { label: "fan6".into(), rpm: 0, duty_percent: Some(0.0), ..Default::default() },
                    ],
                    voltages: vec![sysmon_core::snapshot::VoltageReading { label: "in1".into(), volts: 1.67 }],
                    ..Default::default()
                },
                SensorChip {
                    name: "amdgpu".into(),
                    kind: SensorKind::Gpu,
                    temps: vec![temp("edge", 40.0)],
                    ..Default::default()
                },
            ],
            battery: None,
        }
    }

    #[test]
    fn readings_land_in_human_groups_with_human_names() {
        let groups = group_sensors(&this_machine());
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["CPU", "CT2000P3PSSD8", "KINGSTON SA400S3", "Motherboard"], "GPU excluded, drives by name");

        let cpu = &groups[0];
        let names: Vec<String> = cpu
            .readings
            .iter()
            .map(|r| match r {
                Reading::Temperature { name, .. } => name.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(names, ["Die (Tctl)", "Chiplet 1", "Chiplet 2"]);
        assert_eq!(cpu.hottest().map(|(c, _)| c), Some(90.0));

        // A tie names the first reading, not the last: the board's TSI
        // mirror of the die must never outrank the CPU's own Tctl.
        let tied = Group {
            readings: vec![
                Reading::Temperature { name: "Die (Tctl)".into(), raw: String::new(), celsius: 86.0, limit: 95.0, limit_is_reported: false },
                Reading::Temperature { name: "CPU via board (TSI)".into(), raw: String::new(), celsius: 86.0, limit: 95.0, limit_is_reported: false },
            ],
            ..cpu.clone()
        };
        assert_eq!(tied.hottest().map(|(_, name)| name), Some("Die (Tctl)"));

        // Four NVMe channels → four distinct names, not four "nvme0n1".
        let nvme_names: Vec<String> = groups[1]
            .readings
            .iter()
            .filter_map(|r| match r {
                Reading::Temperature { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(nvme_names, ["Drive", "Controller 1", "Controller 2"]);
    }

    #[test]
    fn floating_inputs_and_empty_headers_fold_away_but_are_listed() {
        let groups = group_sensors(&this_machine());
        let board = groups.iter().find(|g| g.key == "board").unwrap();
        // −62 °C and 0 °C are unconnected; SYSTIN stays.
        let temps = board.readings.iter().filter(|r| matches!(r, Reading::Temperature { .. })).count();
        assert_eq!(temps, 1);
        // fan6 (0 rpm, 0 duty) is an empty header; fan1 (0 rpm at 52%
        // duty) is a real warning and stays visible.
        let fans: Vec<&Reading> = board.readings.iter().filter(|r| matches!(r, Reading::Fan { .. })).collect();
        assert_eq!(fans.len(), 2);
        assert!(board.unconnected.iter().any(|u| u.contains("AUXTIN1")));
        assert!(board.unconnected.iter().any(|u| u.contains("fan6")));
        // Raw Super-I/O pins aren't rails: not on the card.
        assert!(!board.readings.iter().any(|r| matches!(r, Reading::Volts { .. })));
    }

    #[test]
    fn heat_limits_come_from_the_part() {
        let groups = group_sensors(&this_machine());
        let limit_of = |group: &Group| match &group.readings[0] {
            Reading::Temperature { limit, .. } => *limit,
            _ => 0.0,
        };
        assert_eq!(limit_of(&groups[0]), 95.0, "Zen without crit → 95");
        assert_eq!(limit_of(&groups[2]), 70.0, "SATA drive → 70");
    }
}
