// SPDX-License-Identifier: GPL-3.0-or-later
//! The Overview page's own pieces (docs/dev/OVERVIEW-VIEW.md): the CPU
//! card's three views, the two-column breakpoint, and the status bar
//! along the bottom. 3.6 folded the Performance page into the
//! Overview: its scope graphs became the cards' graphs, its boxes
//! became card stats, and its status bar lives on here.

use sysmon_core::snapshot::SystemSnapshot;
use sysmon_core::units::{format_size, format_temperature_in};

use super::settings::Display;
use super::theme::Palette;

/// Two columns of cards from here (the central panel's width).
pub const TWO_COLUMN_WIDTH: f32 = 880.0;
/// The status bar names the commit limit and the uptime from here.
pub const WIDE_STATUS_WIDTH: f32 = 560.0;

/// The CPU card's view (Ben, 3.6: "different colored lines for each
/// cpu thread … displayed in one window view"). Stored in
/// `settings.cpu_graph_mode` by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuGraphMode {
    /// One trace for the whole CPU (busy, with kernel time under it).
    Total,
    /// Every thread as its own coloured line on one field.
    Threads,
    /// A small graph per thread.
    Grid,
}

impl CpuGraphMode {
    pub const ALL: [CpuGraphMode; 3] = [CpuGraphMode::Total, CpuGraphMode::Threads, CpuGraphMode::Grid];

    pub fn from_id(id: &str) -> Option<CpuGraphMode> {
        match id {
            "total" => Some(CpuGraphMode::Total),
            "threads" => Some(CpuGraphMode::Threads),
            "grid" => Some(CpuGraphMode::Grid),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            CpuGraphMode::Total => "total",
            CpuGraphMode::Threads => "threads",
            CpuGraphMode::Grid => "grid",
        }
    }

    /// The switch's word for it.
    pub fn label(self) -> &'static str {
        self.id()
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0)
    }
}

/// `3d 04:12` style uptime: days, then hours:minutes.
pub fn format_uptime(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours:02}:{minutes:02}")
    } else {
        format!("{hours:02}:{minutes:02}")
    }
}

/// The status line: the numbers worth a glance while the cards scroll,
/// most important first, so a narrow window loses the tail (the label
/// truncates and shows the whole line on hover). A wide one also names
/// the commit limit and the uptime.
pub fn status_text(snapshot: &SystemSnapshot, display: Display, wide: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(processes) = &snapshot.processes {
        parts.push(format!("{} proc", processes.len()));
    }
    if let Some(cpu) = &snapshot.cpu {
        parts.push(format!("CPU {:.0}%", cpu.overall_percent));
    }
    if let Some(gpu) = snapshot.gpu.as_ref().filter(|g| g.available) {
        parts.push(format!("GPU {:.0}%", gpu.busy_percent));
    }
    if let Some(gpu) = snapshot.nvidia.as_ref().filter(|g| g.available) {
        parts.push(format!("NVIDIA {:.0}%", gpu.busy_percent));
    }
    if let Some(t) = snapshot.cpu.as_ref().and_then(|c| c.temperature_celsius) {
        parts.push(format_temperature_in(t, display.temperature));
    }
    if let Some(memory) = &snapshot.memory {
        if wide {
            parts.push(format!(
                "Commit {} / {}",
                format_size(memory.committed_bytes, display.units),
                format_size(memory.commit_limit_bytes, display.units)
            ));
        } else {
            parts.push(format!("Commit {}", format_size(memory.committed_bytes, display.units)));
        }
    }
    if wide && let Some(system) = &snapshot.system {
        parts.push(format!("up {}", format_uptime(system.uptime_seconds)));
    }
    parts.join(" │ ")
}

pub fn status_bar(ctx: &egui::Context, palette: &Palette, text: &str) {
    egui::TopBottomPanel::bottom("overview_status")
        .frame(egui::Frame::new().fill(palette.surface_2).inner_margin(egui::Margin::symmetric(8, 3)))
        .show(ctx, |ui| {
            // A sensed label gets a real accessibility node (the plain
            // label's text is otherwise invisible to the harness).
            let response = ui.add(
                egui::Label::new(egui::RichText::new(text).monospace().size(11.0).color(palette.ink_2))
                    .truncate()
                    .sense(egui::Sense::click()),
            );
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("status: {text}")));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_views_round_trip_their_ids() {
        for mode in CpuGraphMode::ALL {
            assert_eq!(CpuGraphMode::from_id(mode.id()), Some(mode));
            assert_eq!(CpuGraphMode::ALL[mode.index()], mode);
        }
        assert_eq!(CpuGraphMode::from_id("per_thread"), None, "3.2's ids are migrated by settings, not accepted here");
    }

    #[test]
    fn uptime_reads_like_a_clock() {
        assert_eq!(format_uptime(0.0), "00:00");
        assert_eq!(format_uptime(3_600.0 * 4.0 + 60.0 * 12.0 + 86_400.0 * 3.0), "3d 04:12");
        assert_eq!(format_uptime(59.0), "00:00");
        assert_eq!(format_uptime(61.0), "00:01");
    }
}
