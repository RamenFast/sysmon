// SPDX-License-Identifier: GPL-3.0-or-later
//! "Copy specs": the copy buttons' plumbing. The sheet itself is
//! `sysmon_core::specs` (shared with `sysmon specs`, so the window and
//! the CLI can never disagree); this file runs the collection off the
//! UI thread and hands the finished text back to a frame, which puts
//! it on the clipboard.
//!
//! Collection takes ~0.1–0.2 s here and up to the helper deadline (3 s)
//! on a machine with a hung GPU stack. Either way the window keeps
//! drawing; the button only answers when the text is real.
//!
//! The Sensors pop-out is the one exception: sensors are readings, not
//! specs, so its button copies the current readings, labelled with the
//! time they were taken.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use sysmon_core::snapshot::SystemSnapshot;
use sysmon_core::specs;
use sysmon_core::units::{TemperatureScale, format_temperature_in};

/// The spec section a pop-out's copy button copies. None for
/// Sensors, which copies its live readings instead.
pub fn spec_section_for_popout(popout: &str) -> Option<&'static str> {
    Some(match popout {
        "cpu" => "cpu",
        "memory" => "memory",
        "gpu" => "graphics",
        "disks" => "storage",
        "network" => "network",
        _ => return None,
    })
}

/// A finished copy: the text, and what the toast should say.
pub struct Copied {
    pub text: String,
    pub toast: String,
}

/// One collection at a time, off the UI thread.
pub struct SpecsCopier {
    tx: Sender<Copied>,
    rx: Receiver<Copied>,
    busy: Arc<AtomicBool>,
}

impl Default for SpecsCopier {
    fn default() -> Self {
        let (tx, rx) = channel();
        SpecsCopier { tx, rx, busy: Arc::new(AtomicBool::new(false)) }
    }
}

impl SpecsCopier {
    /// Start reading the machine; `section` None is the whole sheet.
    /// A click while a read is running is ignored (it would copy the
    /// same text twice). `what` names the copy for the toast.
    pub fn request(&self, ctx: &egui::Context, section: Option<&'static str>, what: String) {
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let tx = self.tx.clone();
        let busy = self.busy.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("sysmon-specs".to_string()).spawn(move || {
            let collected = specs::collect();
            let text = match section {
                Some(section) => specs::text::section_text(section, &collected),
                None => specs::text::specs_text(&collected),
            };
            let _ = tx.send(Copied { text, toast: format!("{what} copied") });
            busy.store(false, Ordering::Release);
            ctx.request_repaint();
        });
        if spawned.is_err() {
            self.busy.store(false, Ordering::Release);
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    /// A finished copy, when one is waiting.
    pub fn poll(&self) -> Option<Copied> {
        self.rx.try_recv().ok()
    }
}

/// The Sensors pop-out's copy: every chip by what it watches, with its
/// plausible readings at the snapshot's time.
pub fn sensor_readings_text(snapshot: &SystemSnapshot) -> String {
    let mut lines = vec![format!("Sensor readings at {}", clock_time(snapshot.ts))];
    if let Some(sensors) = &snapshot.sensors {
        for chip in &sensors.chips {
            let readings: Vec<String> = chip
                .temps
                .iter()
                .filter(|t| t.plausible)
                .map(|t| format!("{} {}", t.label, format_temperature_in(t.celsius, TemperatureScale::Both)))
                .chain(chip.fans.iter().map(|f| format!("{} {} rpm", f.label, f.rpm)))
                .collect();
            if readings.is_empty() {
                continue;
            }
            let name = match (&chip.device_model, &chip.device) {
                (Some(model), _) => format!("{} ({model})", chip.name),
                (None, Some(device)) => format!("{} ({device})", chip.name),
                _ => chip.name.clone(),
            };
            lines.push(format!("{name}: {}", readings.join(", ")));
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Unix seconds → local "HH:MM:SS".
fn clock_time(ts: f64) -> String {
    let seconds = ts as libc::time_t;
    // SAFETY: localtime_r only writes into the struct we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null() {
        return format!("{ts:.0} (unix)");
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sysmon_core::snapshot::{SensorChip, SensorsSnapshot, TempReading};

    #[test]
    fn every_hardware_popout_maps_to_a_spec_section() {
        for popout in crate::gui::settings::SECTION_KEYS {
            match spec_section_for_popout(popout) {
                Some(section) => assert!(specs::SPEC_SECTIONS.contains(&section), "{popout} → {section}"),
                None => assert_eq!(popout, "sensors", "only sensors copies readings"),
            }
        }
    }

    #[test]
    fn sensor_copy_is_labelled_as_readings_and_skips_dead_inputs() {
        let snapshot = SystemSnapshot {
            ts: 1_791_600_000.0,
            sensors: Some(SensorsSnapshot {
                chips: vec![SensorChip {
                    name: "k10temp".to_string(),
                    temps: vec![
                        TempReading { label: "Tctl".to_string(), celsius: 55.0, plausible: true, ..Default::default() },
                        TempReading { label: "temp9".to_string(), celsius: -62.0, plausible: false, ..Default::default() },
                    ],
                    ..Default::default()
                }],
                battery: None,
            }),
            ..Default::default()
        };
        let text = sensor_readings_text(&snapshot);
        assert!(text.starts_with("Sensor readings at "), "{text}");
        assert!(text.contains("k10temp: Tctl 131°F · 55°C\n"), "{text}");
        assert!(!text.contains("temp9"), "{text}");
    }
}
