// SPDX-License-Identifier: GPL-3.0-or-later
//! History graphs, painted straight onto egui: LINE (thin line, soft
//! fill — GPU, network), AREA (heavier fill — CPU), BARS (discrete —
//! memory), plus the per-core bar row. v1's visual identity in the
//! house frame: sharp corners, hairline strokes, grid at 25/50/75%.
//! Hovering reads out the value under the cursor.

use std::collections::VecDeque;

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::theme::Palette;

pub const HISTORY_LENGTH: usize = 150;

#[derive(Clone, Copy, PartialEq)]
pub enum GraphStyle {
    Line,
    Area,
    Bars,
}

/// One scrolling series buffer (newest at the back).
#[derive(Clone, Default)]
pub struct History {
    samples: VecDeque<f64>,
    /// Every push ever, so a full buffer still knows how far time
    /// has moved (the scope graticule scrolls on this, V5).
    pushed: u64,
}

impl History {
    pub fn push(&mut self, value: f64) {
        if self.samples.len() >= HISTORY_LENGTH {
            self.samples.pop_front();
        }
        self.samples.push_back(value.max(0.0));
        self.pushed += 1;
    }

    pub fn iter(&self) -> impl Iterator<Item = f64> + '_ {
        self.samples.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn max(&self) -> f64 {
        self.samples.iter().copied().fold(0.0, f64::max)
    }

    /// The newest sample, if any.
    pub fn latest(&self) -> Option<f64> {
        self.samples.back().copied()
    }

    pub fn pushed(&self) -> u64 {
        self.pushed
    }

    /// index_from_newest: 0 = latest sample.
    pub fn value_from_newest(&self, index_from_newest: usize) -> Option<f64> {
        if index_from_newest >= self.samples.len() {
            return None;
        }
        self.samples
            .get(self.samples.len() - 1 - index_from_newest)
            .copied()
    }
}

pub struct GraphConfig<'a> {
    pub style: GraphStyle,
    pub height: f32,
    /// None → autoscale to the observed maximum.
    pub fixed_maximum: Option<f64>,
    pub minimum_autoscale: f64,
    pub colors: &'a [Color32],
    /// Formats the values under the cursor for the hover readout.
    pub hover_formatter: &'a dyn Fn(&[f64]) -> String,
}

// ------------------------------------------------------------ scope graph
//
// The Performance page's instrument (docs/dev/PERFORMANCE-VIEW.md):
// a dark field, a graticule that scrolls with the samples, up to two
// traces on one scale, the autoscale ceiling written in the corner.

/// Vertical graticule pitch, in samples.
pub const GRATICULE_PITCH: usize = 15;

pub struct ScopeConfig<'a> {
    /// What this graph is, for screen readers and the test harness
    /// ("CPU history"). The accessible label is this plus the newest
    /// readout, so a test can check the drawn value (V1, V3).
    pub name: &'a str,
    pub height: f32,
    /// Some(100) for percentages; None autoscales (and writes the
    /// ceiling in the corner, V4).
    pub fixed_maximum: Option<f64>,
    pub minimum_autoscale: f64,
    /// One colour per series; the first series also gets a soft
    /// fill, the rest are bare lines.
    pub colors: &'a [Color32],
    /// Draw the second series dashed (greyscale theme, V6).
    pub dashed_second: bool,
    /// Formats the autoscale ceiling for the corner label.
    pub ceiling_formatter: &'a dyn Fn(f64) -> String,
    pub hover_formatter: &'a dyn Fn(&[f64]) -> String,
    /// A short tag in the top-left of the field (a thread number).
    pub tag: Option<&'a str>,
    /// Hairline graticule only (mini graphs skip the labels).
    pub mini: bool,
}

/// Where the vertical graticule lines sit, in samples from the newest.
/// Pure in the push count, so the grid moves exactly one step per
/// sample and never per frame (V5). With reduced motion the grid is
/// fixed.
pub fn graticule_offset(pushed: u64, reduced_motion: bool) -> usize {
    if reduced_motion {
        0
    } else {
        (pushed % GRATICULE_PITCH as u64) as usize
    }
}

/// The scale a scope graph draws against: fixed, or 15 % above the
/// observed maximum, never below the floor. Always ≥ every sample
/// (V4: a spike is never clipped, the ceiling is written instead).
pub fn scope_ceiling(series: &[&History], fixed_maximum: Option<f64>, minimum_autoscale: f64) -> f64 {
    fixed_maximum.unwrap_or_else(|| {
        let observed = series.iter().map(|s| s.max()).fold(0.0, f64::max);
        (observed * 1.15).max(minimum_autoscale)
    })
}

/// Draw a scope graph. Returns the field's response (click = the
/// caller's business; hover is read out here).
pub fn scope_graph(ui: &mut Ui, palette: &Palette, series: &[&History], config: &ScopeConfig) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, config.height), Sense::click());
    {
        let latest: Vec<f64> = series.iter().filter_map(|h| h.latest()).collect();
        let readout = if latest.len() == series.len() && !latest.is_empty() {
            (config.hover_formatter)(&latest)
        } else {
            String::from("no samples yet")
        };
        let name = config.name;
        response.widget_info(move || {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{name}: {readout}"))
        });
    }
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);

    // The field, carved a hair into the surface: a dark fill, the
    // hairline frame, and a one-pixel shadow along the top and left
    // so the trace sits *in* the panel rather than on it.
    painter.rect_filled(rect, 0.0, palette.field());
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);
    let shadow = Stroke::new(1.0, Color32::BLACK.gamma_multiply(0.35));
    painter.line_segment([pos2(rect.left() + 1.0, rect.top() + 1.5), pos2(rect.right() - 1.0, rect.top() + 1.5)], shadow);
    painter.line_segment([pos2(rect.left() + 1.5, rect.top() + 1.0), pos2(rect.left() + 1.5, rect.bottom() - 1.0)], shadow);

    let padding = 2.0f32;
    let drawable = rect.shrink(padding);
    let step = drawable.width() / (HISTORY_LENGTH.saturating_sub(1)) as f32;

    // Graticule: horizontal quarters, vertical lines that ride the
    // samples. One shape each, hairline, accent at low alpha.
    let grid = Stroke::new(1.0, palette.graticule());
    for fraction in [0.25f32, 0.5, 0.75] {
        let y = drawable.top() + drawable.height() * fraction;
        painter.line_segment([pos2(drawable.left(), y), pos2(drawable.right(), y)], grid);
    }
    let pushed = series.iter().map(|s| s.pushed()).max().unwrap_or(0);
    let offset = graticule_offset(pushed, super::theme::prefers_reduced_motion());
    let mut from_newest = offset;
    while from_newest < HISTORY_LENGTH {
        let x = drawable.right() - from_newest as f32 * step;
        painter.line_segment([pos2(x, drawable.top()), pos2(x, drawable.bottom())], grid);
        from_newest += GRATICULE_PITCH;
    }

    let maximum = scope_ceiling(series, config.fixed_maximum, config.minimum_autoscale);
    if maximum > 0.0 {
        for (series_index, history) in series.iter().enumerate() {
            if history.is_empty() {
                continue;
            }
            let color = config.colors.get(series_index).copied().unwrap_or(palette.accent);
            let count = history.len();
            let points: Vec<Pos2> = history
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let from_newest = (count - 1 - index) as f32;
                    let x = drawable.right() - from_newest * step;
                    let y = drawable.bottom() - ((value / maximum).min(1.0) as f32) * drawable.height();
                    pos2(x, y)
                })
                .collect();
            if series_index == 0 {
                // Soft fill under the first trace, one trapezoid per
                // segment (a single concave polygon tessellates into
                // stripes).
                let fill_color = color.gamma_multiply(0.22);
                for pair in points.windows(2) {
                    painter.add(egui::Shape::convex_polygon(
                        vec![pos2(pair[0].x, drawable.bottom()), pair[0], pair[1], pos2(pair[1].x, drawable.bottom())],
                        fill_color,
                        Stroke::NONE,
                    ));
                }
            }
            let stroke = Stroke::new(if config.mini { 1.0 } else { 1.4 }, color);
            if series_index == 1 && config.dashed_second {
                painter.add(egui::Shape::dashed_line(&points, stroke, 4.0, 3.0));
            } else {
                painter.add(egui::Shape::line(points, stroke));
            }
        }
    }

    // Corner labels, on the field's own ink.
    if let Some(tag) = config.tag {
        painter.text(
            drawable.left_top() + vec2(3.0, 1.0),
            egui::Align2::LEFT_TOP,
            tag,
            egui::FontId::monospace(if config.mini { 9.0 } else { 10.0 }),
            palette.on_field_muted(),
        );
    }
    if config.fixed_maximum.is_none() && !config.mini {
        painter.text(
            drawable.right_top() + vec2(-3.0, 1.0),
            egui::Align2::RIGHT_TOP,
            format!("↑ {}", (config.ceiling_formatter)(maximum)),
            egui::FontId::monospace(9.5),
            palette.on_field_muted(),
        );
    }

    // Hover readout.
    if let Some(pointer) = response.hover_pos() {
        let from_newest = ((drawable.right() - pointer.x) / step).round().max(0.0) as usize;
        let values: Vec<f64> = series
            .iter()
            .filter_map(|history| history.value_from_newest(from_newest))
            .collect();
        if values.len() == series.len() && !values.is_empty() {
            let text = (config.hover_formatter)(&values);
            response.clone().on_hover_ui_at_pointer(|ui| {
                ui.monospace(text);
            });
        }
    }
    response
}

/// The LED meter: a vertical column of segments lit from the bottom,
/// the value in monospace under it. One click target with its label.
pub const LED_SEGMENTS: usize = 20;
pub const LED_WIDTH: f32 = 18.0;

pub fn led_meter(ui: &mut Ui, palette: &Palette, name: &str, fraction: f32, color: Color32, label: &str, height: f32) -> egui::Response {
    let label_height = 14.0;
    let label_width = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(label.to_string(), egui::FontId::monospace(10.5), palette.ink)
            .size()
            .x
    });
    let width = label_width.max(LED_WIDTH + 8.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height + label_height), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{name} meter: {label}")));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    let column = Rect::from_min_size(pos2(rect.center().x - LED_WIDTH / 2.0, rect.top()), vec2(LED_WIDTH, height));
    painter.rect_filled(column, 0.0, palette.field());
    painter.rect_stroke(column, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);
    let lit = (fraction.clamp(0.0, 1.0) * LED_SEGMENTS as f32).round() as usize;
    let pitch = (height - 2.0) / LED_SEGMENTS as f32;
    let segment_height = (pitch - 1.0).max(1.0);
    for index in 0..LED_SEGMENTS {
        let bottom = column.bottom() - 1.0 - index as f32 * pitch;
        let segment = Rect::from_min_max(
            pos2(column.left() + 2.0, bottom - segment_height),
            pos2(column.right() - 2.0, bottom),
        );
        let fill = if index < lit {
            // The top of a lit column glows a touch brighter.
            color.gamma_multiply(if index + 1 == lit { 1.0 } else { 0.85 })
        } else {
            palette.on_field().gamma_multiply(0.07)
        };
        painter.rect_filled(segment, 0.0, fill);
    }
    painter.text(
        pos2(rect.center().x, rect.bottom()),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::monospace(10.5),
        if response.hovered() { palette.ink } else { palette.value },
    );
    response
}

/// An etched group box: hairline frame with the title set into the
/// top edge, a faint carved highlight inside. The lower tier of the
/// page, so the carving is quiet.
pub fn group_box<R>(ui: &mut Ui, palette: &Palette, title: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let title_font = egui::FontId::proportional(11.0);
    let title_galley = ui.fonts_mut(|fonts| fonts.layout_no_wrap(title.to_string(), title_font.clone(), palette.title));
    let title_height = title_galley.size().y;
    let outer_top = ui.cursor().min.y;
    let frame_top = outer_top + title_height / 2.0;
    let inner_margin = egui::Margin { left: 8, right: 8, top: (title_height / 2.0 + 6.0) as i8, bottom: 7 };
    let frame = egui::Frame::new().inner_margin(inner_margin);
    let response = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        add_contents(ui)
    });
    let rect = response.response.rect;
    let frame_rect = Rect::from_min_max(pos2(rect.left(), frame_top), rect.right_bottom());
    let painter = ui.painter();
    // Carved: a light line inside the top/left, a dark one inside
    // the bottom/right, under the hairline.
    let inner = frame_rect.shrink(1.0);
    painter.line_segment([inner.left_top(), inner.right_top()], Stroke::new(1.0, palette.stone_hi.gamma_multiply(0.5)));
    painter.line_segment([inner.left_top(), inner.left_bottom()], Stroke::new(1.0, palette.stone_hi.gamma_multiply(0.5)));
    painter.line_segment([inner.left_bottom(), inner.right_bottom()], Stroke::new(1.0, palette.stone_lo.gamma_multiply(0.5)));
    painter.line_segment([inner.right_top(), inner.right_bottom()], Stroke::new(1.0, palette.stone_lo.gamma_multiply(0.5)));
    painter.rect_stroke(frame_rect, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);
    // The title breaks the top edge: paint the surface behind it,
    // then the text.
    let title_pos = pos2(frame_rect.left() + 8.0, outer_top);
    let title_rect = Rect::from_min_size(title_pos - vec2(3.0, 0.0), title_galley.size() + vec2(6.0, 0.0));
    painter.rect_filled(title_rect, 0.0, palette.surface);
    painter.galley(title_pos, title_galley, palette.title);
    response.inner
}

/// Draw one graph over any number of series (they share the scale).
pub fn history_graph(ui: &mut Ui, palette: &Palette, series: &[&History], config: &GraphConfig) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, config.height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);

    // Frame + faint fill, sharp.
    painter.rect_filled(rect, 0.0, palette.surface_2.gamma_multiply(0.6));
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);

    // Grid at quarters.
    for fraction in [0.25f32, 0.5, 0.75] {
        let y = rect.top() + rect.height() * fraction;
        painter.line_segment(
            [pos2(rect.left() + 1.0, y), pos2(rect.right() - 1.0, y)],
            Stroke::new(1.0, palette.line.gamma_multiply(0.55)),
        );
    }

    let maximum = config.fixed_maximum.unwrap_or_else(|| {
        let observed = series.iter().map(|s| s.max()).fold(0.0, f64::max);
        (observed * 1.15).max(config.minimum_autoscale)
    });
    if maximum <= 0.0 {
        return;
    }

    let padding = 3.0f32;
    let drawable = rect.shrink(padding);
    let step = drawable.width() / (HISTORY_LENGTH.saturating_sub(1)) as f32;

    for (series_index, history) in series.iter().enumerate() {
        if history.is_empty() {
            continue;
        }
        let color = config
            .colors
            .get(series_index)
            .copied()
            .unwrap_or(palette.accent);
        let count = history.len();
        let points: Vec<Pos2> = history
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let from_newest = (count - 1 - index) as f32;
                let x = drawable.right() - from_newest * step;
                let y = drawable.bottom()
                    - ((value / maximum).min(1.0) as f32) * drawable.height();
                pos2(x, y)
            })
            .collect();

        match config.style {
            GraphStyle::Bars => {
                let bar_width = (step * 0.65).max(1.0);
                for point in &points {
                    let bar_height = (drawable.bottom() - point.y).max(0.75);
                    painter.rect_filled(
                        Rect::from_min_max(
                            pos2(point.x - bar_width / 2.0, drawable.bottom() - bar_height),
                            pos2(point.x + bar_width / 2.0, drawable.bottom()),
                        ),
                        0.0,
                        color.gamma_multiply(0.85),
                    );
                }
            }
            GraphStyle::Line | GraphStyle::Area => {
                let fill_alpha = if config.style == GraphStyle::Area {
                    0.32
                } else {
                    0.14
                };
                // Fill under the curve as one trapezoid per segment —
                // a single concave polygon tessellates into stripes.
                let fill_color = color.gamma_multiply(fill_alpha);
                for pair in points.windows(2) {
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            pos2(pair[0].x, drawable.bottom()),
                            pair[0],
                            pair[1],
                            pos2(pair[1].x, drawable.bottom()),
                        ],
                        fill_color,
                        Stroke::NONE,
                    ));
                }
                let line_width = if config.style == GraphStyle::Area {
                    1.3
                } else {
                    1.6
                };
                painter.add(egui::Shape::line(
                    points,
                    Stroke::new(line_width, color.gamma_multiply(0.95)),
                ));
            }
        }
    }

    // Hover readout.
    if let Some(pointer) = response.hover_pos() {
        let from_newest = ((drawable.right() - pointer.x) / step).round().max(0.0) as usize;
        let values: Vec<f64> = series
            .iter()
            .filter_map(|history| history.value_from_newest(from_newest))
            .collect();
        if values.len() == series.len() && !values.is_empty() {
            let text = (config.hover_formatter)(&values);
            response.on_hover_ui_at_pointer(|ui| {
                ui.monospace(text);
            });
        }
    }
}

/// The row of per-core bars under the CPU graph. Hover names the
/// core and its number.
pub fn per_core_bars(ui: &mut Ui, palette: &Palette, percentages: &[f32], color: Color32) {
    if percentages.is_empty() {
        return;
    }
    let height = 40.0;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    let count = percentages.len();
    let gap = 3.0;
    let bar_width = ((width - gap * (count as f32 - 1.0)) / count as f32).max(2.0);

    for (index, percent) in percentages.iter().enumerate() {
        let x = rect.left() + index as f32 * (bar_width + gap);
        let track = Rect::from_min_size(pos2(x, rect.top()), vec2(bar_width, height));
        painter.rect_filled(track, 0.0, palette.ink.gamma_multiply(0.08));
        let fill_height = (height * percent.min(100.0) / 100.0).max(1.5);
        painter.rect_filled(
            Rect::from_min_max(
                pos2(x, rect.bottom() - fill_height),
                pos2(x + bar_width, rect.bottom()),
            ),
            0.0,
            color.gamma_multiply(0.9),
        );
    }

    if let Some(pointer) = response.hover_pos() {
        let index = ((pointer.x - rect.left()) / (bar_width + gap)) as usize;
        if index < count {
            let percent = percentages[index];
            response.on_hover_ui_at_pointer(|ui| {
                ui.monospace(format!("Core {index} — {percent:.0}%"));
            });
        }
    }
}

/// A slim, sharp level bar (VRAM, disk usage).
pub fn level_bar(ui: &mut Ui, palette: &Palette, fraction: f32, color: Color32) {
    let height = 6.0;
    let width = ui.available_width();
    let (rect, _response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, palette.ink.gamma_multiply(0.08));
    let filled = Rect::from_min_size(
        rect.min,
        vec2(rect.width() * fraction.clamp(0.0, 1.0), height),
    );
    painter.rect_filled(filled, 0.0, color.gamma_multiply(0.85));
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);
}

/// A stacked share bar: each segment's width is its share of the
/// summed values, in its own color — how the combined-details window
/// shows who owns how much of each metric. Zero-sum renders the empty
/// rail (honest: nothing to apportion).
pub fn share_bar(ui: &mut Ui, palette: &Palette, segments: &[(Color32, f64)]) -> egui::Response {
    let height = 6.0;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, palette.ink.gamma_multiply(0.08));
    let total: f64 = segments.iter().map(|(_, value)| value.max(0.0)).sum();
    if total > 0.0 {
        let mut x = rect.min.x;
        for (color, value) in segments {
            let share = (value.max(0.0) / total) as f32;
            let segment_width = rect.width() * share;
            painter.rect_filled(
                Rect::from_min_size(pos2(x, rect.min.y), vec2(segment_width, height)),
                0.0,
                color.gamma_multiply(0.85),
            );
            x += segment_width;
        }
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);
    response
}

/// A small single-series line for the Inspector (no grid, no hover).
pub fn sparkline(ui: &mut Ui, palette: &Palette, values: &[f64], height: f32, color: Color32, maximum: Option<f64>) {
    let width = ui.available_width();
    let (rect, _response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) || values.is_empty() {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, palette.surface_2.gamma_multiply(0.6));
    // With a given maximum (a percentage), scale 0..max. Without one,
    // scale the series' own min..max: an RSS that moves 2 MB on 240 MB
    // is otherwise a flat line pinned to the top.
    let (floor, top) = match maximum {
        Some(maximum) => (0.0, maximum.max(1e-9)),
        None => {
            let low = values.iter().copied().fold(f64::INFINITY, f64::min);
            let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let pad = ((high - low) * 0.15).max(high.abs() * 0.002).max(1e-9);
            ((low - pad).max(0.0), high + pad)
        }
    };
    let step = rect.width() / (HISTORY_LENGTH.saturating_sub(1)) as f32;
    let count = values.len();
    let points: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let x = rect.right() - (count - 1 - index) as f32 * step;
            let share = ((value - floor) / (top - floor)).clamp(0.0, 1.0) as f32;
            let y = rect.bottom() - share * (rect.height() - 2.0) - 1.0;
            pos2(x, y)
        })
        .collect();
    for pair in points.windows(2) {
        painter.add(egui::Shape::convex_polygon(
            vec![pos2(pair[0].x, rect.bottom()), pair[0], pair[1], pos2(pair[1].x, rect.bottom())],
            color.gamma_multiply(0.18),
            Stroke::NONE,
        ));
    }
    painter.add(egui::Shape::line(points, Stroke::new(1.3, color)));
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V5: the graticule rides the samples. Fifteen pushes later the
    /// grid is in the same place; one push later it has moved one.
    #[test]
    fn the_graticule_moves_one_step_per_sample() {
        let mut history = History::default();
        for _ in 0..(HISTORY_LENGTH + 7) {
            history.push(1.0);
        }
        let before = graticule_offset(history.pushed(), false);
        history.push(1.0);
        assert_eq!(graticule_offset(history.pushed(), false), (before + 1) % GRATICULE_PITCH);
        for _ in 0..(GRATICULE_PITCH - 1) {
            history.push(1.0);
        }
        assert_eq!(graticule_offset(history.pushed(), false), before);
        assert_eq!(graticule_offset(history.pushed(), true), 0, "reduced motion: fixed grid");
    }

    /// V4: the ceiling is never below a sample, and a floor keeps a
    /// quiet graph from autoscaling noise to full height.
    #[test]
    fn the_scope_ceiling_covers_every_sample_and_respects_the_floor() {
        let mut spike = History::default();
        spike.push(2.0e9);
        spike.push(5.0e6);
        let mut quiet = History::default();
        quiet.push(300.0);
        assert!(scope_ceiling(&[&spike, &quiet], None, 1.0e6) >= 2.0e9);
        assert_eq!(scope_ceiling(&[&quiet], None, 1.0e6), 1.0e6);
        assert_eq!(scope_ceiling(&[&spike], Some(100.0), 1.0), 100.0);
    }
}
