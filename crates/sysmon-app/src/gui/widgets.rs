// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared chrome: the carved glyph buttons (pause, pin, menu, pop-out)
//! and the hover-lit menu rows every popup uses. Split out of cards.rs
//! in 3.1 so the card file is only cards.

use egui::{Color32, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::theme::{self, Palette};

// ---------------------------------------------------------------- glyphs

/// Hand-painted glyph buttons — no font-coverage gambling.
#[derive(Clone, Copy, PartialEq)]
pub enum ButtonGlyph {
    Pause,
    Play,
    Pin,
    Menu,
    PopOut,
}

pub fn glyph_button(
    ui: &mut Ui,
    palette: &Palette,
    glyph: ButtonGlyph,
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
        let active_mix = theme::eased_bool(ui.ctx(), response.id, active) * 0.22;
        let hover_mix =
            theme::eased_bool(ui.ctx(), response.id.with("hover"), hovered) * 0.10;
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
            ButtonGlyph::Pause => {
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
            ButtonGlyph::Play => {
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
            ButtonGlyph::Pin => {
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
            ButtonGlyph::Menu => {
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
            ButtonGlyph::PopOut => {
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
        let hover_t = theme::eased_bool(ui.ctx(), response.id.with("hover"), response.hovered());
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


// ------------------------------------------------------------ stone switch

/// A two-position carved stone switch (the Performance page's CPU
/// graph toggle). Both labels sit on one raised plate; the chosen
/// side is sunk in and tinted toward the accent. Returns the newly
/// chosen index when clicked.
pub fn stone_switch(ui: &mut Ui, palette: &Palette, options: [&str; 2], selected: usize, tooltip: &str) -> Option<usize> {
    let font = egui::FontId::monospace(10.5);
    let widths: Vec<f32> = options
        .iter()
        .map(|label| ui.fonts_mut(|f| f.layout_no_wrap(label.to_string(), font.clone(), palette.ink).size().x) + 14.0)
        .collect();
    let size = vec2(widths[0] + widths[1], 18.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    // The accessible name says which side is down (R15); a keyboard
    // activation (no pointer) flips to the other side.
    {
        let state = format!("{tooltip} (now: {})", options[selected]);
        response.widget_info(move || {
            let mut info = egui::WidgetInfo::labeled(egui::WidgetType::Button, true, state.clone());
            info.selected = Some(true);
            info
        });
    }
    let response = response.on_hover_text(tooltip);
    let mut chosen = None;
    if response.clicked() {
        let index = match response.interact_pointer_pos() {
            Some(pointer) => {
                if pointer.x < rect.left() + widths[0] { 0 } else { 1 }
            }
            None => 1 - selected,
        };
        if index != selected {
            chosen = Some(index);
        }
    }
    if !ui.is_rect_visible(rect) {
        return chosen;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, palette.stone);
    let mut x = rect.left();
    for (index, label) in options.iter().enumerate() {
        let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(widths[index], rect.height()));
        let sunk = index == selected;
        let mix = theme::eased_bool(ui.ctx(), response.id.with(index), sunk) * 0.26;
        painter.rect_filled(cell, 0.0, palette.stone.lerp_to_gamma(palette.accent, mix));
        let (top_left, bottom_right) = if sunk {
            (palette.stone_lo, palette.stone_hi)
        } else {
            (palette.stone_hi, palette.stone_lo)
        };
        painter.line_segment([cell.left_top(), cell.right_top()], Stroke::new(1.0, top_left));
        painter.line_segment([cell.left_top(), cell.left_bottom()], Stroke::new(1.0, top_left));
        painter.line_segment([cell.left_bottom(), cell.right_bottom()], Stroke::new(1.0, bottom_right));
        painter.line_segment([cell.right_top(), cell.right_bottom()], Stroke::new(1.0, bottom_right));
        painter.text(
            cell.center() + if sunk { vec2(0.5, 0.5) } else { vec2(0.0, 0.0) },
            egui::Align2::CENTER_CENTER,
            *label,
            font.clone(),
            if sunk { palette.ink } else { palette.ink_2 },
        );
        x += widths[index];
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);
    chosen
}
