// SPDX-License-Identifier: GPL-3.0-or-later
//! The copy buttons' plumbing. The texts themselves live in the engine
//! (`sysmon_core::specs` for the spec sheet, `sysmon_core::history` for
//! the usage summary), shared with `sysmon specs` and `sysmon history`
//! so the window and the CLI can never disagree. This file runs the
//! reading off the UI thread and hands the finished text back to a
//! frame, which puts it on the clipboard.
//!
//! Two kinds of copy:
//! - the top bar's: the whole spec sheet (what the machine is);
//! - a section's (a pop-out's row, or a docked card's header): that
//!   section's specs, then its usage over the copy range the slider
//!   sets: min/avg/p95/max, a timeline, and for the GPU the programs
//!   that held it. The Sensors section has no specs; its readings at
//!   the moment of the click stand in for them.
//!
//! Specs take ~0.1–0.2 s here (helpers bounded at 3 s); an hour of
//! history ~10 ms, 30 days or a year well under a second through the
//! hourly rollups (measured: docs/dev/RECEIPTS.md, 3.5).
//! Either way the window keeps drawing; the button only answers when
//! the text is real.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use sysmon_core::history::{self, text::TextStyle};
use sysmon_core::snapshot::SystemSnapshot;
use sysmon_core::specs;
use sysmon_core::units::{TemperatureScale, format_temperature_in};

/// The spec section a pop-out's copy button copies. None for
/// Sensors, which copies its live readings instead.
pub fn spec_section_for_popout(popout: &str) -> Option<&'static str> {
    Some(match popout {
        "cpu" => "cpu",
        "memory" => "memory",
        "gpu" | "nvidia" => "graphics",
        "disks" => "storage",
        "network" => "network",
        _ => return None,
    })
}

/// The title a section's copy carries ("GPU", "Disks").
pub fn section_title(section: &str) -> &'static str {
    match section {
        "gpu" => "GPU",
        "nvidia" => "NVIDIA",
        "memory" => "Memory",
        "cpu" => "CPU",
        "network" => "Network",
        "disks" => "Disks",
        "sensors" => "Sensors",
        _ => "SysMon",
    }
}

/// What a copy holds besides the usage.
#[derive(Clone, Debug)]
pub enum SpecsPart {
    /// The whole spec sheet (the top bar).
    Whole,
    /// One spec section (`specs::SPEC_SECTIONS`).
    Section(&'static str),
    /// Text read on the UI thread (the Sensors readings).
    Text(String),
}

/// The usage half of a section's copy.
#[derive(Clone, Debug)]
pub struct HistoryPart {
    /// A `history::HISTORY_SECTIONS` name.
    pub section: &'static str,
    pub range_seconds: f64,
    pub dir: PathBuf,
    pub style: TextStyle,
}

/// One click's worth of work.
#[derive(Clone, Debug)]
pub struct CopyJob {
    pub title: String,
    pub specs: SpecsPart,
    pub history: Option<HistoryPart>,
}

impl CopyJob {
    /// The text, read now (the copier calls this off the UI thread;
    /// tests call it directly). Returns the text and the toast.
    pub fn run(&self, now_ts: f64) -> (String, String) {
        let specs_text = match &self.specs {
            SpecsPart::Whole => specs::text::specs_text(&specs::collect()),
            SpecsPart::Section(section) => specs::text::section_text(section, &specs::collect()),
            SpecsPart::Text(text) => text.clone(),
        };
        let Some(part) = &self.history else {
            return (specs_text, format!("{} copied", self.title));
        };
        let from = now_ts - part.range_seconds;
        // Both tiers; sealed hours merge from their rollups, so a year
        // copies in about the time a week used to.
        let summary = history::summarize_dir(&part.dir, part.section, from, now_ts, history::summary::TIMELINE_BUCKETS, true);
        let text = history::text::section_report(&self.title, Some(&specs_text), &summary, part.style);
        let range = history::text::range_label(part.range_seconds);
        let what = if matches!(self.specs, SpecsPart::Text(_)) { "readings" } else { "specs" };
        let toast = if summary.samples == 0 {
            format!("{} {what} copied (no history recorded in the last {range})", self.title)
        } else {
            format!("{} {what} + last {range} of usage copied", self.title)
        };
        (text, toast)
    }
}

/// A finished copy: the text, and what the toast should say.
pub struct Copied {
    pub text: String,
    pub toast: String,
}

/// One copy at a time, off the UI thread.
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
    /// Start a copy. A click while one is running is ignored (it would
    /// copy the same text twice).
    pub fn request(&self, ctx: &egui::Context, job: CopyJob) {
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let tx = self.tx.clone();
        let busy = self.busy.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("sysmon-copy".to_string()).spawn(move || {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let (text, toast) = job.run(now);
            let _ = tx.send(Copied { text, toast });
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

    /// A section copy is its specs, then the usage over the range, read
    /// from a real history directory (the GUI's own writer path).
    #[test]
    fn a_section_copy_carries_the_readings_and_their_history() {
        use sysmon_core::history::{GpuRecord, HistoryRecord, Limits, Recorder};
        let dir = std::env::temp_dir().join(format!("sysmon-copyjob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = 1_791_690_000.0;
        let mut recorder = Recorder::open(&dir, Limits::new(1e9, None, None), "test").unwrap();
        for i in 0..30 {
            recorder
                .record(&HistoryRecord {
                    ts: now - 60.0 + f64::from(i) * 2.0,
                    interval_seconds: 2.0,
                    gpu: Some(GpuRecord { busy_percent: 50.0, vram_used_bytes: 5_000_000_000, vram_total_bytes: 34_000_000_000, ..Default::default() }),
                    ..Default::default()
                })
                .unwrap();
        }
        let job = CopyJob {
            title: "GPU".into(),
            specs: SpecsPart::Text("GPU       Test card\n".into()),
            history: Some(HistoryPart { section: "gpu", range_seconds: 300.0, dir: dir.clone(), style: TextStyle::default() }),
        };
        let (text, toast) = job.run(now);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(text.starts_with(&format!("SysMon {} · GPU\n\nGPU       Test card\n\nUsage · last 5 min · ", sysmon_core::VERSION)), "{text}");
        assert!(text.contains("30 samples, one every 2 s"), "{text}");
        assert!(text.lines().any(|l| l.starts_with("VRAM used") && l.ends_with("5.0 GB")), "{text}");
        assert_eq!(toast, "GPU readings + last 5 min of usage copied", "a Text part is readings, not specs");
        // Nothing recorded in the range: said, not zeroed.
        let empty = CopyJob { history: Some(HistoryPart { dir: std::env::temp_dir().join("sysmon-no-such-history"), ..job.history.clone().unwrap() }), ..job };
        let (text, toast) = empty.run(now);
        assert!(text.contains("No history recorded in this range"), "{text}");
        assert_eq!(toast, "GPU readings copied (no history recorded in the last 5 min)", "Text specs are readings");
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
