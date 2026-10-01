// SPDX-License-Identifier: GPL-3.0-or-later
//! The Processes page: v1's full table, grown — icon, name, PID,
//! user, CPU%, memory, GPU%, VRAM, disk read/write, net ↓/↑, threads,
//! priority, state, started, command. Every column sorts (whole-header
//! click target, painted ▲▼ on all of them), the table scrolls
//! horizontally when the columns outgrow the window, type to filter
//! (Ctrl+F focuses), right-click for actions, Ctrl+click multi-selects
//! up to five for the combined details view. Rows are virtualized;
//! sorting is stable so the table doesn't shimmer.
//!
//! New in 3.1: a summary strip that links back to the Overview, a
//! group-by-app view (one row per application, its processes summed),
//! and the Inspector docked on the right — click a row and its whole
//! story opens there.

use std::collections::HashMap;

use egui::{Align, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use egui_extras::{Column, TableBuilder};

use sysmon_core::snapshot::{ProcessRecord, SystemSnapshot};
use sysmon_core::units::{format_duration_seconds, format_percent, format_rate, format_size};

use super::cards::{AppAction, MAX_SELECTED, process_context_menu, selection_color, toggle_selection};
use super::glyphs::{self, Glyph};
use super::icons::{self, IconCache};
use super::inspector::{self, InspectorState};
use super::settings::Display;
use super::theme::Palette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    const ALL: [SortColumn; 16] = [
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
    ];

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
        SortColumn::ALL.into_iter().find(|column| column.id() == id)
    }

    /// Metrics read best biggest-first; identities read A→Z.
    pub fn defaults_descending(self) -> bool {
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
    /// One row per application instead of per process.
    pub group_by_app: bool,
    pub inspector: InspectorState,
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
            group_by_app: false,
            inspector: InspectorState::default(),
        }
    }
}

pub(crate) fn matches_filter(record: &ProcessRecord, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    record.name.to_lowercase().contains(filter)
        || record.display_name.to_lowercase().contains(filter)
        || record.command_line.to_lowercase().contains(filter)
        || record.pid.to_string() == filter
        || record.user.to_lowercase().contains(filter)
}

fn sort_records(records: &mut [&ProcessRecord], column: SortColumn, descending: bool) {
    use std::cmp::Ordering;
    let optional = |value: Option<f64>| value.unwrap_or(-1.0);
    let compare = |a: &&ProcessRecord, b: &&ProcessRecord| -> Ordering {
        let ordering = match column {
            SortColumn::Name => icons::display_name(a).to_lowercase().cmp(&icons::display_name(b).to_lowercase()),
            SortColumn::Pid => a.pid.cmp(&b.pid),
            SortColumn::User => a.user.cmp(&b.user),
            SortColumn::Cpu => a.cpu_percent.total_cmp(&b.cpu_percent),
            SortColumn::Memory => a.memory_rss_bytes.cmp(&b.memory_rss_bytes),
            SortColumn::Gpu => a.gpu_busy_percent.total_cmp(&b.gpu_busy_percent),
            SortColumn::Vram => a.gpu_vram_bytes.cmp(&b.gpu_vram_bytes),
            SortColumn::Read => optional(a.disk_read_bps).total_cmp(&optional(b.disk_read_bps)),
            SortColumn::Write => optional(a.disk_write_bps).total_cmp(&optional(b.disk_write_bps)),
            SortColumn::NetDown => optional(a.net_rx_bps).total_cmp(&optional(b.net_rx_bps)),
            SortColumn::NetUp => optional(a.net_tx_bps).total_cmp(&optional(b.net_tx_bps)),
            SortColumn::Threads => a.threads.cmp(&b.threads),
            SortColumn::Nice => a.nice.cmp(&b.nice),
            SortColumn::State => a.state.cmp(&b.state),
            SortColumn::Started => a.started_ts.total_cmp(&b.started_ts),
            SortColumn::Command => a.command_line.to_lowercase().cmp(&b.command_line.to_lowercase()),
        };
        // Stable tiebreak so equal rows never shimmer between frames.
        ordering.then_with(|| a.pid.cmp(&b.pid))
    };
    records.sort_by(|a, b| {
        let ordering = compare(a, b);
        if descending { ordering.reverse() } else { ordering }
    });
}

/// Group-by-app: one synthetic record per application, metrics
/// summed, `threads` holding the summed thread count, the app's
/// oldest process (its root) lending its pid, name and command. Keyed
/// by executable so "thorium ×12" collapses its renderer swarm.
pub fn group_by_app(records: &[&ProcessRecord]) -> Vec<(ProcessRecord, usize)> {
    let mut groups: HashMap<String, (ProcessRecord, usize)> = HashMap::new();
    for record in records {
        let key = if record.is_kernel_thread {
            "kernel threads".to_string()
        } else {
            record.exe_basename.clone().unwrap_or_else(|| icons::display_name(record).to_string())
        };
        let entry = groups.entry(key.clone()).or_insert_with(|| {
            let mut first = (*record).clone();
            first.cpu_percent = 0.0;
            first.memory_rss_bytes = 0;
            first.gpu_busy_percent = 0.0;
            first.gpu_vram_bytes = 0;
            first.threads = 0;
            first.disk_read_bps = None;
            first.disk_write_bps = None;
            first.net_rx_bps = None;
            first.net_tx_bps = None;
            if record.is_kernel_thread {
                first.display_name = key.clone();
            }
            (first, 0)
        });
        let (sum, count) = entry;
        *count += 1;
        sum.cpu_percent += record.cpu_percent;
        sum.memory_rss_bytes += record.memory_rss_bytes;
        sum.gpu_busy_percent += record.gpu_busy_percent;
        sum.gpu_vram_bytes += record.gpu_vram_bytes;
        sum.threads += record.threads;
        let add = |total: &mut Option<f64>, value: Option<f64>| {
            if let Some(value) = value {
                *total.get_or_insert(0.0) += value;
            }
        };
        add(&mut sum.disk_read_bps, record.disk_read_bps);
        add(&mut sum.disk_write_bps, record.disk_write_bps);
        add(&mut sum.net_rx_bps, record.net_rx_bps);
        add(&mut sum.net_tx_bps, record.net_tx_bps);
        // The oldest process is the app's root: it names the row.
        if record.started_ts < sum.started_ts && !record.is_kernel_thread {
            sum.pid = record.pid;
            sum.ppid = record.ppid;
            sum.display_name = icons::display_name(record).to_string();
            sum.command_line = record.command_line.clone();
            sum.started_ts = record.started_ts;
            sum.state = record.state.clone();
            sum.state_word = record.state_word.clone();
            sum.user = record.user.clone();
        }
    }
    groups.into_values().collect()
}

#[allow(clippy::too_many_arguments)]
pub fn processes_page(
    ui: &mut Ui,
    palette: &Palette,
    graph_palette_id: &str,
    display: Display,
    snapshot: &SystemSnapshot,
    state: &mut ProcessTableState,
    icon_cache: &mut IconCache,
    actions: &mut Vec<AppAction>,
    show_inspector: bool,
) {
    let Some(records) = &snapshot.processes else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("Gathering the first process snapshot…").color(palette.muted).italics());
        });
        return;
    };

    // Selection can outlive its processes — drop exited pids so the
    // count and the color slots stay honest.
    state.selected_pids.retain(|pid| records.iter().any(|r| r.pid == *pid));

    summary_strip(ui, palette, display, snapshot, actions);
    ui.add_space(2.0);
    toolbar(ui, palette, records, state, actions, show_inspector);
    ui.add_space(2.0);

    // The Inspector docks on the right when there's room for both.
    // Only then does the table get its own clipped panel (it must not
    // draw under the Inspector); otherwise it keeps the page's full
    // width, margins included, exactly as before.
    let wide_enough = ui.available_width() >= 760.0;
    if !(show_inspector && wide_enough) {
        table(ui, palette, graph_palette_id, display, snapshot, records, state, icon_cache, actions);
        return;
    }
    let selected = state.selected_pids.clone();
    egui::SidePanel::right("inspector_panel")
        .resizable(true)
        .default_width(330.0)
        .width_range(260.0..=520.0)
        .frame(egui::Frame::new().fill(palette.plane).inner_margin(egui::Margin::symmetric(6, 0)))
        .show_inside(ui, |ui| {
            inspector::inspector(
                ui,
                palette,
                graph_palette_id,
                display,
                snapshot,
                &mut state.inspector,
                icon_cache,
                &selected,
                actions,
            );
        });
    egui::CentralPanel::default()
        .frame(egui::Frame::new())
        .show_inside(ui, |ui| table(ui, palette, graph_palette_id, display, snapshot, records, state, icon_cache, actions));
}

/// One line of the machine at a glance, each figure a link back to its
/// Overview card — the other half of the Overview ↔ Processes bridge.
fn summary_strip(ui: &mut Ui, palette: &Palette, display: Display, snapshot: &SystemSnapshot, actions: &mut Vec<AppAction>) {
    ui.horizontal(|ui| {
        // One widget per chip (glyph + figure painted into a single
        // click rect) so nothing selectable sits on top of the target.
        let mut chip = |ui: &mut Ui, glyph: Glyph, text: String, section: &'static str| {
            let galley = ui.painter().layout_no_wrap(text, egui::FontId::monospace(11.0), palette.ink_2);
            let size = vec2(14.0 + galley.size().x + 6.0, galley.size().y.max(14.0) + 2.0);
            let (rect, response) = ui.allocate_exact_size(size, Sense::click());
            if ui.is_rect_visible(rect) {
                if response.hovered() {
                    ui.painter().rect_filled(rect, 0.0, palette.ink.gamma_multiply(0.06));
                }
                glyphs::paint(ui.painter(), glyph, pos2(rect.left() + 7.0, rect.center().y), 11.0, palette.muted);
                ui.painter().galley(
                    pos2(rect.left() + 16.0, rect.center().y - galley.size().y / 2.0),
                    galley,
                    palette.ink_2,
                );
            }
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Overview {section}"))
            });
            let response = response
                .on_hover_text(format!("Back to the Overview's {section} card"))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.clicked() {
                actions.push(AppAction::ShowOverview(section));
            }
            ui.add_space(6.0);
        };
        if let Some(cpu) = &snapshot.cpu {
            chip(ui, Glyph::Cpu, format_percent(cpu.overall_percent), "cpu");
        }
        if let Some(memory) = &snapshot.memory {
            chip(ui, Glyph::Memory, format_size(memory.used_bytes, display.units), "memory");
        }
        if let Some(gpu) = snapshot.gpu.as_ref().filter(|gpu| gpu.available) {
            chip(ui, Glyph::Gpu, format_percent(gpu.busy_percent), "gpu");
        }
        if let Some(network) = &snapshot.network {
            chip(ui, Glyph::Network, format!("↓{}", format_rate(network.download_bps, display.units)), "network");
        }
    });
}

fn toolbar(
    ui: &mut Ui,
    palette: &Palette,
    records: &[ProcessRecord],
    state: &mut ProcessTableState,
    actions: &mut Vec<AppAction>,
    show_inspector: bool,
) {
    ui.horizontal(|ui| {
        let filter_edit = egui::TextEdit::singleline(&mut state.filter)
            .hint_text("Filter by name, command, user, or PID…")
            .desired_width((ui.available_width() - 300.0).max(140.0));
        let filter_response = ui.add(filter_edit);
        if state.focus_filter {
            filter_response.request_focus();
            state.focus_filter = false;
        }
        let group_label = if state.group_by_app { "Grouped by app" } else { "Group by app" };
        if ui
            .selectable_label(state.group_by_app, RichText::new(group_label).size(11.0))
            .on_hover_text("One row per application, its processes summed (thorium ×12)")
            .clicked()
        {
            actions.push(AppAction::SetGroupByApp(!state.group_by_app));
        }
        let inspector_label = if show_inspector { "Inspector ›" } else { "‹ Inspector" };
        if ui
            .selectable_label(show_inspector, RichText::new(inspector_label).size(11.0))
            .on_hover_text("Show or hide the process Inspector (needs a wide window)")
            .clicked()
        {
            actions.push(AppAction::SetInspector(!show_inspector));
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
}

#[allow(clippy::too_many_arguments)]
fn table(
    ui: &mut Ui,
    palette: &Palette,
    graph_palette_id: &str,
    display: Display,
    snapshot: &SystemSnapshot,
    records: &[ProcessRecord],
    state: &mut ProcessTableState,
    icon_cache: &mut IconCache,
    actions: &mut Vec<AppAction>,
) {
    let units = display.units;
    let filter = state.filter.to_lowercase();
    let matching: Vec<&ProcessRecord> = records.iter().filter(|r| matches_filter(r, &filter)).collect();
    let grouped: Vec<(ProcessRecord, usize)>;
    let mut rows: Vec<(&ProcessRecord, usize)> = if state.group_by_app {
        grouped = group_by_app(&matching);
        grouped.iter().map(|(record, count)| (record, *count)).collect()
    } else {
        matching.iter().map(|record| (*record, 1)).collect()
    };
    {
        let mut refs: Vec<&ProcessRecord> = rows.iter().map(|(r, _)| *r).collect();
        sort_records(&mut refs, state.sort_column, state.sort_descending);
        let counts: HashMap<i32, usize> = rows.iter().map(|(r, c)| (r.pid, *c)).collect();
        rows = refs.into_iter().map(|r| (r, counts[&r.pid])).collect();
    }

    let text_height = 20.0f32;
    let threads_title = if state.group_by_app { "Procs" } else { "Thr" };
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
        (threads_title, SortColumn::Threads, 46.0),
        ("Pri", SortColumn::Nice, 40.0),
        ("State", SortColumn::State, 64.0),
        ("Started", SortColumn::Started, 64.0),
    ];

    let now_ts = snapshot.ts;
    let modifiers = ui.input(|input| input.modifiers);
    let blank_under = |value: f64, threshold: f64, formatted: String| if value < threshold { String::new() } else { formatted };

    // The table lives inside a horizontal scroll area (a real
    // horizontal scrollbar when the columns outgrow the window).
    // Inside it the width is unbounded, so the Command column absorbs
    // the spare width computed from the real viewport, floored at a
    // readable minimum.
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
                && let Some(index) = rows.iter().position(|(r, _)| r.pid == pid)
            {
                table = table.scroll_to_row(index, Some(Align::Center));
            }

            table
                .header(text_height, |mut header| {
                    for (title, sort_column, _) in &columns {
                        header.col(|ui| header_cell(ui, palette, title, Some(*sort_column), state));
                    }
                    header.col(|ui| header_cell(ui, palette, "Command", Some(SortColumn::Command), state));
                })
                .body(|body| {
                    body.rows(text_height, rows.len(), |mut row| {
                        let (record, count) = rows[row.index()];
                        let selection_slot = state.selected_pids.iter().position(|p| *p == record.pid);
                        let inspected = state.inspector.pid == Some(record.pid);
                        row.set_selected(selection_slot.is_some() || inspected);

                        row.col(|ui| {
                            // Selected rows carry their color slot as a
                            // spine — the same color the combined
                            // details window will use for this process.
                            if let Some(slot) = selection_slot {
                                let color = selection_color(graph_palette_id, palette.dark, slot);
                                let cell = ui.max_rect();
                                ui.painter().rect_filled(
                                    Rect::from_min_max(pos2(cell.min.x - 2.0, cell.min.y - 2.0), pos2(cell.min.x + 1.0, cell.max.y + 2.0)),
                                    0.0,
                                    color,
                                );
                            }
                            let (icon_rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
                            icon_cache.paint(ui, icon_rect, record, palette.muted);
                            let name = if count > 1 {
                                format!("{} ×{count}", icons::display_name(record))
                            } else {
                                icons::display_name(record).to_string()
                            };
                            let name_text = if record.is_kernel_thread {
                                RichText::new(name).color(palette.muted).size(12.0)
                            } else {
                                RichText::new(name).size(12.0)
                            };
                            ui.add(egui::Label::new(name_text).truncate().selectable(false));
                        });
                        mono_cell(&mut row, record.pid.to_string(), false);
                        text_cell(&mut row, record.user.clone(), palette);
                        mono_cell(&mut row, blank_under(record.cpu_percent as f64, 0.05, format!("{:.1}", record.cpu_percent)), true);
                        mono_cell(&mut row, format_size(record.memory_rss_bytes, units), true);
                        mono_cell(&mut row, blank_under(record.gpu_busy_percent as f64, 0.05, format!("{:.1}", record.gpu_busy_percent)), true);
                        mono_cell(&mut row, blank_under(record.gpu_vram_bytes as f64, 1.0, format_size(record.gpu_vram_bytes, units)), true);
                        for value in [record.disk_read_bps, record.disk_write_bps] {
                            mono_cell(
                                &mut row,
                                match value {
                                    Some(rate) => blank_under(rate, 1024.0, format_rate(rate, units)),
                                    None => "—".to_string(),
                                },
                                true,
                            );
                        }
                        for value in [record.net_rx_bps, record.net_tx_bps] {
                            mono_cell(
                                &mut row,
                                value.map(|rate| blank_under(rate, 1.0, format_rate(rate, units))).unwrap_or_default(),
                                true,
                            );
                        }
                        mono_cell(&mut row, if count > 1 { count.to_string() } else { record.threads.to_string() }, true);
                        mono_cell(&mut row, format!("{}", record.nice), true);
                        row.col(|ui| {
                            glyphs::show(ui, glyphs::for_process_state(&record.state), 10.0, palette.muted);
                            ui.add(egui::Label::new(RichText::new(&record.state_word).color(palette.ink_2).size(11.5)).truncate().selectable(false));
                        });
                        mono_cell(&mut row, format_duration_seconds((now_ts - record.started_ts).max(0.0)), true);
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&record.command_line).color(palette.ink_2).monospace().size(11.0))
                                    .truncate()
                                    .selectable(false),
                            );
                        });

                        let row_response = row.response();
                        if row_response.clicked() {
                            if modifiers.command || modifiers.ctrl {
                                toggle_selection(&mut state.selected_pids, record.pid, actions);
                            } else if modifiers.shift {
                                let visible: Vec<&ProcessRecord> = rows.iter().map(|(r, _)| *r).collect();
                                range_select(state, &visible, record.pid, actions);
                            } else {
                                state.selected_pids = vec![record.pid];
                                actions.push(AppAction::Inspect(record.pid));
                            }
                        }
                        if row_response.double_clicked() {
                            actions.push(AppAction::OpenDetails(record.pid));
                        }
                        process_context_menu(&row_response, actions, record, &state.selected_pids, true);
                    });
                });
        });
}

/// Shift+click: select the visible span from the selection anchor
/// (last selected row) to the clicked row, oldest-first, capped at
/// MAX_SELECTED with a toast when the span is longer.
fn range_select(state: &mut ProcessTableState, visible: &[&ProcessRecord], clicked_pid: i32, actions: &mut Vec<AppAction>) {
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
fn header_cell(ui: &mut Ui, palette: &Palette, title: &str, sort_column: Option<SortColumn>, state: &mut ProcessTableState) {
    let is_active = sort_column == Some(state.sort_column);
    let width = ui.available_width().max(24.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click());
    let direction = if state.sort_descending { " (descending)" } else { " (ascending)" };
    let sort_label = format!("Sort by {title}{}", if is_active { direction } else { "" });
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, sort_label.clone()));
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
            let lit = |up: bool| {
                if is_active && state.sort_descending != up {
                    palette.accent
                } else {
                    palette.muted.gamma_multiply(0.55)
                }
            };
            let cy = rect.center().y;
            for (up, tip, base) in [(true, cy - 4.6, cy - 1.4), (false, cy + 4.6, cy + 1.4)] {
                painter.add(egui::Shape::convex_polygon(
                    vec![pos2(x - 2.6, base), pos2(x + 2.6, base), pos2(x, tip)],
                    lit(up),
                    Stroke::NONE,
                ));
            }
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

fn mono_cell(row: &mut egui_extras::TableRow, text: String, right: bool) {
    row.col(|ui| {
        let label = egui::Label::new(RichText::new(text).monospace().size(11.5)).truncate().selectable(false);
        if right {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(label);
            });
        } else {
            ui.add(label);
        }
    });
}

fn text_cell(row: &mut egui_extras::TableRow, text: String, palette: &Palette) {
    row.col(|ui| {
        ui.add(egui::Label::new(RichText::new(text).color(palette.ink_2).size(11.5)).truncate().selectable(false));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_(pid: i32, exe: &str, cpu: f32, rss: u64, started: f64) -> ProcessRecord {
        ProcessRecord {
            pid,
            name: exe.into(),
            display_name: exe.into(),
            exe_basename: Some(exe.into()),
            cpu_percent: cpu,
            memory_rss_bytes: rss,
            threads: 4,
            started_ts: started,
            command_line: format!("/usr/bin/{exe}"),
            ..Default::default()
        }
    }

    #[test]
    fn group_by_app_sums_and_names_by_the_oldest() {
        let records = [
            proc_(200, "thorium", 3.0, 100, 50.0),
            proc_(100, "thorium", 1.0, 300, 10.0),
            proc_(300, "thorium", 2.0, 200, 70.0),
            proc_(400, "sway", 5.0, 150, 5.0),
        ];
        let refs: Vec<&ProcessRecord> = records.iter().collect();
        let mut groups = group_by_app(&refs);
        groups.sort_by_key(|(r, _)| r.pid);
        let (thorium, count) = groups.iter().find(|(r, _)| r.exe_basename.as_deref() == Some("thorium")).unwrap();
        assert_eq!(*count, 3);
        assert_eq!(thorium.pid, 100, "the oldest process names the app");
        assert_eq!(thorium.cpu_percent, 6.0);
        assert_eq!(thorium.memory_rss_bytes, 600);
        assert_eq!(thorium.threads, 12);
    }

    #[test]
    fn sort_ids_round_trip() {
        for column in SortColumn::ALL {
            assert_eq!(SortColumn::from_id(column.id()), Some(column));
        }
    }
}
