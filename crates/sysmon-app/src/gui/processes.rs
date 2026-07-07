// SPDX-License-Identifier: GPL-3.0-or-later
//! The Processes page: v1's full table, grown — icon, name, PID,
//! user, CPU%, memory, GPU%, VRAM, disk read/write, net ↓/↑ (split,
//! new), threads (new), priority, state (new), started (new),
//! command. Click a header to sort, type to filter (Ctrl+F focuses),
//! right-click for actions, double-click for details. Rows are
//! virtualized; sorting is stable so the table doesn't shimmer.

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Ui, pos2, vec2};
use egui_extras::{Column, TableBuilder};

use sysmon_core::snapshot::{ProcessRecord, SystemSnapshot};
use sysmon_core::units::{Units, format_duration_seconds, format_rate, format_size};

use super::cards::{AppAction, process_context_menu};
use super::icons::{self, IconCache};
use super::theme::Palette;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Pid,
    User,
    Cpu,
    Memory,
    Gpu,
    Vram,
    Read,
    Write,
    NetDown,
    NetUp,
    Threads,
    Nice,
    State,
    Started,
}

pub struct ProcessTableState {
    pub filter: String,
    pub sort_column: SortColumn,
    pub sort_descending: bool,
    pub selected_pid: Option<i32>,
    pub focus_filter: bool,
}

impl Default for ProcessTableState {
    fn default() -> Self {
        ProcessTableState {
            filter: String::new(),
            sort_column: SortColumn::Cpu,
            sort_descending: true,
            selected_pid: None,
            focus_filter: false,
        }
    }
}

fn matches_filter(record: &ProcessRecord, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    record.name.to_lowercase().contains(filter)
        || record.command_line.to_lowercase().contains(filter)
        || record.pid.to_string() == filter
        || record.user.to_lowercase().contains(filter)
}

fn sort_records(records: &mut [&ProcessRecord], column: SortColumn, descending: bool) {
    use std::cmp::Ordering;
    let compare = |a: &&ProcessRecord, b: &&ProcessRecord| -> Ordering {
        let ordering = match column {
            SortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortColumn::Pid => a.pid.cmp(&b.pid),
            SortColumn::User => a.user.cmp(&b.user),
            SortColumn::Cpu => a
                .cpu_percent
                .partial_cmp(&b.cpu_percent)
                .unwrap_or(Ordering::Equal),
            SortColumn::Memory => a.memory_rss_bytes.cmp(&b.memory_rss_bytes),
            SortColumn::Gpu => a
                .gpu_busy_percent
                .partial_cmp(&b.gpu_busy_percent)
                .unwrap_or(Ordering::Equal),
            SortColumn::Vram => a.gpu_vram_bytes.cmp(&b.gpu_vram_bytes),
            SortColumn::Read => optional_f64(a.disk_read_bps).total_cmp(&optional_f64(b.disk_read_bps)),
            SortColumn::Write => {
                optional_f64(a.disk_write_bps).total_cmp(&optional_f64(b.disk_write_bps))
            }
            SortColumn::NetDown => optional_f64(a.net_rx_bps).total_cmp(&optional_f64(b.net_rx_bps)),
            SortColumn::NetUp => optional_f64(a.net_tx_bps).total_cmp(&optional_f64(b.net_tx_bps)),
            SortColumn::Threads => a.threads.cmp(&b.threads),
            SortColumn::Nice => a.nice.cmp(&b.nice),
            SortColumn::State => a.state.cmp(&b.state),
            SortColumn::Started => a
                .started_ts
                .partial_cmp(&b.started_ts)
                .unwrap_or(Ordering::Equal),
        };
        // Stable tiebreak so equal rows never shimmer between frames.
        ordering.then_with(|| a.pid.cmp(&b.pid))
    };
    records.sort_by(|a, b| {
        let ordering = compare(a, b);
        if descending { ordering.reverse() } else { ordering }
    });
}

fn optional_f64(value: Option<f64>) -> f64 {
    value.unwrap_or(-1.0)
}

fn blank_under(value: f64, threshold: f64, formatted: String) -> String {
    if value < threshold {
        String::new()
    } else {
        formatted
    }
}

pub fn processes_page(
    ui: &mut Ui,
    palette: &Palette,
    units: Units,
    snapshot: &SystemSnapshot,
    state: &mut ProcessTableState,
    icon_cache: &mut IconCache,
    actions: &mut Vec<AppAction>,
) {
    let Some(records) = &snapshot.processes else {
        ui.centered_and_justified(|ui| {
            ui.label(
                RichText::new("Gathering the first process snapshot…")
                    .color(palette.muted)
                    .italics(),
            );
        });
        return;
    };

    // ---- toolbar --------------------------------------------------
    ui.horizontal(|ui| {
        let filter_edit = egui::TextEdit::singleline(&mut state.filter)
            .hint_text("Filter by name, command, user, or PID…")
            .desired_width(ui.available_width() - 150.0);
        let filter_response = ui.add(filter_edit);
        if state.focus_filter {
            filter_response.request_focus();
            state.focus_filter = false;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let filter = state.filter.to_lowercase();
            let visible_count = records.iter().filter(|r| matches_filter(r, &filter)).count();
            let label = if filter.is_empty() {
                format!("{} processes", records.len())
            } else {
                format!("{visible_count} of {} processes", records.len())
            };
            ui.label(RichText::new(label).color(palette.muted).size(11.0));
        });
    });
    ui.add_space(2.0);

    // ---- rows ------------------------------------------------------
    let filter = state.filter.to_lowercase();
    let mut visible: Vec<&ProcessRecord> =
        records.iter().filter(|r| matches_filter(r, &filter)).collect();
    sort_records(&mut visible, state.sort_column, state.sort_descending);

    let text_height = 20.0f32;
    let columns: [(&str, SortColumn, f32); 15] = [
        ("Name", SortColumn::Name, 170.0),
        ("PID", SortColumn::Pid, 62.0),
        ("User", SortColumn::User, 64.0),
        ("CPU %", SortColumn::Cpu, 56.0),
        ("Memory", SortColumn::Memory, 74.0),
        ("GPU %", SortColumn::Gpu, 52.0),
        ("VRAM", SortColumn::Vram, 68.0),
        ("Read/s", SortColumn::Read, 74.0),
        ("Write/s", SortColumn::Write, 74.0),
        ("Net ↓/s", SortColumn::NetDown, 74.0),
        ("Net ↑/s", SortColumn::NetUp, 74.0),
        ("Thr", SortColumn::Threads, 44.0),
        ("Pri", SortColumn::Nice, 40.0),
        ("State", SortColumn::State, 56.0),
        ("Started", SortColumn::Started, 64.0),
    ];

    let now_ts = snapshot.ts;
    let mut table = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .min_scrolled_height(0.0);
    for (_, _, width) in &columns {
        table = table.column(Column::initial(*width).at_least(36.0).clip(true));
    }
    // Command gets the remainder.
    table = table.column(Column::remainder().at_least(120.0).clip(true));

    table
        .header(text_height, |mut header| {
            for (title, sort_column, _) in &columns {
                header.col(|ui| {
                    header_cell(ui, palette, title, Some(*sort_column), state);
                });
            }
            header.col(|ui| {
                header_cell(ui, palette, "Command", None, state);
            });
        })
        .body(|body| {
            body.rows(text_height, visible.len(), |mut row| {
                let record = visible[row.index()];
                row.set_selected(state.selected_pid == Some(record.pid));

                row.col(|ui| {
                    let icon_size = 15.0;
                    let (icon_rect, _) = ui.allocate_exact_size(
                        vec2(icon_size, icon_size),
                        Sense::hover(),
                    );
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
                    let name_text = if record.is_kernel_thread {
                        RichText::new(&record.name).color(palette.muted).size(12.0)
                    } else {
                        RichText::new(&record.name).size(12.0)
                    };
                    ui.add(egui::Label::new(name_text).truncate().selectable(false));
                });
                mono_cell(&mut row, record.pid.to_string(), palette, false);
                text_cell(&mut row, record.user.clone(), palette);
                mono_cell(
                    &mut row,
                    if record.cpu_percent >= 0.05 {
                        format!("{:.1}", record.cpu_percent)
                    } else {
                        String::new()
                    },
                    palette,
                    true,
                );
                mono_cell(
                    &mut row,
                    format_size(record.memory_rss_bytes, units),
                    palette,
                    true,
                );
                mono_cell(
                    &mut row,
                    if record.gpu_busy_percent >= 0.05 {
                        format!("{:.1}", record.gpu_busy_percent)
                    } else {
                        String::new()
                    },
                    palette,
                    true,
                );
                mono_cell(
                    &mut row,
                    if record.gpu_vram_bytes > 0 {
                        format_size(record.gpu_vram_bytes, units)
                    } else {
                        String::new()
                    },
                    palette,
                    true,
                );
                mono_cell(
                    &mut row,
                    match record.disk_read_bps {
                        Some(rate) => blank_under(rate, 1024.0, format_rate(rate, units)),
                        None => "—".to_string(),
                    },
                    palette,
                    true,
                );
                mono_cell(
                    &mut row,
                    match record.disk_write_bps {
                        Some(rate) => blank_under(rate, 1024.0, format_rate(rate, units)),
                        None => "—".to_string(),
                    },
                    palette,
                    true,
                );
                mono_cell(
                    &mut row,
                    match record.net_rx_bps {
                        Some(rate) => blank_under(rate, 1.0, format_rate(rate, units)),
                        None => String::new(),
                    },
                    palette,
                    true,
                );
                mono_cell(
                    &mut row,
                    match record.net_tx_bps {
                        Some(rate) => blank_under(rate, 1.0, format_rate(rate, units)),
                        None => String::new(),
                    },
                    palette,
                    true,
                );
                mono_cell(&mut row, record.threads.to_string(), palette, true);
                mono_cell(&mut row, format!("{}", record.nice), palette, true);
                text_cell(&mut row, record.state_word.clone(), palette);
                mono_cell(
                    &mut row,
                    format_duration_seconds((now_ts - record.started_ts).max(0.0)),
                    palette,
                    true,
                );
                row.col(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&record.command_line)
                                .color(palette.ink_2)
                                .monospace()
                                .size(11.0),
                        )
                        .truncate()
                        .selectable(false),
                    );
                });

                let row_response = row.response();
                if row_response.clicked() {
                    state.selected_pid = Some(record.pid);
                }
                if row_response.double_clicked() {
                    actions.push(AppAction::OpenDetails(record.pid));
                }
                process_context_menu(&row_response, actions, record);
            });
        });
}

fn header_cell(
    ui: &mut Ui,
    palette: &Palette,
    title: &str,
    sort_column: Option<SortColumn>,
    state: &mut ProcessTableState,
) {
    let is_active = sort_column == Some(state.sort_column);
    let arrow = if is_active {
        if state.sort_descending { " ↓" } else { " ↑" }
    } else {
        ""
    };
    let text = RichText::new(format!("{title}{arrow}"))
        .color(if is_active { palette.accent } else { palette.ink_2 })
        .monospace()
        .size(10.5)
        .strong();
    let response = ui.add(egui::Label::new(text).sense(Sense::click()).selectable(false));
    if response.clicked()
        && let Some(column) = sort_column
    {
        if state.sort_column == column {
            state.sort_descending = !state.sort_descending;
        } else {
            state.sort_column = column;
            state.sort_descending = matches!(
                column,
                SortColumn::Cpu
                    | SortColumn::Memory
                    | SortColumn::Gpu
                    | SortColumn::Vram
                    | SortColumn::Read
                    | SortColumn::Write
                    | SortColumn::NetDown
                    | SortColumn::NetUp
                    | SortColumn::Threads
                    | SortColumn::Started
            );
        }
    }
}

fn mono_cell(row: &mut egui_extras::TableRow, text: String, palette: &Palette, right: bool) {
    row.col(|ui| {
        let label = egui::Label::new(RichText::new(text).monospace().size(11.5))
            .truncate()
            .selectable(false);
        if right {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(label);
            });
        } else {
            ui.add(label);
        }
    });
    let _ = palette;
}

fn text_cell(row: &mut egui_extras::TableRow, text: String, palette: &Palette) {
    row.col(|ui| {
        ui.add(
            egui::Label::new(RichText::new(text).color(palette.ink_2).size(11.5))
                .truncate()
                .selectable(false),
        );
    });
}
