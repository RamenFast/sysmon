// SPDX-License-Identifier: GPL-3.0-or-later
//! The Processes page: v1's full table, grown — icon, name, PID,
//! user, CPU%, memory, GPU%, VRAM, disk read/write, net ↓/↑ (split,
//! new), threads (new), priority, state (new), started (new),
//! command. Every column sorts (whole-header click target, painted
//! ▲▼ affordance on all of them — Ben's ask), the table scrolls
//! horizontally when the columns outgrow the window, type to filter
//! (Ctrl+F focuses), right-click for actions, double-click for
//! details, Ctrl+click multi-selects up to five for the combined
//! details view. Rows are virtualized; sorting is stable so the
//! table doesn't shimmer.

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use egui_extras::{Column, TableBuilder};

use sysmon_core::snapshot::{ProcessRecord, SystemSnapshot};
use sysmon_core::units::{Units, format_duration_seconds, format_rate, format_size};

use super::cards::{
    AppAction, MAX_SELECTED, process_context_menu, selection_color, toggle_selection,
};
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
    Command,
}

impl SortColumn {
    /// Stable ids for the settings file (survive enum reshuffles).
    pub fn id(self) -> &'static str {
        match self {
            SortColumn::Name => "name",
            SortColumn::Pid => "pid",
            SortColumn::User => "user",
            SortColumn::Cpu => "cpu",
            SortColumn::Memory => "memory",
            SortColumn::Gpu => "gpu",
            SortColumn::Vram => "vram",
            SortColumn::Read => "read",
            SortColumn::Write => "write",
            SortColumn::NetDown => "net_down",
            SortColumn::NetUp => "net_up",
            SortColumn::Threads => "threads",
            SortColumn::Nice => "nice",
            SortColumn::State => "state",
            SortColumn::Started => "started",
            SortColumn::Command => "command",
        }
    }

    pub fn from_id(id: &str) -> Option<SortColumn> {
        [
            SortColumn::Name,
            SortColumn::Pid,
            SortColumn::User,
            SortColumn::Cpu,
            SortColumn::Memory,
            SortColumn::Gpu,
            SortColumn::Vram,
            SortColumn::Read,
            SortColumn::Write,
            SortColumn::NetDown,
            SortColumn::NetUp,
            SortColumn::Threads,
            SortColumn::Nice,
            SortColumn::State,
            SortColumn::Started,
            SortColumn::Command,
        ]
        .into_iter()
        .find(|column| column.id() == id)
    }

    /// Metrics read best biggest-first; identities read A→Z.
    fn defaults_descending(self) -> bool {
        matches!(
            self,
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
        )
    }
}

pub struct ProcessTableState {
    pub filter: String,
    pub sort_column: SortColumn,
    pub sort_descending: bool,
    /// Multi-selection (≤ MAX_SELECTED), shared with the overview
    /// rows; order is selection order and fixes each pid's color slot.
    pub selected_pids: Vec<i32>,
    pub focus_filter: bool,
    /// One-shot: scroll this pid into view (set by "Open in process
    /// viewer" from the overview).
    pub reveal_pid: Option<i32>,
}

impl Default for ProcessTableState {
    fn default() -> Self {
        ProcessTableState {
            filter: String::new(),
            sort_column: SortColumn::Cpu,
            sort_descending: true,
            selected_pids: Vec::new(),
            focus_filter: false,
            reveal_pid: None,
        }
    }
}

pub(crate) fn matches_filter(record: &ProcessRecord, filter: &str) -> bool {
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
            SortColumn::Command => a
                .command_line
                .to_lowercase()
                .cmp(&b.command_line.to_lowercase()),
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

#[allow(clippy::too_many_arguments)]
pub fn processes_page(
    ui: &mut Ui,
    palette: &Palette,
    graph_palette_id: &str,
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

    // Selection can outlive its processes — drop exited pids so the
    // count and the color slots stay honest.
    state
        .selected_pids
        .retain(|pid| records.iter().any(|r| r.pid == *pid));

    // ---- toolbar --------------------------------------------------
    ui.horizontal(|ui| {
        let filter_edit = egui::TextEdit::singleline(&mut state.filter)
            .hint_text("Filter by name, command, user, or PID…")
            .desired_width(ui.available_width() - 240.0);
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
            if state.selected_pids.len() >= 2
                && ui
                    .button(format!("Compare ({})", state.selected_pids.len()))
                    .on_hover_text("Combined details for the selected processes")
                    .clicked()
            {
                actions.push(AppAction::OpenCombinedDetails(state.selected_pids.clone()));
            }
            if !state.selected_pids.is_empty() {
                ui.label(
                    RichText::new(format!("{} selected ·", state.selected_pids.len()))
                        .color(palette.accent)
                        .size(11.0),
                )
                .on_hover_text("Ctrl+click rows to select · Esc clears");
            }
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
    let modifiers = ui.input(|input| input.modifiers);

    // The table lives inside a horizontal scroll area (Ben's ask: a
    // real horizontal scrollbar when the columns outgrow the window).
    // Inside it the width is unbounded, so Column::remainder() would
    // misbehave — the Command column instead absorbs the spare width
    // computed from the real viewport, floored at a readable minimum.
    let viewport_width = ui.available_width();
    egui::ScrollArea::horizontal()
        .id_salt("process_table_hscroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let spacing_x = ui.spacing().item_spacing.x;
            let fixed: f32 = columns.iter().map(|(_, _, width)| *width).sum::<f32>()
                + spacing_x * (columns.len() as f32 + 1.0)
                + ui.spacing().scroll.allocated_width();
            let command_width = (viewport_width - fixed).max(240.0);

            let mut table = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .min_scrolled_height(0.0);
            for (_, _, width) in &columns {
                table = table.column(Column::initial(*width).at_least(36.0).clip(true));
            }
            table = table.column(Column::initial(command_width).at_least(200.0).clip(true));

            if let Some(pid) = state.reveal_pid.take()
                && let Some(index) = visible.iter().position(|r| r.pid == pid)
            {
                table = table.scroll_to_row(index, Some(Align::Center));
            }

            table
                .header(text_height, |mut header| {
                    for (title, sort_column, _) in &columns {
                        header.col(|ui| {
                            header_cell(ui, palette, title, Some(*sort_column), state);
                        });
                    }
                    header.col(|ui| {
                        header_cell(ui, palette, "Command", Some(SortColumn::Command), state);
                    });
                })
                .body(|body| {
                    body.rows(text_height, visible.len(), |mut row| {
                        let record = visible[row.index()];
                        let selection_slot =
                            state.selected_pids.iter().position(|p| *p == record.pid);
                        row.set_selected(selection_slot.is_some());

                        row.col(|ui| {
                            // Selected rows carry their color slot as a
                            // spine — the same color the combined
                            // details window will use for this process.
                            if let Some(slot) = selection_slot {
                                let color =
                                    selection_color(graph_palette_id, palette.dark, slot);
                                let cell = ui.max_rect();
                                ui.painter().rect_filled(
                                    Rect::from_min_max(
                                        pos2(cell.min.x - 2.0, cell.min.y - 2.0),
                                        pos2(cell.min.x + 1.0, cell.max.y + 2.0),
                                    ),
                                    0.0,
                                    color,
                                );
                            }
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
                            if modifiers.command || modifiers.ctrl {
                                toggle_selection(
                                    &mut state.selected_pids,
                                    record.pid,
                                    actions,
                                );
                            } else if modifiers.shift {
                                range_select(state, &visible, record.pid, actions);
                            } else {
                                state.selected_pids = vec![record.pid];
                            }
                        }
                        if row_response.double_clicked() {
                            actions.push(AppAction::OpenDetails(record.pid));
                        }
                        process_context_menu(
                            &row_response,
                            actions,
                            record,
                            &state.selected_pids,
                            true,
                        );
                    });
                });
        });
}

/// Shift+click: select the visible span from the selection anchor
/// (last selected row) to the clicked row, oldest-first, capped at
/// MAX_SELECTED with a toast when the span is longer.
fn range_select(
    state: &mut ProcessTableState,
    visible: &[&ProcessRecord],
    clicked_pid: i32,
    actions: &mut Vec<AppAction>,
) {
    let anchor_pid = match state.selected_pids.last() {
        Some(pid) => *pid,
        None => {
            state.selected_pids = vec![clicked_pid];
            return;
        }
    };
    let (Some(anchor), Some(clicked)) = (
        visible.iter().position(|r| r.pid == anchor_pid),
        visible.iter().position(|r| r.pid == clicked_pid),
    ) else {
        state.selected_pids = vec![clicked_pid];
        return;
    };
    let span: Vec<i32> = if anchor <= clicked {
        (anchor..=clicked).map(|i| visible[i].pid).collect()
    } else {
        (clicked..=anchor).rev().map(|i| visible[i].pid).collect()
    };
    if span.len() > MAX_SELECTED {
        actions.push(AppAction::Notify(format!(
            "Range is {} rows — keeping the first {MAX_SELECTED} from the anchor",
            span.len()
        )));
    }
    state.selected_pids = span.into_iter().take(MAX_SELECTED).collect();
}

/// A whole-cell sortable header: the full column width is the click
/// target, every sortable column shows the ▲▼ pair (painted — the
/// glyphs aren't in the proportional font), and the active direction
/// carries the accent.
fn header_cell(
    ui: &mut Ui,
    palette: &Palette,
    title: &str,
    sort_column: Option<SortColumn>,
    state: &mut ProcessTableState,
) {
    let is_active = sort_column == Some(state.sort_column);
    let width = ui.available_width().max(24.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click());
    let sort_label = format!(
        "Sort by {title}{}",
        if is_active {
            if state.sort_descending { " (descending)" } else { " (ascending)" }
        } else {
            ""
        }
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, sort_label.clone())
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_filled(rect, 0.0, palette.ink.gamma_multiply(0.06));
        }
        let text_color = if is_active { palette.accent } else { palette.ink_2 };
        painter.text(
            pos2(rect.left() + 2.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::monospace(10.5),
            text_color,
        );
        if sort_column.is_some() {
            // ▲▼ pair at the right edge; the live direction lights up.
            let x = rect.right() - 6.0;
            let up_color = if is_active && !state.sort_descending {
                palette.accent
            } else {
                palette.muted.gamma_multiply(0.55)
            };
            let down_color = if is_active && state.sort_descending {
                palette.accent
            } else {
                palette.muted.gamma_multiply(0.55)
            };
            let cy = rect.center().y;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(x - 2.6, cy - 1.4),
                    pos2(x + 2.6, cy - 1.4),
                    pos2(x, cy - 4.6),
                ],
                up_color,
                Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(x - 2.6, cy + 1.4),
                    pos2(x + 2.6, cy + 1.4),
                    pos2(x, cy + 4.6),
                ],
                down_color,
                Stroke::NONE,
            ));
        }
    }
    if response.clicked()
        && let Some(column) = sort_column
    {
        if state.sort_column == column {
            state.sort_descending = !state.sort_descending;
        } else {
            state.sort_column = column;
            state.sort_descending = column.defaults_descending();
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
