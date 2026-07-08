// SPDX-License-Identifier: GPL-3.0-or-later
//! The Overview cards: GPU, Memory, CPU, Network, Disks, Sensors —
//! v1's information priority in the house chrome. Each card is one
//! function over the latest snapshot, reused verbatim by the main
//! page and (wave 7) the pop-out viewports. Cards emit AppActions
//! (kill this pid, open details, toggle pop-out) that the app
//! applies after the frame — no borrow tangles.

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use sysmon_core::snapshot::{ProcessNetSource, ProcessRecord, SystemSnapshot};
use sysmon_core::units::{
    self, Units, format_frequency_mhz, format_percent, format_power, format_rate, format_size,
    format_temperature,
};

use super::graphs::{self, GraphConfig, GraphStyle, History};
use super::icons::{self, IconCache};
use super::theme::{
    GRAPH_SERIES_CPU, GRAPH_SERIES_GPU, GRAPH_SERIES_MEMORY, GRAPH_SERIES_NET_DOWN,
    GRAPH_SERIES_NET_UP, Palette, card_frame, graph_color,
};

pub const FULL_GRAPH_HEIGHT: f32 = 66.0;
pub const COMPACT_GRAPH_HEIGHT: f32 = 36.0;

/// Everything a card needs for one frame.
pub struct CardContext<'a> {
    pub palette: &'a Palette,
    pub graph_palette_id: &'a str,
    pub units: Units,
    pub compact: bool,
    pub snapshot: &'a SystemSnapshot,
    pub icon_cache: &'a mut IconCache,
    pub actions: &'a mut Vec<AppAction>,
    pub popped_out: &'a [String],
    /// The app-wide multi-selection (shared with the process table) —
    /// Ctrl+click on any top-process row joins it.
    pub selected: &'a mut Vec<i32>,
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
    ConfirmTerminate(i32, String),
    ConfirmKill(i32, String),
    SetPriority(i32, String, i32),
    CopyPid(i32),
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

// ---------------------------------------------------------------- glyphs

/// Hand-painted glyph buttons — no font-coverage gambling.
#[derive(Clone, Copy, PartialEq)]
pub enum Glyph {
    Pause,
    Play,
    Pin,
    Menu,
    PopOut,
}

pub fn glyph_button(
    ui: &mut Ui,
    palette: &Palette,
    glyph: Glyph,
    active: bool,
    tooltip: &str,
) -> egui::Response {
    let size = vec2(22.0, 18.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    // Screen readers (and the kittest harness) see the tooltip text.
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tooltip)
    });
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let pressed = response.is_pointer_button_down_on();
        // The glyph NEVER chases the accent (phosphor's bevel_toggle
        // rule): the accent lives in the face tint and the border,
        // the glyph stays ink. The v2.2.0 port recolored the active
        // glyph toward the accent while the face also eased toward
        // it — the two converged and the pin washed out in every
        // room (Ben hid the button over it).
        let stroke_color = if active || hovered {
            palette.ink
        } else {
            palette.ink_2
        };
        // Carved stone, phosphor's bevel_toggle feel (ported at Ben's
        // ask, mixes matched to its numbers): the face EASES toward
        // the accent on hover/active — short, purposeful animation —
        // and the glyph nudges 1px when pressed. Dimension encodes
        // importance; the shape never changes, the surface does.
        // Pressed or active = sunk in.
        let active_mix = ui.ctx().animate_bool(response.id, active) * 0.22;
        let hover_mix =
            ui.ctx().animate_bool(response.id.with("hover"), hovered) * 0.10;
        let face = palette
            .stone
            .lerp_to_gamma(palette.accent, (active_mix + hover_mix).min(0.32));
        let painter = ui.painter();
        let sunk = active || pressed;
        painter.rect_filled(rect, 0.0, face);
        let (top_left, bottom_right) = if sunk {
            (palette.stone_lo, palette.stone_hi) // inset
        } else {
            (palette.stone_hi, palette.stone_lo) // raised
        };
        let edge = |a: egui::Pos2, b: egui::Pos2, color: Color32| {
            painter.line_segment([a, b], Stroke::new(1.0, color));
        };
        edge(rect.left_top(), rect.right_top(), top_left);
        edge(rect.left_top(), rect.left_bottom(), top_left);
        edge(rect.left_bottom(), rect.right_bottom(), bottom_right);
        edge(rect.right_top(), rect.right_bottom(), bottom_right);
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, if active { palette.accent } else { palette.line }),
            StrokeKind::Inside,
        );
        let stroke = Stroke::new(1.4, stroke_color);
        let nudge = if pressed { vec2(1.0, 1.0) } else { vec2(0.0, 0.0) };
        let center = rect.center() + nudge;
        match glyph {
            Glyph::Pause => {
                for offset in [-2.5f32, 2.5] {
                    painter.line_segment(
                        [
                            pos2(center.x + offset, center.y - 4.0),
                            pos2(center.x + offset, center.y + 4.0),
                        ],
                        stroke,
                    );
                }
            }
            Glyph::Play => {
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        pos2(center.x - 3.0, center.y - 4.5),
                        pos2(center.x + 4.5, center.y),
                        pos2(center.x - 3.0, center.y + 4.5),
                    ],
                    stroke_color,
                    Stroke::NONE,
                ));
            }
            Glyph::Pin => {
                // Phosphor's push-pin, hand-painted (its icon font
                // would tofu here): diagonal thumbtack — round head
                // upper-right, shoulder plate, needle to lower-left.
                painter.line_segment(
                    [
                        pos2(center.x - 4.6, center.y + 4.6),
                        pos2(center.x - 1.4, center.y + 1.4),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        pos2(center.x - 3.4, center.y - 0.6),
                        pos2(center.x + 0.6, center.y + 3.4),
                    ],
                    stroke,
                );
                painter.circle_filled(
                    pos2(center.x + 2.1, center.y - 2.1),
                    2.4,
                    stroke_color,
                );
            }
            Glyph::Menu => {
                for offset in [-3.5f32, 0.0, 3.5] {
                    painter.line_segment(
                        [
                            pos2(center.x - 4.5, center.y + offset),
                            pos2(center.x + 4.5, center.y + offset),
                        ],
                        stroke,
                    );
                }
            }
            Glyph::PopOut => {
                let back = Rect::from_center_size(pos2(center.x - 1.5, center.y + 1.5), vec2(7.0, 7.0));
                let front = Rect::from_center_size(pos2(center.x + 2.0, center.y - 2.0), vec2(7.0, 7.0));
                painter.rect_stroke(back, 0.0, Stroke::new(1.2, stroke_color.gamma_multiply(0.6)), StrokeKind::Inside);
                painter.rect_stroke(front, 0.0, Stroke::new(1.2, stroke_color), StrokeKind::Inside);
            }
        }
    }
    response.on_hover_text(tooltip)
}

// ------------------------------------------------------------- menu rows

#[derive(Clone, Copy, PartialEq)]
enum MenuMark {
    Radio,
    Check,
}

/// One hover-lit menu item: full-width (or chip-sized) hit target, an
/// eased ink glow under the pointer (the effect Ben asked for — bare
/// egui radios paint nothing on hover), a sharp engraved mark, and an
/// accent spine on the selected row.
fn menu_item(
    ui: &mut Ui,
    palette: &Palette,
    selected: bool,
    label: &str,
    mark: MenuMark,
    full_width: bool,
) -> egui::Response {
    let text_color = if selected { palette.ink } else { palette.ink_2 };
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(12.5),
        text_color,
    );
    let mark_span = 17.0;
    let intrinsic = galley.size().x + mark_span + 10.0;
    let width = if full_width {
        ui.available_width().max(intrinsic)
    } else {
        intrinsic
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, 19.0), Sense::click());
    let label_owned = label.to_string();
    response.widget_info(move || {
        egui::WidgetInfo::selected(
            match mark {
                MenuMark::Radio => egui::WidgetType::RadioButton,
                MenuMark::Check => egui::WidgetType::Checkbox,
            },
            true,
            selected,
            label_owned.clone(),
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let hover_t = ui
            .ctx()
            .animate_bool(response.id.with("hover"), response.hovered());
        if hover_t > 0.0 {
            painter.rect_filled(rect, 0.0, palette.ink.gamma_multiply(0.08 * hover_t));
        }
        if selected && full_width {
            painter.rect_filled(
                Rect::from_min_max(rect.min, pos2(rect.min.x + 2.0, rect.max.y)),
                0.0,
                palette.accent,
            );
        }
        let mark_rect =
            Rect::from_center_size(pos2(rect.min.x + 10.0, rect.center().y), vec2(8.0, 8.0));
        let frame_color = if selected {
            palette.accent
        } else {
            palette.line_strong
        };
        painter.rect_stroke(mark_rect, 0.0, Stroke::new(1.0, frame_color), StrokeKind::Inside);
        if selected {
            match mark {
                MenuMark::Radio => {
                    painter.rect_filled(mark_rect.shrink(2.5), 0.0, palette.accent);
                }
                MenuMark::Check => {
                    let check = Stroke::new(1.4, palette.accent);
                    let low = pos2(mark_rect.center().x - 0.8, mark_rect.max.y - 2.2);
                    painter.line_segment(
                        [pos2(mark_rect.min.x + 1.6, mark_rect.center().y + 0.4), low],
                        check,
                    );
                    painter.line_segment(
                        [low, pos2(mark_rect.max.x - 1.4, mark_rect.min.y + 1.6)],
                        check,
                    );
                }
            }
        }
        let text_pos = pos2(
            rect.min.x + mark_span + 3.0,
            rect.center().y - galley.size().y / 2.0,
        );
        painter.galley(text_pos, galley, text_color);
    }
    response
}

/// Full-width single-choice row (theme list and friends).
pub fn menu_option_row(ui: &mut Ui, palette: &Palette, selected: bool, label: &str) -> egui::Response {
    menu_item(ui, palette, selected, label, MenuMark::Radio, true)
}

/// Chip-sized single-choice item for horizontal groups (units,
/// intervals, graph palettes).
pub fn menu_chip(ui: &mut Ui, palette: &Palette, selected: bool, label: &str) -> egui::Response {
    menu_item(ui, palette, selected, label, MenuMark::Radio, false)
}

/// Full-width toggle row; returns the response — callers flip on click.
pub fn menu_check_row(ui: &mut Ui, palette: &Palette, checked: bool, label: &str) -> egui::Response {
    menu_item(ui, palette, checked, label, MenuMark::Check, true)
}

// ------------------------------------------------------------ card chrome

/// Header row: title · subtitle · headline (right) · pop-out toggle.
/// Returns true when the body should render (always, today; hook for
/// future collapse).
fn card_header(
    ui: &mut Ui,
    cx: &mut CardContext,
    section_key: &'static str,
    title: &str,
    subtitle: &str,
    headline: &str,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .color(cx.palette.title)
                .strong()
                .size(13.0),
        );
        if !subtitle.is_empty() && !cx.compact {
            // Give the subtitle only what the right-side headline +
            // pop-out button won't need, so it truncates instead of
            // running underneath them.
            let headline_width = ui.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        headline.to_string(),
                        egui::FontId::monospace(12.5),
                        cx.palette.value,
                    )
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
                        egui::Label::new(
                            RichText::new(subtitle).color(cx.palette.muted).size(11.0),
                        )
                        .truncate(),
                    );
                },
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let popped = cx.popped_out.contains(&section_key.to_string());
            if glyph_button(
                ui,
                cx.palette,
                Glyph::PopOut,
                popped,
                if popped {
                    "Return to the main window"
                } else {
                    "Pop out into its own window"
                },
            )
            .clicked()
            {
                cx.actions.push(AppAction::TogglePopOut(section_key));
            }
            ui.label(
                RichText::new(headline)
                    .color(cx.palette.value)
                    .monospace()
                    .size(12.5),
            );
        });
    });
}

/// Two-column grid of dim label / mono value pairs (v1's StatGrid).
fn stat_grid(ui: &mut Ui, palette: &Palette, stats: &[(&str, String)]) {
    let column_count = 2;
    egui::Grid::new(ui.next_auto_id())
        .num_columns(column_count * 2)
        .spacing(vec2(10.0, 2.0))
        .show(ui, |ui| {
            for (index, (label, value)) in stats.iter().enumerate() {
                ui.label(
                    RichText::new(*label)
                        .color(palette.muted)
                        .monospace()
                        .size(10.5),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(value).monospace().size(11.5));
                });
                if index % column_count == column_count - 1 {
                    ui.end_row();
                }
            }
        });
}

/// One top-process row: rank · icon · name … value. Right-click for
/// actions, click for details.
#[allow(clippy::too_many_arguments)]
fn top_process_row(
    ui: &mut Ui,
    cx: &mut CardContext,
    rank: usize,
    record: &ProcessRecord,
    value_text: String,
) {
    let row_height = 18.0;
    let full_width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(vec2(full_width, row_height), Sense::click());
    // Assistive tech (and kittest) sees each row as a named button.
    let a11y_label = format!("{} — PID {}", record.name, record.pid);
    response.widget_info(move || {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y_label.clone())
    });
    if !ui.is_rect_visible(rect) {
        return;
    }
    // Multi-selected rows wear their slot color (the same color the
    // combined-details window uses for this process).
    let selection_slot = cx.selected.iter().position(|p| *p == record.pid);
    if let Some(slot) = selection_slot {
        let color = selection_color(cx.graph_palette_id, cx.palette.dark, slot);
        ui.painter().rect_filled(rect, 0.0, color.gamma_multiply(0.12));
        ui.painter().rect_filled(
            Rect::from_min_max(rect.min, pos2(rect.min.x + 3.0, rect.max.y)),
            0.0,
            color,
        );
    }
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, cx.palette.ink.gamma_multiply(0.05));
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
    match cx.icon_cache.texture_for(ui.ctx(), record) {
        Some(texture) => {
            painter.image(
                texture.id(),
                icon_rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            icons::draw_letter_tile(
                painter,
                icon_rect,
                &record.name,
                record.is_kernel_thread,
                cx.palette.muted,
            );
        }
    }
    x += 20.0;

    let value_width = value_text.len() as f32 * 7.0 + 6.0;
    painter.text(
        pos2(rect.right() - 2.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        &value_text,
        egui::FontId::monospace(11.5),
        cx.palette.ink.gamma_multiply(0.95),
    );

    let name_width = (rect.right() - value_width - x).max(20.0);
    let name = truncate_to_width(&record.name, name_width, 6.6);
    painter.text(
        pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(12.0),
        cx.palette.ink,
    );

    let response = response.on_hover_text(format!(
        "PID {} — click for details, Ctrl+click to multi-select",
        record.pid
    ));
    if response.clicked() {
        let modifiers = ui.input(|input| input.modifiers);
        if modifiers.command || modifiers.ctrl {
            toggle_selection(cx.selected, record.pid, cx.actions);
        } else {
            cx.actions.push(AppAction::OpenDetails(record.pid));
        }
    }
    process_context_menu(&response, cx.actions, record, cx.selected, false);
}

fn truncate_to_width(text: &str, width: f32, per_char: f32) -> String {
    let max_chars = (width / per_char).max(4.0) as usize;
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// The shared right-click menu (top-3 rows, the process table, and
/// the combined-details blocks). `in_process_table` hides the "Open
/// in process viewer" jump when the row already lives there.
pub fn process_context_menu(
    response: &egui::Response,
    actions: &mut Vec<AppAction>,
    record: &ProcessRecord,
    selected: &[i32],
    in_process_table: bool,
) {
    let pid = record.pid;
    let name = record.name.clone();
    let nice = record.nice;
    let combined: Option<Vec<i32>> = (selected.len() >= 2 && selected.contains(&pid))
        .then(|| selected.to_vec());
    response.context_menu(|ui| {
        ui.label(
            RichText::new(format!("{name}  (PID {pid})"))
                .monospace()
                .size(11.5),
        );
        if let Some(pids) = &combined {
            ui.separator();
            if ui
                .button(format!("Combined details ({} selected)…", pids.len()))
                .clicked()
            {
                actions.push(AppAction::OpenCombinedDetails(pids.clone()));
                ui.close();
            }
        }
        ui.separator();
        if ui.button("End process").clicked() {
            actions.push(AppAction::ConfirmTerminate(pid, name.clone()));
            ui.close();
        }
        if ui.button("Force kill").clicked() {
            actions.push(AppAction::ConfirmKill(pid, name.clone()));
            ui.close();
        }
        ui.separator();
        ui.menu_button("Set priority", |ui| {
            for (label, value) in [
                ("Very High", -12),
                ("High", -5),
                ("Normal", 0),
                ("Low", 10),
                ("Very Low", 19),
            ] {
                let current = if value == nice { "  ✓" } else { "" };
                if ui.button(format!("{label} ({value:+}){current}")).clicked() {
                    actions.push(AppAction::SetPriority(pid, name.clone(), value));
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button("Details…").clicked() {
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
    });
}

fn top_processes_by<F>(snapshot: &SystemSnapshot, minimum: f64, value: F) -> Vec<&ProcessRecord>
where
    F: Fn(&ProcessRecord) -> f64,
{
    snapshot.top_processes_by(3, minimum, value)
}

// ------------------------------------------------------------------ cards

pub fn gpu_card(ui: &mut Ui, cx: &mut CardContext, history: &History) {
    card_frame(cx.palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(gpu) = &cx.snapshot.gpu else {
            card_header(ui, cx, "gpu", "GPU", "", "—");
            return;
        };
        if !gpu.available {
            card_header(ui, cx, "gpu", "GPU", "", "");
            ui.label(
                RichText::new(
                    "No AMD GPU on this machine — this card stays quiet. \
                     (NVIDIA/Intel support is on the ledger.)",
                )
                .color(cx.palette.muted)
                .italics()
                .size(11.0),
            );
            return;
        }

        let mut headline = format_percent(gpu.busy_percent);
        if let Some(edge) = gpu.temperature_edge_celsius {
            headline += &format!(" · {}", format_temperature(edge));
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
            ui.horizontal(|ui| {
                ui.label(RichText::new("VRAM").color(cx.palette.muted).size(11.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} / {}",
                            format_size(gpu.vram_used_bytes, cx.units),
                            format_size(gpu.vram_total_bytes, cx.units)
                        ))
                        .monospace()
                        .size(11.5),
                    );
                    let fraction = gpu.vram_used_bytes as f32 / gpu.vram_total_bytes as f32;
                    graphs::level_bar(ui, cx.palette, fraction, color);
                });
            });
        }

        if !cx.compact {
            let power = match (gpu.power_draw_watts, gpu.power_cap_watts) {
                (Some(draw), Some(cap)) => format!("{} / {}", format_power(draw), format_power(cap)),
                (Some(draw), None) => format_power(draw),
                _ => "—".to_string(),
            };
            let gtt = if gpu.gtt_total_bytes > 0 {
                format!(
                    "{} / {}",
                    format_size(gpu.gtt_used_bytes, cx.units),
                    format_size(gpu.gtt_total_bytes, cx.units)
                )
            } else {
                "—".to_string()
            };
            stat_grid(
                ui,
                cx.palette,
                &[
                    ("Core", gpu.core_clock_mhz.map(format_frequency_mhz).unwrap_or("—".into())),
                    ("VRAM Clk", gpu.memory_clock_mhz.map(format_frequency_mhz).unwrap_or("—".into())),
                    ("Power", power),
                    ("Hot Spot", gpu.temperature_junction_celsius.map(format_temperature).unwrap_or("—".into())),
                    ("Fan", gpu.fan_rpm.map(|rpm| format!("{rpm} rpm")).unwrap_or("0 rpm".into())),
                    ("GTT", gtt),
                ],
            );

            let gpu_processes = &gpu.processes;
            if let Some(records) = &cx.snapshot.processes {
                let mut shown = 0;
                for usage in gpu_processes.iter() {
                    if shown >= 3 {
                        break;
                    }
                    if let Some(record) = records.iter().find(|r| r.pid == usage.pid) {
                        shown += 1;
                        let value = format!(
                            "{:.0}% · {}",
                            usage.busy_percent,
                            format_size(usage.vram_bytes, cx.units)
                        );
                        top_process_row(ui, cx, shown, record, value);
                    }
                }
            }
        }
    });
}

pub fn memory_card(ui: &mut Ui, cx: &mut CardContext, history: &History) {
    card_frame(cx.palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(memory) = &cx.snapshot.memory else {
            card_header(ui, cx, "memory", "Memory", "", "—");
            return;
        };
        let headline = format!(
            "{} · {} / {}",
            format_percent(memory.used_percent),
            format_size(memory.used_bytes, cx.units),
            format_size(memory.total_bytes, cx.units)
        );
        card_header(ui, cx, "memory", "Memory", "", &headline);

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

        if !cx.compact {
            let swap = if memory.swap_total_bytes > 0 {
                format!(
                    "{} / {}",
                    format_size(memory.swap_used_bytes, cx.units),
                    format_size(memory.swap_total_bytes, cx.units)
                )
            } else {
                "none".to_string()
            };
            stat_grid(
                ui,
                cx.palette,
                &[
                    ("Used", format_size(memory.used_bytes, cx.units)),
                    ("Available", format_size(memory.available_bytes, cx.units)),
                    ("Cached", format_size(memory.cached_bytes, cx.units)),
                    ("Swap", swap),
                    ("Buffers", format_size(memory.buffers_bytes, cx.units)),
                    ("Dirty", format_size(memory.dirty_bytes, cx.units)),
                ],
            );

            let top: Vec<(usize, ProcessRecord, String)> =
                top_processes_by(cx.snapshot, 0.0, |p| p.memory_rss_bytes as f64)
                    .into_iter()
                    .enumerate()
                    .map(|(index, record)| {
                        (
                            index + 1,
                            record.clone(),
                            format_size(record.memory_rss_bytes, cx.units),
                        )
                    })
                    .collect();
            for (rank, record, value) in top {
                top_process_row(ui, cx, rank, &record, value);
            }
        }
    });
}

pub fn cpu_card(ui: &mut Ui, cx: &mut CardContext, history: &History) {
    card_frame(cx.palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(cpu) = &cx.snapshot.cpu else {
            card_header(ui, cx, "cpu", "CPU", "", "—");
            return;
        };
        let mut headline = format_percent(cpu.overall_percent);
        if let Some(frequency) = cpu.frequency_mhz {
            headline += &format!(" · {}", format_frequency_mhz(frequency));
        }
        if let Some(temperature) = cpu.temperature_celsius {
            headline += &format!(" · {}", format_temperature(temperature));
        }
        let subtitle = format!("{} threads", cpu.core_count);
        card_header(ui, cx, "cpu", "CPU", &subtitle, &headline);

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

        if !cx.compact {
            graphs::per_core_bars(ui, cx.palette, &cpu.per_core_percent, color);
            stat_grid(
                ui,
                cx.palette,
                &[
                    (
                        "Load",
                        format!("{:.2} · {:.2} · {:.2}", cpu.load_1m, cpu.load_5m, cpu.load_15m),
                    ),
                    (
                        "Tasks",
                        format!(
                            "{}",
                            cx.snapshot
                                .processes
                                .as_ref()
                                .map(|p| p.len())
                                .unwrap_or(cpu.tasks_total as usize)
                        ),
                    ),
                    (
                        "Ctx/s",
                        format!("{:.0}", cpu.context_switches_per_second),
                    ),
                    (
                        "Range",
                        match (cpu.frequency_min_mhz, cpu.frequency_max_mhz) {
                            (Some(min), Some(max)) => format!(
                                "{}–{}",
                                format_frequency_mhz(min),
                                format_frequency_mhz(max)
                            ),
                            _ => "—".to_string(),
                        },
                    ),
                ],
            );

            let top: Vec<(usize, ProcessRecord, String)> =
                top_processes_by(cx.snapshot, 0.0, |p| p.cpu_percent as f64)
                    .into_iter()
                    .enumerate()
                    .map(|(index, record)| {
                        (index + 1, record.clone(), format!("{:.1}%", record.cpu_percent))
                    })
                    .collect();
            for (rank, record, value) in top {
                top_process_row(ui, cx, rank, &record, value);
            }
        }
    });
}

pub fn network_card(ui: &mut Ui, cx: &mut CardContext, down: &History, up: &History) {
    card_frame(cx.palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(network) = &cx.snapshot.network else {
            card_header(ui, cx, "network", "Network", "", "—");
            return;
        };
        let headline = format!(
            "↓ {}   ↑ {}",
            format_rate(network.download_bps, cx.units),
            format_rate(network.upload_bps, cx.units)
        );
        card_header(ui, cx, "network", "Network", "", &headline);

        let down_color = graph_color(cx.graph_palette_id, GRAPH_SERIES_NET_DOWN, cx.palette.dark);
        let up_color = graph_color(cx.graph_palette_id, GRAPH_SERIES_NET_UP, cx.palette.dark);
        let units_for_hover = cx.units;
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

        if !cx.compact {
            stat_grid(
                ui,
                cx.palette,
                &[
                    ("Total ↓", format_size(network.total_received_bytes, cx.units)),
                    ("Total ↑", format_size(network.total_sent_bytes, cx.units)),
                ],
            );

            // Interfaces worth showing: physical always; virtual only
            // while it's up and moving.
            for interface in &network.interfaces {
                use sysmon_core::snapshot::InterfaceKind;
                let interesting = match interface.kind {
                    InterfaceKind::Physical => true,
                    InterfaceKind::Loopback => false,
                    InterfaceKind::Virtual => {
                        interface.is_up && (interface.rx_bps + interface.tx_bps) > 1024.0
                    }
                };
                if !interesting {
                    continue;
                }
                ui.horizontal(|ui| {
                    let dot_color = if interface.is_up {
                        down_color
                    } else {
                        cx.palette.muted
                    };
                    let (dot_rect, _) =
                        ui.allocate_exact_size(vec2(6.0, 6.0), Sense::hover());
                    ui.painter().rect_filled(dot_rect, 0.0, dot_color);
                    ui.label(RichText::new(&interface.name).monospace().size(11.5));
                    if let Some(address) = interface.ipv4.first() {
                        ui.label(
                            RichText::new(address).color(cx.palette.muted).size(11.0),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "↓ {}  ↑ {}",
                                format_rate(interface.rx_bps, cx.units),
                                format_rate(interface.tx_bps, cx.units)
                            ))
                            .monospace()
                            .size(11.0),
                        );
                    });
                });
            }

            // Top processes (split ↓/↑ — v2 upgrade over v1's combined).
            let mut shown = 0;
            let top_entries = network.top_processes.clone();
            if let Some(records) = &cx.snapshot.processes {
                for entry in &top_entries {
                    if shown >= 3 {
                        break;
                    }
                    if let Some(record) = records.iter().find(|r| r.pid == entry.pid) {
                        shown += 1;
                        let value = format!(
                            "↓ {} ↑ {}",
                            format_rate(entry.rx_bps, cx.units),
                            format_rate(entry.tx_bps, cx.units)
                        );
                        top_process_row(ui, cx, shown, record, value);
                    }
                }
            }
            if shown == 0
                && network.process_source == ProcessNetSource::None
                && let Some(hint) = &network.process_source_hint
            {
                ui.label(
                    RichText::new(hint)
                        .color(cx.palette.muted)
                        .italics()
                        .size(10.5),
                );
            } else if network.process_source == ProcessNetSource::TcpDiag && shown > 0 {
                ui.label(
                    RichText::new("per-process: TCP sockets (install nethogs for UDP/QUIC)")
                        .color(cx.palette.muted.gamma_multiply(0.8))
                        .size(9.5),
                );
            }
        }
    });
}

pub fn disks_card(ui: &mut Ui, cx: &mut CardContext) {
    card_frame(cx.palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(disks) = &cx.snapshot.disks else {
            card_header(ui, cx, "disks", "Disks", "", "—");
            return;
        };
        let total_read: f64 = disks.iter().map(|d| d.read_bps).sum();
        let total_write: f64 = disks.iter().map(|d| d.write_bps).sum();
        let headline = format!(
            "R {}   W {}",
            format_rate(total_read, cx.units),
            format_rate(total_write, cx.units)
        );
        card_header(ui, cx, "disks", "Disks", "", &headline);

        for disk in disks {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(RichText::new(&disk.display_name).size(12.0)).truncate(),
                )
                .on_hover_text(&disk.device);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "R {}  W {}",
                            format_rate(disk.read_bps, cx.units),
                            format_rate(disk.write_bps, cx.units)
                        ))
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
                            format_size(disk.used_bytes, cx.units),
                            format_size(disk.total_bytes, cx.units)
                        ))
                        .color(cx.palette.muted)
                        .size(10.5),
                    );
                    let fraction = if disk.total_bytes > 0 {
                        disk.used_bytes as f32 / disk.total_bytes as f32
                    } else {
                        0.0
                    };
                    graphs::level_bar(ui, cx.palette, fraction, cx.palette.accent);
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
    });
}

pub fn sensors_card(ui: &mut Ui, cx: &mut CardContext) {
    card_frame(cx.palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(sensors) = &cx.snapshot.sensors else {
            card_header(ui, cx, "sensors", "Sensors", "", "—");
            return;
        };
        // Headline: hottest reading (excluding the GPU card's chip).
        let hottest = sensors
            .chips
            .iter()
            .filter(|chip| chip.name != "amdgpu")
            .flat_map(|chip| chip.temps.iter())
            .map(|t| t.celsius)
            .fold(f32::NAN, f32::max);
        let headline = if hottest.is_nan() {
            "—".to_string()
        } else {
            format_temperature(hottest)
        };
        card_header(ui, cx, "sensors", "Sensors", "", &headline);

        if cx.compact {
            return;
        }
        let mut any = false;
        for chip in &sensors.chips {
            if chip.name == "amdgpu" {
                continue; // the GPU card owns those readings
            }
            for temp in &chip.temps {
                any = true;
                ui.horizontal(|ui| {
                    // A drive chip names its drive ("sda · KINGSTON…"),
                    // or four SATA drives all read "drivetemp · temp1".
                    let row_label = match (&chip.device, &chip.device_model) {
                        (Some(device), Some(model)) => format!("{device} · {model}"),
                        (Some(device), None) => format!("{device} · {}", temp.label),
                        _ => format!("{} · {}", chip.name, temp.label),
                    };
                    ui.label(
                        RichText::new(row_label)
                            .color(cx.palette.muted)
                            .size(11.0),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let mut text =
                            RichText::new(format_temperature(temp.celsius)).monospace().size(11.5);
                        if let Some(critical) = temp.crit_celsius
                            && temp.celsius > critical - 10.0
                        {
                            text = text.color(cx.palette.accent);
                        }
                        ui.label(text);
                    });
                });
            }
            for fan in &chip.fans {
                any = true;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} · {}", chip.name, fan.label))
                            .color(cx.palette.muted)
                            .size(11.0),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let value = match fan.max_rpm {
                            Some(max) => format!("{} / {} rpm", fan.rpm, max),
                            None => format!("{} rpm", fan.rpm),
                        };
                        ui.label(RichText::new(value).monospace().size(11.5));
                    });
                });
            }
            for voltage in &chip.voltages {
                any = true;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} · {}", chip.name, voltage.label))
                            .color(cx.palette.muted)
                            .size(11.0),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{:.3} V", voltage.volts))
                                .monospace()
                                .size(11.5),
                        );
                    });
                });
            }
            for power in &chip.power {
                any = true;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} · {}", chip.name, power.label))
                            .color(cx.palette.muted)
                            .size(11.0),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let value = match power.cap_watts {
                            Some(cap) => format!("{:.1} W · cap {:.0} W", power.watts, cap),
                            None => format!("{:.1} W", power.watts),
                        };
                        ui.label(RichText::new(value).monospace().size(11.5));
                    });
                });
            }
        }
        // Drives report temperatures only through the `drivetemp`
        // (SATA) or `nvme` hwmon drivers — when neither is loaded but
        // disks exist, say what would unlock them instead of showing
        // silently less (the never-hide-a-degraded-mode rule).
        let has_drive_chip = sensors
            .chips
            .iter()
            .any(|chip| chip.name == "drivetemp" || chip.name == "nvme");
        if !has_drive_chip
            && cx.snapshot.disks.as_ref().is_some_and(|disks| !disks.is_empty())
        {
            ui.label(
                RichText::new(
                    "Drive temperatures appear once the kernel's drivetemp \
                     module is loaded (modprobe drivetemp).",
                )
                .color(cx.palette.muted)
                .italics()
                .size(11.0),
            );
        }
        if let Some(battery) = &sensors.battery {
            any = true;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("battery · {}", battery.status.to_lowercase()))
                        .color(cx.palette.muted)
                        .size(11.0),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut value = format!("{:.0}%", battery.percent);
                    if let Some(seconds) = battery.seconds_remaining {
                        value += &format!(
                            " · {}",
                            units::format_duration_seconds(seconds as f64)
                        );
                    }
                    ui.label(RichText::new(value).monospace().size(11.5));
                });
            });
        }
        if !any {
            ui.label(
                RichText::new("No extra sensors beyond what the other cards already show.")
                    .color(cx.palette.muted)
                    .italics()
                    .size(11.0),
            );
        }
    });
}

fn graph_height(cx: &CardContext) -> f32 {
    if cx.compact {
        COMPACT_GRAPH_HEIGHT
    } else {
        FULL_GRAPH_HEIGHT
    }
}
