// SPDX-License-Identifier: GPL-3.0-or-later
//! The Inspector: deep detail for the selected process, docked beside
//! the process table (Ben, 3.1: "allow for deeper inspection of
//! processes, and nice UI/UX integration between overview and
//! processes").
//!
//! Top to bottom: the parent chain as clickable breadcrumbs
//! (systemd › sway › ghostty › jcode), identity, live CPU and memory
//! sparklines, the memory split (RSS · PSS · USS · swap — what it maps,
//! what its fair share is, what quitting it would free), I/O, the
//! open-file count, cgroup and working directory, its children, its
//! sockets, and the actions. Everything heavier than the snapshot
//! (smaps_rollup, fd count) is read for this one pid only, at most
//! once a second.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use egui::{Align, Layout, RichText, ScrollArea, Sense, Ui, vec2};

use sysmon_core::collect::detail::{ProcessDetail, read_process_detail};
use sysmon_core::snapshot::{ProcessRecord, SystemSnapshot};
use sysmon_core::units::{format_duration_seconds, format_rate, format_size};

use super::cards::{AppAction, process_context_menu};
use super::glyphs::{self, Glyph};
use super::graphs::{self, HISTORY_LENGTH};
use super::icons::{self, IconCache};
use super::settings::Display;
use super::theme::{GRAPH_SERIES_CPU, GRAPH_SERIES_MEMORY, Palette, card_frame, graph_color};

const DETAIL_REFRESH: Duration = Duration::from_secs(1);

/// The Inspector's own memory: which pid, its recent history, and the
/// last deep read.
#[derive(Default)]
pub struct InspectorState {
    pub pid: Option<i32>,
    /// starttime of the inspected instance — a recycled pid is a
    /// different process and must not inherit the history.
    started_ts: f64,
    cpu: VecDeque<f64>,
    rss: VecDeque<f64>,
    last_ts: f64,
    detail: Option<ProcessDetail>,
    detail_read_at: Option<Instant>,
}

impl InspectorState {
    pub fn inspect(&mut self, pid: i32) {
        if self.pid != Some(pid) {
            *self = InspectorState {
                pid: Some(pid),
                ..Default::default()
            };
        }
    }

    pub fn clear(&mut self) {
        *self = InspectorState::default();
    }

    /// Fold the new snapshot in (once per sample, not per frame) and
    /// refresh the deep read when due.
    fn update(&mut self, snapshot: &SystemSnapshot, record: &ProcessRecord) {
        if record.started_ts != self.started_ts {
            self.cpu.clear();
            self.rss.clear();
            self.detail = None;
            self.detail_read_at = None;
            self.started_ts = record.started_ts;
        }
        if snapshot.ts != self.last_ts {
            self.last_ts = snapshot.ts;
            push(&mut self.cpu, record.cpu_percent as f64);
            push(&mut self.rss, record.memory_rss_bytes as f64);
        }
        let due = self.detail_read_at.is_none_or(|at| at.elapsed() >= DETAIL_REFRESH);
        if due {
            self.detail = read_process_detail(record.pid);
            self.detail_read_at = Some(Instant::now());
        }
    }
}

fn push(series: &mut VecDeque<f64>, value: f64) {
    if series.len() >= HISTORY_LENGTH {
        series.pop_front();
    }
    series.push_back(value);
}

/// pid → its ancestors, outermost first (stops at pid 1 / a missing
/// parent / a loop guard).
pub fn ancestry<'a>(records: &'a [ProcessRecord], by_pid: &HashMap<i32, usize>, pid: i32) -> Vec<&'a ProcessRecord> {
    let mut chain = Vec::new();
    let mut current = by_pid.get(&pid).map(|&i| &records[i]);
    while let Some(record) = current {
        if chain.len() > 32 {
            break; // a reparenting race should never loop us
        }
        chain.push(record);
        if record.ppid <= 0 || record.ppid == record.pid {
            break;
        }
        current = by_pid.get(&record.ppid).map(|&i| &records[i]);
    }
    chain.reverse();
    chain
}

#[allow(clippy::too_many_arguments)]
pub fn inspector(
    ui: &mut Ui,
    palette: &Palette,
    graph_palette_id: &str,
    display: Display,
    snapshot: &SystemSnapshot,
    state: &mut InspectorState,
    icon_cache: &mut IconCache,
    selected: &[i32],
    actions: &mut Vec<AppAction>,
) {
    let Some(records) = &snapshot.processes else {
        return;
    };
    let by_pid: HashMap<i32, usize> = records.iter().enumerate().map(|(i, r)| (r.pid, i)).collect();
    let Some(pid) = state.pid else {
        empty_state(ui, palette);
        return;
    };
    let Some(record) = by_pid.get(&pid).map(|&i| &records[i]) else {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            glyphs::show(ui, Glyph::Zombie, 14.0, palette.muted);
            ui.label(RichText::new(format!("PID {pid} has exited.")).color(palette.muted).italics());
        });
        if ui.button("Clear").clicked() {
            state.clear();
        }
        return;
    };
    state.update(snapshot, record);
    let size = |bytes: u64| format_size(bytes, display.units);

    ScrollArea::vertical().auto_shrink([false, false]).id_salt("inspector_scroll").show(ui, |ui| {
        // ── breadcrumbs: the parent chain, each crumb inspectable
        let chain = ancestry(records, &by_pid, pid);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            glyphs::show(ui, Glyph::Tree, 11.0, palette.muted);
            for (index, ancestor) in chain.iter().enumerate() {
                if index > 0 {
                    glyphs::show(ui, Glyph::Chevron, 8.0, palette.muted);
                }
                let is_self = ancestor.pid == pid;
                let text = RichText::new(icons::display_name(ancestor))
                    .size(10.5)
                    .color(if is_self { palette.ink } else { palette.muted });
                let response = ui
                    .add(egui::Label::new(text).sense(Sense::click()).selectable(false))
                    .on_hover_text(format!("{} · PID {} — inspect", ancestor.name, ancestor.pid));
                let crumb = format!("Breadcrumb {} — PID {}", icons::display_name(ancestor), ancestor.pid);
                response.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crumb.clone()));
                if response.clicked() && !is_self {
                    actions.push(AppAction::Inspect(ancestor.pid));
                }
            }
        });
        ui.add_space(4.0);

        // ── identity
        let identity = ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
            icon_cache.paint(ui, rect, record, palette.muted);
            ui.vertical(|ui| {
                ui.label(RichText::new(icons::display_name(record)).color(palette.title).strong().size(15.0));
                ui.horizontal(|ui| {
                    glyphs::show(ui, glyphs::for_process_state(&record.state), 10.0, palette.muted);
                    ui.label(
                        RichText::new(format!(
                            "{} · PID {} · {} · started {} ago",
                            record.state_word,
                            record.pid,
                            record.user,
                            format_duration_seconds((snapshot.ts - record.started_ts).max(0.0))
                        ))
                        .color(palette.muted)
                        .size(10.5),
                    );
                });
            });
        });
        process_context_menu(&identity.response.interact(Sense::click()), actions, record, selected, true);
        if record.display_name != record.name && !record.display_name.is_empty() {
            ui.label(RichText::new(format!("kernel name: {}", record.name)).color(palette.muted).size(10.0));
        }
        ui.add(
            egui::Label::new(
                RichText::new(if record.command_line.is_empty() {
                    "(kernel thread)".to_string()
                } else {
                    record.command_line.clone()
                })
                .color(palette.ink_2)
                .monospace()
                .size(10.0),
            )
            .wrap(),
        );
        ui.add_space(6.0);

        // ── live CPU + memory
        let cpu_color = graph_color(graph_palette_id, GRAPH_SERIES_CPU, palette.dark);
        let memory_color = graph_color(graph_palette_id, GRAPH_SERIES_MEMORY, palette.dark);
        section(ui, palette, Glyph::Gauge, "Now", |ui| {
            metric(ui, palette, "CPU", format!("{:.1}%", record.cpu_percent));
            let cpu: Vec<f64> = state.cpu.iter().copied().collect();
            graphs::sparkline(ui, palette, &cpu, 28.0, cpu_color, None);
            metric(ui, palette, "Memory (RSS)", size(record.memory_rss_bytes));
            let rss: Vec<f64> = state.rss.iter().copied().collect();
            graphs::sparkline(ui, palette, &rss, 28.0, memory_color, None);
            if record.gpu_busy_percent > 0.0 || record.gpu_vram_bytes > 0 {
                metric(
                    ui,
                    palette,
                    "GPU",
                    format!("{:.1}% · {}", record.gpu_busy_percent, size(record.gpu_vram_bytes)),
                );
            }
            if let (Some(read), Some(write)) = (record.disk_read_bps, record.disk_write_bps) {
                metric(
                    ui,
                    palette,
                    "Disk",
                    format!("R {}  W {}", format_rate(read, display.units), format_rate(write, display.units)),
                );
            }
            if let (Some(rx), Some(tx)) = (record.net_rx_bps, record.net_tx_bps) {
                metric(
                    ui,
                    palette,
                    "Network",
                    format!("↓ {}  ↑ {}", format_rate(rx, display.units), format_rate(tx, display.units)),
                );
            }
        });

        // ── the memory split
        if let Some(detail) = &state.detail {
            section(ui, palette, Glyph::Layers, "Memory, honestly", |ui| {
                let rss = detail.rss_bytes.max(1);
                note_metric(ui, palette, "Resident (RSS)", size(detail.rss_bytes), "every page it has mapped in, shared pages counted in full");
                if let Some(pss) = detail.pss_bytes {
                    note_metric(ui, palette, "Fair share (PSS)", size(pss), "shared pages split between their sharers — its honest share of the machine's RAM");
                }
                if let Some(uss) = detail.uss_bytes {
                    note_metric(ui, palette, "Unique (USS)", size(uss), "private pages only — what quitting it would free");
                    graphs::share_bar(
                        ui,
                        palette,
                        &[(memory_color, uss as f64), (memory_color.gamma_multiply(0.35), (rss.saturating_sub(uss)) as f64)],
                    )
                    .on_hover_text("private · shared");
                }
                if let Some(swap) = detail.swap_bytes.filter(|swap| *swap > 0) {
                    metric(ui, palette, "Swapped out", size(swap));
                }
                metric(ui, palette, "Virtual", size(record.memory_virtual_bytes));
            });

            section(ui, palette, Glyph::Folder, "Where", |ui| {
                if let Some(files) = detail.open_files {
                    metric(ui, palette, "Open files", files.to_string());
                }
                metric(ui, palette, "Threads", record.threads.to_string());
                metric(ui, palette, "Priority (nice)", format!("{:+}", record.nice));
                metric(ui, palette, "CPU time", format_duration_seconds(record.cpu_time_seconds));
                metric(
                    ui,
                    palette,
                    "Switches",
                    format!("{} vol · {} forced", detail.voluntary_switches, detail.involuntary_switches),
                );
                if let Some(oom) = detail.oom_score {
                    note_metric(ui, palette, "OOM score", oom.to_string(), "the kernel's kill order under memory pressure (higher goes first)");
                }
                for (label, value) in [("Executable", &detail.exe_path), ("Working dir", &detail.cwd), ("cgroup", &detail.cgroup)] {
                    if let Some(value) = value {
                        ui.label(RichText::new(label).color(palette.muted).size(10.5));
                        ui.add(egui::Label::new(RichText::new(value).monospace().size(10.0).color(palette.ink_2)).wrap());
                    }
                }
            });
        }

        // ── children
        let children: Vec<&ProcessRecord> = records.iter().filter(|r| r.ppid == pid).collect();
        if !children.is_empty() {
            section(ui, palette, Glyph::Tree, &format!("Children ({})", children.len()), |ui| {
                for child in children.iter().take(12) {
                    // One click rect per row, painted — labels on top
                    // would swallow the click.
                    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click());
                    let name = icons::display_name(child).to_string();
                    let a11y = format!("Inspect {name} — PID {}", child.pid);
                    response.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y.clone()));
                    if ui.is_rect_visible(rect) {
                        if response.hovered() {
                            ui.painter().rect_filled(rect, 0.0, palette.ink.gamma_multiply(0.06));
                        }
                        let icon = egui::Rect::from_center_size(egui::pos2(rect.left() + 8.0, rect.center().y), vec2(13.0, 13.0));
                        icon_cache.paint(ui, icon, child, palette.muted);
                        let painter = ui.painter();
                        painter.text(
                            egui::pos2(rect.left() + 20.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!("{name}  {}", child.pid),
                            egui::FontId::proportional(11.0),
                            palette.ink,
                        );
                        painter.text(
                            egui::pos2(rect.right() - 2.0, rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            format!("{:.1}% · {}", child.cpu_percent, size(child.memory_rss_bytes)),
                            egui::FontId::monospace(10.5),
                            palette.ink_2,
                        );
                    }
                    if response.on_hover_text("inspect").clicked() {
                        actions.push(AppAction::Inspect(child.pid));
                    }
                }
                if children.len() > 12 {
                    ui.label(RichText::new(format!("… and {} more", children.len() - 12)).color(palette.muted).size(10.5));
                }
            });
        }

        // ── sockets (the connection table is gathered while any detail
        // view is open)
        if let Some(connections) = &snapshot.connections {
            let mine: Vec<_> = connections.iter().filter(|c| c.pid == pid).collect();
            if !mine.is_empty() {
                section(ui, palette, Glyph::Plug, &format!("Sockets ({})", mine.len()), |ui| {
                    for connection in mine.iter().take(16) {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{:<5} {}", connection.protocol, connection.state))
                                    .color(palette.muted)
                                    .monospace()
                                    .size(10.0),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("{} → {}", connection.local_address, connection.remote_address))
                                        .monospace()
                                        .size(10.0),
                                )
                                .truncate(),
                            );
                        });
                    }
                });
            }
        }

        // ── actions
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Details window").clicked() {
                actions.push(AppAction::OpenDetails(pid));
            }
            if ui.button("Copy PID").clicked() {
                actions.push(AppAction::CopyPid(pid));
            }
            if ui.button("End process").clicked() {
                actions.push(AppAction::ConfirmTerminate(pid, icons::display_name(record).to_string()));
            }
        });
    });
}

fn empty_state(ui: &mut Ui, palette: &Palette) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        glyphs::show(ui, Glyph::Sparkle, 18.0, palette.muted);
        ui.label(RichText::new("Pick a process").color(palette.ink_2).size(13.0));
        ui.label(
            RichText::new("Click any row — here, or a top-3 row on the Overview — and its whole story opens here.")
                .color(palette.muted)
                .size(11.0),
        );
    });
}

fn section(ui: &mut Ui, palette: &Palette, glyph: Glyph, title: &str, body: impl FnOnce(&mut Ui)) {
    card_frame(palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            glyphs::show(ui, glyph, 12.0, palette.title);
            ui.label(RichText::new(title).color(palette.title).strong().size(12.0));
        });
        ui.add_space(2.0);
        body(ui);
    });
    ui.add_space(4.0);
}

fn metric(ui: &mut Ui, palette: &Palette, label: &str, value: String) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(palette.muted).size(10.5));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).monospace().size(11.5));
        });
    });
}

fn note_metric(ui: &mut Ui, palette: &Palette, label: &str, value: String, note: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(palette.muted).size(10.5)).on_hover_text(note);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).monospace().size(11.5)).on_hover_text(note);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: i32, ppid: i32, name: &str) -> ProcessRecord {
        ProcessRecord {
            pid,
            ppid,
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn ancestry_walks_to_init_outermost_first() {
        let records = vec![record(1, 0, "systemd"), record(3567, 1, "sway"), record(12539, 3567, "ghostty"), record(13451, 12539, "jcode")];
        let by_pid: HashMap<i32, usize> = records.iter().enumerate().map(|(i, r)| (r.pid, i)).collect();
        let names: Vec<&str> = ancestry(&records, &by_pid, 13451).iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["systemd", "sway", "ghostty", "jcode"]);
    }

    #[test]
    fn ancestry_survives_a_reparenting_loop() {
        // A race can briefly show a→b→a; the walk must end.
        let records = vec![record(10, 11, "a"), record(11, 10, "b")];
        let by_pid: HashMap<i32, usize> = records.iter().enumerate().map(|(i, r)| (r.pid, i)).collect();
        assert!(ancestry(&records, &by_pid, 10).len() <= 33);
    }

    #[test]
    fn a_recycled_pid_starts_a_fresh_history() {
        let mut state = InspectorState::default();
        state.inspect(42);
        let snapshot = |ts: f64| SystemSnapshot { ts, ..Default::default() };
        let mut first = record(42, 1, "old");
        first.started_ts = 100.0;
        first.cpu_percent = 50.0;
        state.update(&snapshot(1.0), &first);
        state.update(&snapshot(2.0), &first);
        assert_eq!(state.cpu.len(), 2);
        let mut reused = record(42, 1, "new");
        reused.started_ts = 500.0;
        state.update(&snapshot(3.0), &reused);
        assert_eq!(state.cpu.len(), 1, "history belongs to the old process");
    }
}
