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
}

impl History {
    pub fn push(&mut self, value: f64) {
        if self.samples.len() >= HISTORY_LENGTH {
            self.samples.pop_front();
        }
        self.samples.push_back(value.max(0.0));
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
pub fn share_bar(ui: &mut Ui, palette: &Palette, segments: &[(Color32, f64)]) {
    let height = 6.0;
    let width = ui.available_width();
    let (rect, _response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
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
}
