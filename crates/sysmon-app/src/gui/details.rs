// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-process detail window (its own viewport, click-to-dismiss):
//! identity, resources, and the live connection list — "what is this
//! app doing on the network" one click from anywhere the process
//! appears. Returns false when the window was closed.
//!
//! Also home to the combined-details window (multi-select → right-
//! click → Combined details): a meta overview of the selection's
//! summed usage with per-process share bars, then a color-coded
//! block per process — each process wears its selection-slot color
//! in both places.

use egui::{
    Align, Color32, Layout, Rect, RichText, Sense, Ui, ViewportBuilder, ViewportId, pos2, vec2,
};

use sysmon_core::snapshot::{ProcessRecord, SystemSnapshot};
use sysmon_core::units::{Units, format_duration_seconds, format_rate, format_size};

use super::cards::{AppAction, process_context_menu, selection_color};
use super::graphs::share_bar;
use super::icons::{self, IconCache};
use super::theme::{Palette, card_frame};

pub fn details_window(
    ctx: &egui::Context,
    palette: &'static Palette,
    units: Units,
    snapshot: &SystemSnapshot,
    pid: i32,
    actions: &mut Vec<AppAction>,
) -> bool {
    let viewport_id = ViewportId::from_hash_of(("details", pid));
    let record = snapshot
        .processes
        .as_ref()
        .and_then(|records| records.iter().find(|r| r.pid == pid))
        .cloned();
    let title = match &record {
        Some(record) => format!("{} ({pid}) — SysMon", record.name),
        None => format!("PID {pid} — SysMon"),
    };

    let mut keep_open = true;
    let builder = ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([440.0, 420.0])
        .with_min_inner_size([340.0, 240.0]);
    ctx.show_viewport_immediate(viewport_id, builder, |ctx, _class| {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(palette.plane)
                    .inner_margin(egui::Margin::same(10)),
            )
            .show(ctx, |ui| {
                let Some(record) = &record else {
                    ui.label(
                        RichText::new("This process has exited.")
                            .color(palette.muted)
                            .italics(),
                    );
                    return;
                };

                let row = |ui: &mut egui::Ui, label: &str, value: String| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).color(palette.muted).size(11.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(value).monospace().size(11.5));
                        });
                    });
                };

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&record.name)
                                .color(palette.title)
                                .strong()
                                .size(15.0),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(if record.command_line.is_empty() {
                                "(kernel thread)".to_string()
                            } else {
                                record.command_line.clone()
                            })
                            .color(palette.ink_2)
                            .monospace()
                            .size(10.5),
                        );
                        ui.add_space(6.0);

                        row(ui, "PID · parent", format!("{} · {}", record.pid, record.ppid));
                        row(ui, "User", record.user.clone());
                        row(
                            ui,
                            "State",
                            format!("{} ({})", record.state_word, record.state),
                        );
                        row(ui, "Threads", record.threads.to_string());
                        row(ui, "Priority (nice)", format!("{}", record.nice));
                        row(
                            ui,
                            "Started",
                            format!(
                                "{} ago",
                                format_duration_seconds(
                                    (snapshot.ts - record.started_ts).max(0.0)
                                )
                            ),
                        );
                        row(
                            ui,
                            "CPU time",
                            format_duration_seconds(record.cpu_time_seconds),
                        );
                        ui.add_space(6.0);
                        ui.separator();

                        row(ui, "CPU", format!("{:.1}%", record.cpu_percent));
                        row(ui, "Memory (RSS)", format_size(record.memory_rss_bytes, units));
                        row(
                            ui,
                            "Memory (virtual)",
                            format_size(record.memory_virtual_bytes, units),
                        );
                        if record.gpu_busy_percent > 0.0 || record.gpu_vram_bytes > 0 {
                            row(ui, "GPU", format!("{:.1}%", record.gpu_busy_percent));
                            row(ui, "VRAM", format_size(record.gpu_vram_bytes, units));
                        }
                        if let (Some(read), Some(write)) =
                            (record.disk_read_bps, record.disk_write_bps)
                        {
                            row(
                                ui,
                                "Disk",
                                format!(
                                    "R {}  W {}",
                                    format_rate(read, units),
                                    format_rate(write, units)
                                ),
                            );
                        }
                        if let (Some(rx), Some(tx)) = (record.net_rx_bps, record.net_tx_bps) {
                            row(
                                ui,
                                "Network",
                                format!(
                                    "↓ {}  ↑ {}",
                                    format_rate(rx, units),
                                    format_rate(tx, units)
                                ),
                            );
                        }

                        ui.add_space(6.0);
                        ui.separator();
                        ui.label(
                            RichText::new("Connections")
                                .color(palette.title)
                                .strong()
                                .size(12.5),
                        );
                        match &snapshot.connections {
                            None => {
                                ui.label(
                                    RichText::new("gathering sockets… (next refresh)")
                                        .color(palette.muted)
                                        .italics()
                                        .size(11.0),
                                );
                            }
                            Some(connections) => {
                                let mine: Vec<_> = connections
                                    .iter()
                                    .filter(|connection| connection.pid == pid)
                                    .collect();
                                if mine.is_empty() {
                                    ui.label(
                                        RichText::new("no open sockets right now")
                                            .color(palette.muted)
                                            .italics()
                                            .size(11.0),
                                    );
                                }
                                for connection in mine {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "{:<5} {}",
                                                connection.protocol, connection.state
                                            ))
                                            .color(palette.muted)
                                            .monospace()
                                            .size(10.5),
                                        );
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if let (Some(rx), Some(tx)) =
                                                    (connection.rx_bps, connection.tx_bps)
                                                    && rx + tx >= 1.0
                                                {
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "↓{} ↑{}",
                                                            format_rate(rx, units),
                                                            format_rate(tx, units)
                                                        ))
                                                        .monospace()
                                                        .size(10.0),
                                                    );
                                                }
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(format!(
                                                            "{} → {}",
                                                            connection.local_address,
                                                            connection.remote_address
                                                        ))
                                                        .monospace()
                                                        .size(10.5),
                                                    )
                                                    .truncate(),
                                                );
                                            },
                                        );
                                    });
                                }
                            }
                        }

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("End process").clicked() {
                                actions.push(AppAction::ConfirmTerminate(
                                    record.pid,
                                    record.name.clone(),
                                ));
                            }
                            if ui.button("Copy PID").clicked() {
                                actions.push(AppAction::CopyPid(record.pid));
                            }
                        });
                    });
            });
        if ctx.input(|input| input.viewport().close_requested())
            || ctx.input(|input| input.key_pressed(egui::Key::Escape))
        {
            keep_open = false;
        }
    });
    keep_open
}

/// Sum an optional per-process metric: Some(total of what's known)
/// when at least one process reports it, None when nobody does —
/// degraded modes stay disclosed, never silently zero.
fn sum_optional<F>(records: &[(i32, Option<&ProcessRecord>, Color32)], value: F) -> Option<f64>
where
    F: Fn(&ProcessRecord) -> Option<f64>,
{
    let mut total = None;
    for (_, record, _) in records {
        if let Some(record) = record
            && let Some(v) = value(record)
        {
            *total.get_or_insert(0.0) += v;
        }
    }
    total
}

/// The combined details window: meta overview of the whole selection
/// (totals + who-owns-what share bars), then one block per process in
/// its slot color. Returns false when closed.
#[allow(clippy::too_many_arguments)]
pub fn combined_details_window(
    ctx: &egui::Context,
    palette: &'static Palette,
    graph_palette_id: &str,
    units: Units,
    snapshot: &SystemSnapshot,
    pids: &[i32],
    icon_cache: &mut IconCache,
    actions: &mut Vec<AppAction>,
) -> bool {
    let mut sorted = pids.to_vec();
    sorted.sort_unstable();
    let viewport_id = ViewportId::from_hash_of(("combined", sorted));

    // Selection order fixes each process's color slot — the same
    // color it wears in the table and overview rows.
    let entries: Vec<(i32, Option<&ProcessRecord>, Color32)> = pids
        .iter()
        .enumerate()
        .map(|(slot, pid)| {
            let record = snapshot
                .processes
                .as_ref()
                .and_then(|records| records.iter().find(|r| r.pid == *pid));
            (*pid, record, selection_color(graph_palette_id, palette.dark, slot))
        })
        .collect();
    let live = entries.iter().filter(|(_, r, _)| r.is_some()).count();

    let mut keep_open = true;
    let builder = ViewportBuilder::default()
        .with_title(format!("Combined ({}) — SysMon", pids.len()))
        .with_inner_size([460.0, 560.0])
        .with_min_inner_size([360.0, 300.0]);
    ctx.show_viewport_immediate(viewport_id, builder, |ctx, _class| {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(palette.plane)
                    .inner_margin(egui::Margin::same(10)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Combined details")
                                .color(palette.title)
                                .strong()
                                .size(15.0),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} processes selected · {live} live",
                                pids.len()
                            ))
                            .color(palette.muted)
                            .size(11.0),
                        );
                        ui.add_space(6.0);

                        combined_meta_card(ui, palette, units, &entries);
                        ui.add_space(8.0);

                        for (pid, record, color) in &entries {
                            process_block(
                                ui,
                                palette,
                                units,
                                snapshot,
                                *pid,
                                *record,
                                *color,
                                pids,
                                icon_cache,
                                actions,
                            );
                            ui.add_space(6.0);
                        }
                    });
            });
        if ctx.input(|input| input.viewport().close_requested())
            || ctx.input(|input| input.key_pressed(egui::Key::Escape))
        {
            keep_open = false;
        }
    });
    keep_open
}

/// The meta overview: each metric as `label · total`, with a stacked
/// share bar underneath — the color coding that maps every slice to
/// its process block below.
fn combined_meta_card(
    ui: &mut Ui,
    palette: &Palette,
    units: Units,
    entries: &[(i32, Option<&ProcessRecord>, Color32)],
) {
    let metric_row = |ui: &mut Ui, label: &str, total: String, segments: Vec<(Color32, f64)>| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).color(palette.muted).monospace().size(10.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(total)
                        .color(palette.value)
                        .monospace()
                        .size(12.0),
                );
            });
        });
        share_bar(ui, palette, &segments);
        ui.add_space(4.0);
    };
    let segments_of = |value: &dyn Fn(&ProcessRecord) -> f64| -> Vec<(Color32, f64)> {
        entries
            .iter()
            .map(|(_, record, color)| (*color, record.map(value).unwrap_or(0.0)))
            .collect()
    };

    card_frame(palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("Everything together")
                .color(palette.title)
                .strong()
                .size(12.5),
        );
        ui.add_space(4.0);

        let cpu_total: f64 = entries
            .iter()
            .filter_map(|(_, r, _)| r.map(|r| r.cpu_percent as f64))
            .sum();
        metric_row(
            ui,
            "CPU",
            format!("{cpu_total:.1} %"),
            segments_of(&|r| r.cpu_percent as f64),
        );

        let rss_total: u64 = entries
            .iter()
            .filter_map(|(_, r, _)| r.map(|r| r.memory_rss_bytes))
            .sum();
        metric_row(
            ui,
            "Memory",
            format_size(rss_total, units),
            segments_of(&|r| r.memory_rss_bytes as f64),
        );

        let gpu_total: f64 = entries
            .iter()
            .filter_map(|(_, r, _)| r.map(|r| r.gpu_busy_percent as f64))
            .sum();
        let vram_total: u64 = entries
            .iter()
            .filter_map(|(_, r, _)| r.map(|r| r.gpu_vram_bytes))
            .sum();
        if gpu_total > 0.0 {
            metric_row(
                ui,
                "GPU",
                format!("{gpu_total:.1} %"),
                segments_of(&|r| r.gpu_busy_percent as f64),
            );
        }
        if vram_total > 0 {
            metric_row(
                ui,
                "VRAM",
                format_size(vram_total, units),
                segments_of(&|r| r.gpu_vram_bytes as f64),
            );
        }

        if let Some(read) = sum_optional(entries, |r| r.disk_read_bps) {
            metric_row(
                ui,
                "Read/s",
                format_rate(read, units),
                segments_of(&|r| r.disk_read_bps.unwrap_or(0.0)),
            );
        }
        if let Some(write) = sum_optional(entries, |r| r.disk_write_bps) {
            metric_row(
                ui,
                "Write/s",
                format_rate(write, units),
                segments_of(&|r| r.disk_write_bps.unwrap_or(0.0)),
            );
        }
        if let Some(rx) = sum_optional(entries, |r| r.net_rx_bps) {
            metric_row(
                ui,
                "Net ↓/s",
                format_rate(rx, units),
                segments_of(&|r| r.net_rx_bps.unwrap_or(0.0)),
            );
        }
        if let Some(tx) = sum_optional(entries, |r| r.net_tx_bps) {
            metric_row(
                ui,
                "Net ↑/s",
                format_rate(tx, units),
                segments_of(&|r| r.net_tx_bps.unwrap_or(0.0)),
            );
        }

        let threads_total: u64 = entries
            .iter()
            .filter_map(|(_, r, _)| r.map(|r| u64::from(r.threads)))
            .sum();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Threads").color(palette.muted).monospace().size(10.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(threads_total.to_string())
                        .color(palette.value)
                        .monospace()
                        .size(12.0),
                );
            });
        });
    });
}

/// One process's block: color spine + chip, icon, identity, and its
/// own numbers — the legend for its slices in the meta bars above.
#[allow(clippy::too_many_arguments)]
fn process_block(
    ui: &mut Ui,
    palette: &Palette,
    units: Units,
    snapshot: &SystemSnapshot,
    pid: i32,
    record: Option<&ProcessRecord>,
    color: Color32,
    selected: &[i32],
    icon_cache: &mut IconCache,
    actions: &mut Vec<AppAction>,
) {
    let inner = card_frame(palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(record) = record else {
            ui.horizontal(|ui| {
                color_chip(ui, color);
                ui.label(
                    RichText::new(format!("PID {pid} — exited"))
                        .color(palette.muted)
                        .italics()
                        .size(11.5),
                );
            });
            return;
        };

        ui.horizontal(|ui| {
            color_chip(ui, color);
            let icon_size = 15.0;
            let (icon_rect, _) =
                ui.allocate_exact_size(vec2(icon_size, icon_size), Sense::hover());
            match icon_cache.texture_for(ui.ctx(), record) {
                Some(texture) => {
                    ui.painter().image(
                        texture.id(),
                        icon_rect,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                None => icons::draw_letter_tile(
                    &ui.painter().clone(),
                    icon_rect,
                    &record.name,
                    record.is_kernel_thread,
                    palette.muted,
                ),
            }
            ui.label(RichText::new(&record.name).strong().size(12.5));
            ui.label(
                RichText::new(format!("({})", record.pid))
                    .color(palette.muted)
                    .monospace()
                    .size(11.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(&record.user).color(palette.muted).size(11.0));
            });
        });

        let mut line = format!(
            "CPU {:.1}%  ·  RSS {}",
            record.cpu_percent,
            format_size(record.memory_rss_bytes, units)
        );
        if record.gpu_busy_percent > 0.0 || record.gpu_vram_bytes > 0 {
            line += &format!(
                "  ·  GPU {:.1}% {}",
                record.gpu_busy_percent,
                format_size(record.gpu_vram_bytes, units)
            );
        }
        ui.label(RichText::new(line).monospace().size(11.0));

        let mut io_line = String::new();
        if let (Some(read), Some(write)) = (record.disk_read_bps, record.disk_write_bps) {
            io_line += &format!(
                "R {}  W {}",
                format_rate(read, units),
                format_rate(write, units)
            );
        }
        if let (Some(rx), Some(tx)) = (record.net_rx_bps, record.net_tx_bps) {
            if !io_line.is_empty() {
                io_line += "  ·  ";
            }
            io_line += &format!("↓ {}  ↑ {}", format_rate(rx, units), format_rate(tx, units));
        }
        if !io_line.is_empty() {
            ui.label(RichText::new(io_line).monospace().size(11.0));
        }
        ui.label(
            RichText::new(format!(
                "{} · {} threads · started {} ago",
                record.state_word,
                record.threads,
                format_duration_seconds((snapshot.ts - record.started_ts).max(0.0))
            ))
            .color(palette.muted)
            .size(10.5),
        );
    });

    // The block's color spine, over the hairline frame.
    let frame_rect = inner.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(frame_rect.min, pos2(frame_rect.min.x + 3.0, frame_rect.max.y)),
        0.0,
        color,
    );

    if let Some(record) = record {
        let response = inner.response.interact(Sense::click());
        process_context_menu(&response, actions, record, selected, false);
    }
}

/// The 10×10 sharp color chip that names a process's slot color.
fn color_chip(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color);
}
