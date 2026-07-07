// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-process detail window (its own viewport, click-to-dismiss):
//! identity, resources, and the live connection list — "what is this
//! app doing on the network" one click from anywhere the process
//! appears. Returns false when the window was closed.

use egui::{Align, Layout, RichText, ViewportBuilder, ViewportId};

use sysmon_core::snapshot::SystemSnapshot;
use sysmon_core::units::{Units, format_duration_seconds, format_rate, format_size};

use super::cards::AppAction;
use super::theme::Palette;

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
