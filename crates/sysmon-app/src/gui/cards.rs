// SPDX-License-Identifier: GPL-3.0-or-later
//! The Overview cards: GPU, Memory, CPU, Network, Disks, Sensors —
//! v1's information priority in the house chrome. One function per
//! card over the latest snapshot, drawn by [`draw_card`] for both the
//! main page and the pop-out windows. Cards emit AppActions (kill this
//! pid, open details, toggle pop-out, open the table sorted by…) that
//! the app applies after the frame — no borrow tangles.

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Ui, pos2, vec2};

use sysmon_core::snapshot::{ProcessNetSource, ProcessRecord, SystemSnapshot};
use sysmon_core::units::{
    self, format_frequency_mhz, format_percent, format_power, format_rate, format_size,
    format_temperature_in,
};

use super::glyphs::{self, Glyph};
use super::graphs::{self, GraphConfig, GraphStyle, History};
use super::icons::{self, IconCache};
use super::processes::SortColumn;
use super::sensors_view;
use super::settings::{Display, Settings};
use super::theme::{
    GRAPH_SERIES_CPU, GRAPH_SERIES_GPU, GRAPH_SERIES_MEMORY, GRAPH_SERIES_NET_DOWN,
    GRAPH_SERIES_NET_UP, Palette, card_frame, graph_color,
};
use super::widgets::{ButtonGlyph, glyph_button};

pub const FULL_GRAPH_HEIGHT: f32 = 66.0;
pub const COMPACT_GRAPH_HEIGHT: f32 = 36.0;

/// A Zen CPU's Tctl throttles near 90–95 °C and k10temp states no
/// limit, so the card warns on its own from here.
const CPU_HOT_CELSIUS: f32 = 85.0;

/// Everything a card needs for one frame.
pub struct CardContext<'a> {
    pub palette: &'a Palette,
    pub graph_palette_id: &'a str,
    pub display: Display,
    pub compact: bool,
    pub snapshot: &'a SystemSnapshot,
    pub icon_cache: &'a mut IconCache,
    pub actions: &'a mut Vec<AppAction>,
    pub popped_out: &'a [String],
    /// The app-wide multi-selection (shared with the process table) —
    /// Ctrl+click on any top-process row joins it.
    pub selected: &'a mut Vec<i32>,
    /// Sensors-card group fold state (read-only here; toggles go out
    /// as actions).
    pub folded_sensor_groups: &'a [String],
}

/// Deferred effects a frame's widgets request.
#[derive(Clone, Debug)]
pub enum AppAction {
    TogglePopOut(&'static str),
    OpenDetails(i32),
    /// Combined details for a multi-selection (≤ MAX_SELECTED pids).
    OpenCombinedDetails(Vec<i32>),
    /// Jump to the Processes page with this pid selected + scrolled to.
    RevealInProcesses(i32),
    /// Jump to the Processes page sorted by a column ("more ›").
    ShowProcessesBy(SortColumn),
    /// Back to the Overview, scrolled to a card (the summary strip).
    ShowOverview(&'static str),
    /// Select a pid in the Inspector (and the table).
    Inspect(i32),
    SetGroupByApp(bool),
    SetInspector(bool),
    ConfirmTerminate(i32, String),
    ConfirmKill(i32, String),
    SetPriority(i32, String, i32),
    CopyPid(i32),
    ToggleSensorGroup(String),
    /// Surface a short toast (selection full, etc.).
    Notify(String),
}

/// The multi-select ceiling (Ben's spec: up to 5) — also exactly the
/// number of semantic series colors a graph palette carries, so every
/// selected process owns a stable color.
pub const MAX_SELECTED: usize = 5;

/// The color a selected process wears everywhere (row tint, combined
/// details): its slot in the active graph palette's five series.
pub fn selection_color(graph_palette_id: &str, dark: bool, slot: usize) -> Color32 {
    graph_color(graph_palette_id, slot.min(MAX_SELECTED - 1), dark)
}

/// Toggle a pid in the shared multi-selection, honoring the ceiling.
pub fn toggle_selection(selected: &mut Vec<i32>, pid: i32, actions: &mut Vec<AppAction>) {
    if let Some(index) = selected.iter().position(|p| *p == pid) {
        selected.remove(index);
    } else if selected.len() < MAX_SELECTED {
        selected.push(pid);
    } else {
        actions.push(AppAction::Notify(format!(
            "Selection is full ({MAX_SELECTED}) — deselect one first (Ctrl+click)"
        )));
    }
}

/// Which card, with the histories it graphs.
pub enum Card<'a> {
    Gpu(&'a History),
    Memory(&'a History),
    Cpu(&'a History),
    Network(&'a History, &'a History),
    Disks,
    Sensors,
}

/// The one place a card is drawn (overview and pop-outs alike).
pub fn draw_card(ui: &mut Ui, cx: &mut CardContext, card: Card) {
    card_frame(cx.palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match card {
            Card::Gpu(history) => gpu_card(ui, cx, history),
            Card::Memory(history) => memory_card(ui, cx, history),
            Card::Cpu(history) => cpu_card(ui, cx, history),
            Card::Network(down, up) => network_card(ui, cx, down, up),
            Card::Disks => disks_card(ui, cx),
            Card::Sensors => sensors_card(ui, cx),
        }
    });
}

/// The card for a section key, borrowing the right histories.
pub fn card_for<'a>(section: &str, histories: &'a super::app::Histories) -> Card<'a> {
    match section {
        "gpu" => Card::Gpu(&histories.gpu),
        "memory" => Card::Memory(&histories.memory),
        "cpu" => Card::Cpu(&histories.cpu),
        "network" => Card::Network(&histories.net_down, &histories.net_up),
        "disks" => Card::Disks,
        _ => Card::Sensors,
    }
}

impl<'a> CardContext<'a> {
    /// The context for one frame, from the app's settings.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        palette: &'a Palette,
        settings: &'a Settings,
        snapshot: &'a SystemSnapshot,
        icon_cache: &'a mut IconCache,
        actions: &'a mut Vec<AppAction>,
        selected: &'a mut Vec<i32>,
        compact: bool,
    ) -> Self {
        CardContext {
            palette,
            graph_palette_id: &settings.graph_palette,
            display: settings.display(),
            compact,
            snapshot,
            icon_cache,
            actions,
            popped_out: &settings.popped_out_sections,
            selected,
            folded_sensor_groups: &settings.folded_sensor_groups,
        }
    }

    fn size(&self, bytes: u64) -> String {
        format_size(bytes, self.display.units)
    }

    fn rate(&self, bytes_per_second: f64) -> String {
        format_rate(bytes_per_second, self.display.units)
    }

    fn temperature(&self, celsius: f32) -> String {
        format_temperature_in(celsius, self.display.temperature)
    }
}

// ------------------------------------------------------------ card chrome

/// Header row: glyph · title · subtitle … headline · pop-out toggle.
fn card_header(
    ui: &mut Ui,
    cx: &mut CardContext,
    section_key: &'static str,
    title: &str,
    subtitle: &str,
    headline: &str,
) {
    ui.horizontal(|ui| {
        glyphs::show(ui, glyphs::for_section(section_key), 14.0, cx.palette.title);
        ui.label(RichText::new(title).color(cx.palette.title).strong().size(13.0));
        if !subtitle.is_empty() && !cx.compact {
            // Give the subtitle only what the right-side headline +
            // pop-out button won't need, so it truncates instead of
            // running underneath them.
            let headline_width = ui.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(headline.to_string(), egui::FontId::monospace(12.5), cx.palette.value)
                    .size()
                    .x
            });
            let reserved = headline_width + 26.0 + 24.0; // button + spacing
            let subtitle_width = (ui.available_width() - reserved).max(0.0);
            ui.allocate_ui_with_layout(
                egui::vec2(subtitle_width, 16.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(subtitle).color(cx.palette.muted).size(11.0))
                            .truncate(),
                    );
                },
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let popped = cx.popped_out.iter().any(|s| s == section_key);
            let tooltip = if popped {
                "Return to the main window"
            } else {
                "Pop out into its own window"
            };
            if glyph_button(ui, cx.palette, ButtonGlyph::PopOut, popped, tooltip).clicked() {
                cx.actions.push(AppAction::TogglePopOut(section_key));
            }
            ui.label(RichText::new(headline).color(cx.palette.value).monospace().size(12.5));
        });
    });
}

/// Two-column grid of dim label / mono value pairs (v1's StatGrid).
/// A value may carry a hover note (label, value, Some(why)).
fn stat_grid(ui: &mut Ui, palette: &Palette, stats: &[(&str, String, Option<String>)]) {
    let column_count = 2;
    egui::Grid::new(ui.next_auto_id())
        .num_columns(column_count * 2)
        .spacing(vec2(10.0, 2.0))
        .show(ui, |ui| {
            for (index, (label, value, note)) in stats.iter().enumerate() {
                ui.label(RichText::new(*label).color(palette.muted).monospace().size(10.5));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let response = ui.label(RichText::new(value).monospace().size(11.5));
                    if let Some(note) = note {
                        response.on_hover_text(note);
                    }
                });
                if index % column_count == column_count - 1 {
                    ui.end_row();
                }
            }
        });
}

fn stat(label: &'static str, value: String) -> (&'static str, String, Option<String>) {
    (label, value, None)
}

fn graph_height(cx: &CardContext) -> f32 {
    if cx.compact {
        COMPACT_GRAPH_HEIGHT
    } else {
        FULL_GRAPH_HEIGHT
    }
}

/// A quiet "more ›" link under a top-3 list: the whole table, sorted
/// by what this list ranks by. The bridge from Overview to Processes.
fn more_link(ui: &mut Ui, cx: &mut CardContext, column: SortColumn, what: &str) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let text = RichText::new(format!("all by {what}")).color(cx.palette.muted).size(10.5);
            let response = ui.add(egui::Label::new(text).sense(Sense::click()));
            let arrow = response.rect.left_center() - vec2(8.0, 0.0);
            glyphs::paint(ui.painter(), Glyph::ArrowRight, arrow, 9.0, cx.palette.muted);
            let response = response
                .on_hover_text(format!("Open Processes sorted by {what}"))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Processes by {what}"))
            });
            if response.clicked() {
                cx.actions.push(AppAction::ShowProcessesBy(column));
            }
        });
    });
}

/// One top-process row: rank · icon · name … value. Right-click for
/// actions, click to inspect, Ctrl+click to multi-select.
fn top_process_row(ui: &mut Ui, cx: &mut CardContext, rank: usize, record: &ProcessRecord, value_text: String) {
    let row_height = 18.0;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::click());
    let name = icons::display_name(record).to_string();
    // Assistive tech (and kittest) sees each row as a named button.
    let a11y_label = format!("{name} — PID {}", record.pid);
    response.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y_label.clone()));
    if ui.is_rect_visible(rect) {
        // Multi-selected rows wear their slot color (the same color
        // the combined-details window uses for this process).
        if let Some(slot) = cx.selected.iter().position(|p| *p == record.pid) {
            let color = selection_color(cx.graph_palette_id, cx.palette.dark, slot);
            ui.painter().rect_filled(rect, 0.0, color.gamma_multiply(0.12));
            ui.painter()
                .rect_filled(Rect::from_min_max(rect.min, pos2(rect.min.x + 3.0, rect.max.y)), 0.0, color);
        }
        if response.hovered() {
            ui.painter().rect_filled(rect, 0.0, cx.palette.ink.gamma_multiply(0.05));
        }
        let painter = ui.painter();
        let mut x = rect.left() + 2.0;
        painter.text(
            pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{rank}"),
            egui::FontId::monospace(10.5),
            cx.palette.muted.gamma_multiply(0.8),
        );
        x += 14.0;
        let icon_rect = Rect::from_center_size(pos2(x + 7.0, rect.center().y), vec2(14.0, 14.0));
        cx.icon_cache.paint(ui, icon_rect, record, cx.palette.muted);
        x += 20.0;

        let painter = ui.painter();
        let value_galley = painter.layout_no_wrap(
            value_text,
            egui::FontId::monospace(11.5),
            cx.palette.ink.gamma_multiply(0.95),
        );
        let value_width = value_galley.size().x + 8.0;
        painter.galley(
            pos2(rect.right() - 2.0 - value_galley.size().x, rect.center().y - value_galley.size().y / 2.0),
            value_galley,
            cx.palette.ink,
        );
        let name_width = (rect.right() - value_width - x).max(20.0);
        let name_galley = painter.layout(
            name,
            egui::FontId::proportional(12.0),
            cx.palette.ink,
            f32::INFINITY,
        );
        let name_galley = if name_galley.size().x > name_width {
            let mut job = egui::text::LayoutJob::simple_singleline(
                icons::display_name(record).to_string(),
                egui::FontId::proportional(12.0),
                cx.palette.ink,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(name_width);
            painter.layout_job(job)
        } else {
            name_galley
        };
        painter.galley(pos2(x, rect.center().y - name_galley.size().y / 2.0), name_galley, cx.palette.ink);
    }

    let response = response.on_hover_text(format!(
        "{} · PID {} — click to inspect, Ctrl+click to select, right-click for actions",
        record.name, record.pid
    ));
    if response.clicked() {
        let modifiers = ui.input(|input| input.modifiers);
        if modifiers.command || modifiers.ctrl {
            toggle_selection(cx.selected, record.pid, cx.actions);
        } else {
            cx.actions.push(AppAction::Inspect(record.pid));
        }
    }
    process_context_menu(&response, cx.actions, record, cx.selected, false);
}

/// What a right-click menu acts on, captured the moment it opens.
#[derive(Clone)]
struct MenuTarget {
    pid: i32,
    name: String,
    nice: i32,
    /// The multi-selection at open time, when the target is part of it.
    combined: Option<Vec<i32>>,
}

/// The shared right-click menu (top-3 rows, the process table, the
/// inspector, and the combined-details blocks). `in_process_table`
/// hides the "Open in process viewer" jump when the row already lives
/// there.
///
/// egui keys a context menu to the widget that opened it, and the
/// overview's rows are *slots* re-ranked every sample: without a
/// latch, a busier process taking the slot while the menu is open
/// inherits the menu (End process included). The process is latched
/// on the right-click and every item acts on the latch.
pub fn process_context_menu(
    response: &egui::Response,
    actions: &mut Vec<AppAction>,
    record: &ProcessRecord,
    selected: &[i32],
    in_process_table: bool,
) {
    let latch_id = egui::Popup::default_response_id(response).with("menu_target");
    if response.secondary_clicked() {
        let target = MenuTarget {
            pid: record.pid,
            name: icons::display_name(record).to_string(),
            nice: record.nice,
            combined: (selected.len() >= 2 && selected.contains(&record.pid)).then(|| selected.to_vec()),
        };
        response.ctx.data_mut(|data| data.insert_temp(latch_id, target));
    }
    let MenuTarget { pid, name, nice, combined } = response
        .ctx
        .data(|data| data.get_temp::<MenuTarget>(latch_id))
        .unwrap_or_else(|| MenuTarget {
            pid: record.pid,
            name: icons::display_name(record).to_string(),
            nice: record.nice,
            combined: None,
        });
    response.context_menu(|ui| {
        ui.label(RichText::new(format!("{name}  (PID {pid})")).monospace().size(11.5));
        if let Some(pids) = &combined {
            ui.separator();
            if ui.button(format!("Combined details ({} selected)…", pids.len())).clicked() {
                actions.push(AppAction::OpenCombinedDetails(pids.clone()));
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Inspect").clicked() {
            actions.push(AppAction::Inspect(pid));
            ui.close();
        }
        if ui.button("Details window…").clicked() {
            actions.push(AppAction::OpenDetails(pid));
            ui.close();
        }
        if !in_process_table && ui.button("Open in process viewer").clicked() {
            actions.push(AppAction::RevealInProcesses(pid));
            ui.close();
        }
        if ui.button("Copy PID").clicked() {
            actions.push(AppAction::CopyPid(pid));
            ui.close();
        }
        ui.separator();
        ui.menu_button("Set priority", |ui| {
            for (label, value) in [("Very High", -12), ("High", -5), ("Normal", 0), ("Low", 10), ("Very Low", 19)] {
                let current = if value == nice { "  ✓" } else { "" };
                if ui.button(format!("{label} ({value:+}){current}")).clicked() {
                    actions.push(AppAction::SetPriority(pid, name.clone(), value));
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button("End process").clicked() {
            actions.push(AppAction::ConfirmTerminate(pid, name.clone()));
            ui.close();
        }
        if ui.button("Force kill").clicked() {
            actions.push(AppAction::ConfirmKill(pid, name.clone()));
            ui.close();
        }
    });
    if !response.context_menu_opened() {
        response.ctx.data_mut(|data| data.remove::<MenuTarget>(latch_id));
    }
}

/// The top-3 list under a card, plus its "all by …" link.
fn top_list<F>(ui: &mut Ui, cx: &mut CardContext, column: SortColumn, what: &str, key: F, value: impl Fn(&CardContext, &ProcessRecord) -> String)
where
    F: Fn(&ProcessRecord) -> f64,
{
    let top: Vec<ProcessRecord> = cx.snapshot.top_processes_by(3, 0.0, key).into_iter().cloned().collect();
    for (index, record) in top.iter().enumerate() {
        let text = value(cx, record);
        top_process_row(ui, cx, index + 1, record, text);
    }
    if !top.is_empty() {
        more_link(ui, cx, column, what);
    }
}

// ------------------------------------------------------------------ cards

fn gpu_card(ui: &mut Ui, cx: &mut CardContext, history: &History) {
    let Some(gpu) = cx.snapshot.gpu.clone() else {
        card_header(ui, cx, "gpu", "GPU", "", "—");
        return;
    };
    if !gpu.available {
        card_header(ui, cx, "gpu", "GPU", "", "");
        ui.label(
            RichText::new("No AMD GPU on this machine — this card stays quiet. (NVIDIA/Intel support is on the ledger.)")
                .color(cx.palette.muted)
                .italics()
                .size(11.0),
        );
        return;
    }

    let mut headline = format_percent(gpu.busy_percent);
    if let Some(edge) = gpu.temperature_edge_celsius {
        headline += &format!(" · {}", cx.temperature(edge));
    }
    card_header(ui, cx, "gpu", "GPU", &gpu.device_name, &headline);

    let color = graph_color(cx.graph_palette_id, GRAPH_SERIES_GPU, cx.palette.dark);
    graphs::history_graph(
        ui,
        cx.palette,
        &[history],
        &GraphConfig {
            style: GraphStyle::Line,
            height: graph_height(cx),
            fixed_maximum: Some(100.0),
            minimum_autoscale: 1.0,
            colors: &[color],
            hover_formatter: &|values| format!("{:.0}% busy", values[0]),
        },
    );

    if gpu.vram_total_bytes > 0 {
        let fraction = gpu.vram_used_bytes as f32 / gpu.vram_total_bytes as f32;
        labelled_bar(
            ui,
            cx.palette,
            "VRAM",
            format!("{} / {}", cx.size(gpu.vram_used_bytes), cx.size(gpu.vram_total_bytes)),
            fraction,
            color,
        );
    }

    if cx.compact {
        return;
    }
    let power = match (gpu.power_draw_watts, gpu.power_cap_watts) {
        (Some(draw), Some(cap)) => format!("{} / {}", format_power(draw), format_power(cap)),
        (Some(draw), None) => format_power(draw),
        _ => "—".to_string(),
    };
    let gtt = if gpu.gtt_total_bytes > 0 {
        format!("{} / {}", cx.size(gpu.gtt_used_bytes), cx.size(gpu.gtt_total_bytes))
    } else {
        "—".to_string()
    };
    // "—" when the fan is unreadable: "0 rpm" would claim a stopped fan.
    let fan = match (gpu.fan_rpm, gpu.fan_max_rpm) {
        (Some(rpm), _) => format!("{rpm} rpm"),
        (None, _) => "—".to_string(),
    };
    stat_grid(
        ui,
        cx.palette,
        &[
            stat("Core", gpu.core_clock_mhz.map(format_frequency_mhz).unwrap_or("—".into())),
            stat("VRAM Clk", gpu.memory_clock_mhz.map(format_frequency_mhz).unwrap_or("—".into())),
            stat("Power", power),
            stat("Hot Spot", gpu.temperature_junction_celsius.map(|c| cx.temperature(c)).unwrap_or("—".into())),
            stat("Fan", fan),
            ("GTT", gtt, Some("system RAM the GPU has mapped (spill-over from VRAM)".into())),
        ],
    );

    if let Some(records) = &cx.snapshot.processes {
        let rows: Vec<(ProcessRecord, String)> = gpu
            .processes
            .iter()
            .filter_map(|usage| {
                let record = records.iter().find(|r| r.pid == usage.pid)?;
                Some((record.clone(), format!("{:.0}% · {}", usage.busy_percent, cx.size(usage.vram_bytes))))
            })
            .take(3)
            .collect();
        for (index, (record, value)) in rows.iter().enumerate() {
            top_process_row(ui, cx, index + 1, record, value.clone());
        }
        if !rows.is_empty() {
            more_link(ui, cx, SortColumn::Vram, "VRAM");
        }
    }
}

/// "VRAM ▕████░░░░▏ 1.2 GB / 34.2 GB" — label, a level bar, a value.
fn labelled_bar(ui: &mut Ui, palette: &Palette, label: &str, value: String, fraction: f32, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(palette.muted).size(11.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).monospace().size(11.5));
            graphs::level_bar(ui, palette, fraction, color);
        });
    });
}

fn memory_card(ui: &mut Ui, cx: &mut CardContext, history: &History) {
    let Some(memory) = cx.snapshot.memory.clone() else {
        card_header(ui, cx, "memory", "Memory", "", "—");
        return;
    };
    // The headline compares used to *usable* RAM (MemTotal): what the
    // kernel can hand out. The installed size is said separately,
    // right below, so neither number pretends to be the other.
    let headline = format!(
        "{} · {} / {}",
        format_percent(memory.used_percent),
        cx.size(memory.used_bytes),
        cx.size(memory.total_bytes)
    );
    let subtitle = memory_subtitle(&memory, cx.display.units);
    card_header(ui, cx, "memory", "Memory", &subtitle, &headline);

    let color = graph_color(cx.graph_palette_id, GRAPH_SERIES_MEMORY, cx.palette.dark);
    graphs::history_graph(
        ui,
        cx.palette,
        &[history],
        &GraphConfig {
            style: GraphStyle::Bars,
            height: graph_height(cx),
            fixed_maximum: Some(100.0),
            minimum_autoscale: 1.0,
            colors: &[color],
            hover_formatter: &|values| format!("{:.0}% used", values[0]),
        },
    );

    // Where the RAM is: in use · cache (reclaimable on demand) · free.
    let total = memory.total_bytes.max(1) as f32;
    let cache = (memory.cached_bytes + memory.buffers_bytes).min(memory.total_bytes.saturating_sub(memory.used_bytes));
    graphs::share_bar(
        ui,
        cx.palette,
        &[
            (color, memory.used_bytes as f64),
            (color.gamma_multiply(0.35), cache as f64),
            (Color32::TRANSPARENT, (memory.total_bytes.saturating_sub(memory.used_bytes + cache)) as f64),
        ],
    )
    .on_hover_text(format!(
        "in use {:.0}% · cache {:.0}% (the kernel hands it back on demand) · free {:.0}%",
        memory.used_bytes as f32 / total * 100.0,
        cache as f32 / total * 100.0,
        (memory.total_bytes.saturating_sub(memory.used_bytes + cache)) as f32 / total * 100.0,
    ));

    if cx.compact {
        return;
    }
    let swap = if memory.swap_total_bytes > 0 {
        format!("{} / {}", cx.size(memory.swap_used_bytes), cx.size(memory.swap_total_bytes))
    } else {
        "none".to_string()
    };
    let mut stats = vec![
        ("Used", cx.size(memory.used_bytes), Some("usable − available: what programs hold and can't give back".into())),
        ("Available", cx.size(memory.available_bytes), Some("free + the cache the kernel can reclaim right now".into())),
        ("Cached", cx.size(memory.cached_bytes), Some("page cache + reclaimable slab: speeds up files, given back on demand".into())),
        stat("Swap", swap),
    ];
    if let Some(installed) = memory.installed_bytes {
        let reserved = installed.saturating_sub(memory.total_bytes);
        stats.push((
            "Installed",
            cx.size(installed),
            Some(installed_note(&memory, installed, cx.display.units)),
        ));
        stats.push((
            "Reserved",
            cx.size(reserved),
            Some("installed − usable: kept by the firmware, the GPU's shared aperture and the kernel's own code".into()),
        ));
    } else {
        stats.push(stat("Buffers", cx.size(memory.buffers_bytes)));
        stats.push(stat("Dirty", cx.size(memory.dirty_bytes)));
    }
    stat_grid(ui, cx.palette, &stats);

    top_list(ui, cx, SortColumn::Memory, "memory", |p| p.memory_rss_bytes as f64, |cx, record| {
        cx.size(record.memory_rss_bytes)
    });
}

/// "4 × 32 GiB DDR4 · 2133 MT/s" — the sticks, as the box says.
fn memory_subtitle(memory: &sysmon_core::snapshot::MemorySnapshot, units_: units::Units) -> String {
    let Some(first) = memory.modules.first() else {
        return String::new();
    };
    let same_size = memory.modules.iter().all(|m| m.size_bytes == first.size_bytes);
    let sticks = if same_size {
        // Module sizes are powers of two; their marketing unit is GiB
        // whatever the display units ("32 GB" sticks are 32 GiB).
        format!("{} × {} GB", memory.modules.len(), first.size_bytes >> 30)
    } else {
        format!("{} sticks · {}", memory.modules.len(), format_size(memory.installed_bytes.unwrap_or(0), units_))
    };
    match first.configured_speed_mts {
        Some(speed) => format!("{sticks} {} · {speed} MT/s", first.kind),
        None => format!("{sticks} {}", first.kind),
    }
}

/// The hover note that untangles the three "total"s and the speed.
fn installed_note(memory: &sysmon_core::snapshot::MemorySnapshot, installed: u64, units_: units::Units) -> String {
    let mut note = format!(
        "{} of sticks ({} as sold — memory is sold in binary units: \"128 GB\" is 128 GiB). \
         Usable {} after firmware and kernel reservations.",
        format_size(installed, units_),
        format_size(installed, units::Units::Binary).replace("GiB", "GB"),
        format_size(memory.total_bytes, units_),
    );
    if let Some(module) = memory.modules.first()
        && let Some(configured) = module.configured_speed_mts
    {
        note += &format!("\nRunning at {configured} MT/s");
        if let Some(part) = &module.part_number {
            note += &format!(" ({part})");
        }
        note += ". A rated XMP/EXPO profile is enabled in the BIOS, not by the OS.";
    }
    note
}

fn cpu_card(ui: &mut Ui, cx: &mut CardContext, history: &History) {
    let Some(cpu) = cx.snapshot.cpu.clone() else {
        card_header(ui, cx, "cpu", "CPU", "", "—");
        return;
    };
    // Headline clock: what the working cores run at (busy-weighted);
    // the plain mean over 32 threads mostly measures parked ones.
    let mut headline = format_percent(cpu.overall_percent);
    if let Some(frequency) = cpu.frequency_busy_mhz.or(cpu.frequency_mhz) {
        headline += &format!(" · {}", format_frequency_mhz(frequency));
    }
    let hot = cpu.temperature_celsius.is_some_and(|t| t >= CPU_HOT_CELSIUS);
    if let Some(temperature) = cpu.temperature_celsius {
        headline += &format!(" · {}", cx.temperature(temperature));
    }
    let subtitle = format!("{} threads", cpu.core_count);
    card_header(ui, cx, "cpu", "CPU", &subtitle, &headline);
    if hot {
        hot_note(ui, cx, cpu.temperature_celsius.unwrap_or(0.0), cpu.overall_percent);
    }

    let color = graph_color(cx.graph_palette_id, GRAPH_SERIES_CPU, cx.palette.dark);
    graphs::history_graph(
        ui,
        cx.palette,
        &[history],
        &GraphConfig {
            style: GraphStyle::Area,
            height: graph_height(cx),
            fixed_maximum: Some(100.0),
            minimum_autoscale: 1.0,
            colors: &[color],
            hover_formatter: &|values| format!("{:.0}% overall", values[0]),
        },
    );

    if cx.compact {
        return;
    }
    graphs::per_core_bars(ui, cx.palette, &cpu.per_core_percent, color);
    let processes = cx.snapshot.processes.as_ref().map(|p| p.len());
    stat_grid(
        ui,
        cx.palette,
        &[
            (
                "Load",
                format!("{:.2} · {:.2} · {:.2}", cpu.load_1m, cpu.load_5m, cpu.load_15m),
                Some(format!("runnable + waiting tasks, 1 · 5 · 15 min — {} threads is full", cpu.core_count)),
            ),
            (
                "IO wait",
                format!("{:.0}%", cpu.iowait_percent),
                Some("idle time spent waiting on storage — not counted as busy".into()),
            ),
            (
                "Process count",
                processes.map(|n| n.to_string()).unwrap_or("—".into()),
                Some(format!("{} kernel tasks (threads) in total", cpu.tasks_total)),
            ),
            stat("Ctx/s", format!("{:.0}", cpu.context_switches_per_second)),
            (
                "Avg clock",
                cpu.frequency_mhz.map(format_frequency_mhz).unwrap_or("—".into()),
                Some("mean of all threads' requested clocks, parked ones included".into()),
            ),
            (
                "Range",
                match (cpu.frequency_min_mhz, cpu.frequency_max_mhz) {
                    (Some(min), Some(max)) => format!("{}–{}", format_frequency_mhz(min), format_frequency_mhz(max)),
                    _ => "—".to_string(),
                },
                Some("the clock limits the driver will request (amd-pstate)".into()),
            ),
        ],
    );
    top_list(ui, cx, SortColumn::Cpu, "CPU", |p| p.cpu_percent as f64, |_, record| {
        format!("{:.1}%", record.cpu_percent)
    });
}

/// A calm, clear line when the CPU runs hot — light load at a high
/// temperature usually means cooling (seating, paste, pump header),
/// which software can't fix but should say.
fn hot_note(ui: &mut Ui, cx: &CardContext, celsius: f32, busy: f32) {
    ui.horizontal(|ui| {
        glyphs::show(ui, Glyph::Thermometer, 12.0, cx.palette.accent);
        let text = if busy < 30.0 {
            format!("Running warm for {busy:.0}% load — worth checking the cooler")
        } else {
            "Running near its limit".to_string()
        };
        ui.label(RichText::new(text).color(cx.palette.accent).size(10.5)).on_hover_text(format!(
            "{} on the die. Zen 3 throttles at 90 °C (194 °F). At light load this usually \
             points at cooling: cooler seating and paste, or a pump on a fan header that \
             isn't spinning (see Sensors → Motherboard).",
            format_temperature_in(celsius, sysmon_core::units::TemperatureScale::Both)
        ));
    });
}

fn network_card(ui: &mut Ui, cx: &mut CardContext, down: &History, up: &History) {
    let Some(network) = cx.snapshot.network.clone() else {
        card_header(ui, cx, "network", "Network", "", "—");
        return;
    };
    let headline = format!("↓ {}   ↑ {}", cx.rate(network.download_bps), cx.rate(network.upload_bps));
    card_header(ui, cx, "network", "Network", "", &headline);

    let down_color = graph_color(cx.graph_palette_id, GRAPH_SERIES_NET_DOWN, cx.palette.dark);
    let up_color = graph_color(cx.graph_palette_id, GRAPH_SERIES_NET_UP, cx.palette.dark);
    let units_for_hover = cx.display.units;
    graphs::history_graph(
        ui,
        cx.palette,
        &[down, up],
        &GraphConfig {
            style: GraphStyle::Line,
            height: graph_height(cx),
            fixed_maximum: None,
            minimum_autoscale: 20.0 * 1024.0,
            colors: &[down_color, up_color],
            hover_formatter: &move |values| {
                format!(
                    "↓ {}   ↑ {}",
                    format_rate(values[0], units_for_hover),
                    format_rate(values.get(1).copied().unwrap_or(0.0), units_for_hover)
                )
            },
        },
    );

    if cx.compact {
        return;
    }
    stat_grid(
        ui,
        cx.palette,
        &[
            stat("Total ↓", cx.size(network.total_received_bytes)),
            stat("Total ↑", cx.size(network.total_sent_bytes)),
        ],
    );

    // Interfaces worth showing: physical always; virtual only while
    // it's up and moving.
    for interface in &network.interfaces {
        use sysmon_core::snapshot::InterfaceKind;
        let interesting = match interface.kind {
            InterfaceKind::Physical => true,
            InterfaceKind::Loopback => false,
            InterfaceKind::Virtual => interface.is_up && (interface.rx_bps + interface.tx_bps) > 1024.0,
        };
        if !interesting {
            continue;
        }
        ui.horizontal(|ui| {
            let dot_color = if interface.is_up { down_color } else { cx.palette.muted };
            let (dot_rect, _) = ui.allocate_exact_size(vec2(6.0, 6.0), Sense::hover());
            ui.painter().rect_filled(dot_rect, 0.0, dot_color);
            ui.label(RichText::new(&interface.name).monospace().size(11.5));
            if let Some(address) = interface.ipv4.first() {
                ui.label(RichText::new(address).color(cx.palette.muted).size(11.0));
            }
            if let Some(speed) = interface.speed_mbps {
                ui.label(RichText::new(format!("{speed} Mb/s")).color(cx.palette.muted).size(10.5));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("↓ {}  ↑ {}", cx.rate(interface.rx_bps), cx.rate(interface.tx_bps)))
                        .monospace()
                        .size(11.0),
                );
            });
        });
    }

    // Top processes (split ↓/↑).
    let mut shown = 0;
    if let Some(records) = &cx.snapshot.processes {
        let rows: Vec<(ProcessRecord, String)> = network
            .top_processes
            .iter()
            .filter_map(|entry| {
                let record = records.iter().find(|r| r.pid == entry.pid)?;
                Some((record.clone(), format!("↓ {} ↑ {}", cx.rate(entry.rx_bps), cx.rate(entry.tx_bps))))
            })
            .take(3)
            .collect();
        for (index, (record, value)) in rows.iter().enumerate() {
            top_process_row(ui, cx, index + 1, record, value.clone());
        }
        shown = rows.len();
        if shown > 0 {
            more_link(ui, cx, SortColumn::NetDown, "download");
        }
    }
    // The per-process source, said plainly — the two layers measure
    // different things and must never be mistaken for each other.
    let note = match network.process_source {
        ProcessNetSource::Nethogs => None,
        ProcessNetSource::TcpDiag => Some(
            "per-process: your own TCP sockets only (nethogs adds UDP/QUIC and other users)".to_string(),
        ),
        ProcessNetSource::None => network.process_source_hint.clone(),
    };
    if let Some(note) = note {
        let response = ui.label(RichText::new(note).color(cx.palette.muted.gamma_multiply(0.8)).size(9.5));
        if let Some(hint) = &network.process_source_hint {
            response.on_hover_text(hint);
        }
    }
    let _ = shown;
}

fn disks_card(ui: &mut Ui, cx: &mut CardContext) {
    let Some(disks) = cx.snapshot.disks.clone() else {
        card_header(ui, cx, "disks", "Disks", "", "—");
        return;
    };
    let total_read: f64 = disks.iter().map(|d| d.read_bps).sum();
    let total_write: f64 = disks.iter().map(|d| d.write_bps).sum();
    let headline = format!("R {}   W {}", cx.rate(total_read), cx.rate(total_write));
    card_header(ui, cx, "disks", "Disks", "", &headline);

    for disk in &disks {
        ui.horizontal(|ui| {
            glyphs::show(ui, Glyph::Drive, 12.0, cx.palette.muted);
            ui.add(egui::Label::new(RichText::new(&disk.display_name).size(12.0)).truncate())
                .on_hover_text(format!("{} · {}", disk.device, disk.fs_type));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("R {}  W {}", cx.rate(disk.read_bps), cx.rate(disk.write_bps)))
                        .monospace()
                        .size(11.0),
                );
                if disk.util_percent > 0.5 {
                    ui.label(
                        RichText::new(format!("{:.0}%", disk.util_percent))
                            .color(cx.palette.muted)
                            .monospace()
                            .size(10.5),
                    )
                    .on_hover_text("share of the window the disk was busy");
                }
            });
        });
        if !cx.compact {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} · {} of {}",
                        disk.mount_point,
                        cx.size(disk.used_bytes),
                        cx.size(disk.total_bytes)
                    ))
                    .color(cx.palette.muted)
                    .size(10.5),
                );
                let fraction = if disk.total_bytes > 0 {
                    disk.used_bytes as f32 / disk.total_bytes as f32
                } else {
                    0.0
                };
                // A nearly full filesystem earns the accent.
                let color = if fraction > 0.9 { cx.palette.accent } else { cx.palette.ink_2 };
                graphs::level_bar(ui, cx.palette, fraction, color);
            });
            ui.add_space(2.0);
        }
    }
    if disks.is_empty() {
        ui.label(
            RichText::new("No mounted real drives found — that would be a first.")
                .color(cx.palette.muted)
                .italics()
                .size(11.0),
        );
    }
}

fn sensors_card(ui: &mut Ui, cx: &mut CardContext) {
    let Some(sensors) = cx.snapshot.sensors.clone() else {
        card_header(ui, cx, "sensors", "Sensors", "", "—");
        return;
    };
    let groups = sensors_view::group_sensors(&sensors);
    // Headline: the hottest plausible reading, and where it is.
    let hottest = groups
        .iter()
        .filter_map(|group| group.hottest().map(|(c, name)| (c, format!("{} · {name}", group.title))))
        .max_by(|a, b| a.0.total_cmp(&b.0));
    let headline = hottest.as_ref().map(|(c, _)| cx.temperature(*c)).unwrap_or("—".into());
    let subtitle = hottest.map(|(_, at)| format!("hottest: {at}")).unwrap_or_default();
    card_header(ui, cx, "sensors", "Sensors", &subtitle, &headline);
    if cx.compact {
        return;
    }

    let series = graph_color(cx.graph_palette_id, GRAPH_SERIES_CPU, cx.palette.dark);
    let folded = cx.folded_sensor_groups;
    let mut toggled: Option<String> = None;
    sensors_view::sensors_body(
        ui,
        cx.palette,
        cx.display.temperature,
        series,
        &groups,
        &|key| folded.iter().any(|g| g == key),
        &mut |key| toggled = Some(key.to_string()),
    );
    if let Some(key) = toggled {
        cx.actions.push(AppAction::ToggleSensorGroup(key));
    }

    // Drives report temperatures only through the `drivetemp` (SATA)
    // or `nvme` hwmon drivers — when neither is loaded but disks
    // exist, say what would unlock them (never hide a degraded mode).
    let has_drive_chip = sensors.chips.iter().any(|chip| chip.name == "drivetemp" || chip.name == "nvme");
    if !has_drive_chip && cx.snapshot.disks.as_ref().is_some_and(|disks| !disks.is_empty()) {
        ui.label(
            RichText::new("Drive temperatures appear once the kernel's drivetemp module is loaded (modprobe drivetemp).")
                .color(cx.palette.muted)
                .italics()
                .size(11.0),
        );
    }
    if groups.is_empty() {
        ui.horizontal(|ui| {
            glyphs::show(ui, Glyph::Sparkle, 11.0, cx.palette.muted);
            ui.label(
                RichText::new("No extra sensors beyond what the other cards already show.")
                    .color(cx.palette.muted)
                    .italics()
                    .size(11.0),
            );
        });
    }
}
