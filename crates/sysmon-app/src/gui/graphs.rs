// SPDX-License-Identifier: GPL-3.0-or-later
//! History graphs, painted straight onto egui. One instrument, the
//! scope (docs/dev/OVERVIEW-VIEW.md): a dark field tinted per theme,
//! a graticule that moves one step per sample, traces on one scale,
//! the autoscale ceiling written in the corner. The Overview's cards
//! and the pop-outs draw every graph with it, plus the CPU's two
//! per-thread views (one line per thread on one field, or a grid of
//! small fields), the per-thread bars, and the slim level / share
//! bars. Sharp corners, hairline strokes. Hovering reads out the
//! value under the cursor.

use std::collections::VecDeque;

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::theme::Palette;

pub const HISTORY_LENGTH: usize = 150;

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

// ------------------------------------------------------------ scope graph

/// Vertical graticule pitch, in samples.
pub const GRATICULE_PITCH: usize = 15;
/// The per-thread grid's floor: a cell never gets shorter than this.
pub const MINI_GRAPH_MIN_HEIGHT: f32 = 28.0;

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
    /// Fill under every series, not just the first: the memory graph
    /// draws cache+used first and used on top of it, so the band
    /// between the two traces IS the cache (stacked, R8).
    pub fill_all: bool,
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

/// The field every scope draws on first: a dark fill, the hairline
/// frame, (full size only) a one-pixel shadow along the top and left
/// so the trace sits *in* the panel, and the graticule. Returns the
/// drawable rect and the x step per sample.
fn scope_field(ui: &Ui, painter: &egui::Painter, palette: &Palette, rect: Rect, mini: bool, pushed: u64) -> (Rect, f32) {
    painter.rect_filled(rect, 0.0, palette.field());
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, palette.line), StrokeKind::Inside);
    if !mini {
        let shadow = Stroke::new(1.0, Color32::BLACK.gamma_multiply(0.35));
        painter.line_segment([pos2(rect.left() + 1.0, rect.top() + 1.5), pos2(rect.right() - 1.0, rect.top() + 1.5)], shadow);
        painter.line_segment([pos2(rect.left() + 1.5, rect.top() + 1.0), pos2(rect.left() + 1.5, rect.bottom() - 1.0)], shadow);
    }
    let drawable = rect.shrink(2.0);
    let step = drawable.width() / (HISTORY_LENGTH.saturating_sub(1)) as f32;
    // Graticule: horizontal quarters, vertical lines that ride the
    // samples. A mini graph keeps only the midline and every other
    // vertical: 32 of them must stay cheap (V8), and at 28 px the
    // quarters would be noise anyway.
    let grid = Stroke::new(1.0, palette.graticule());
    let fractions: &[f32] = if mini { &[0.5] } else { &[0.25, 0.5, 0.75] };
    for fraction in fractions {
        let y = drawable.top() + drawable.height() * fraction;
        painter.line_segment([pos2(drawable.left(), y), pos2(drawable.right(), y)], grid);
    }
    let pitch = if mini { GRATICULE_PITCH * 2 } else { GRATICULE_PITCH };
    let mut from_newest = graticule_offset(pushed, super::theme::reduced_motion(ui.ctx()));
    while from_newest < HISTORY_LENGTH {
        let x = drawable.right() - from_newest as f32 * step;
        painter.line_segment([pos2(x, drawable.top()), pos2(x, drawable.bottom())], grid);
        from_newest += pitch;
    }
    (drawable, step)
}

/// One series as points on the field, newest at the right edge.
fn trace_points(history: &History, drawable: Rect, step: f32, maximum: f64) -> Vec<Pos2> {
    let count = history.len();
    let mut points: Vec<Pos2> = Vec::with_capacity(count);
    points.extend(history.iter().enumerate().map(|(index, value)| {
        let from_newest = (count - 1 - index) as f32;
        let x = drawable.right() - from_newest * step;
        let y = drawable.bottom() - ((value / maximum).min(1.0) as f32) * drawable.height();
        pos2(x, y)
    }));
    points
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
    let pushed = series.iter().map(|s| s.pushed()).max().unwrap_or(0);
    let (drawable, step) = scope_field(ui, &painter, palette, rect, config.mini, pushed);

    let maximum = scope_ceiling(series, config.fixed_maximum, config.minimum_autoscale);
    if maximum > 0.0 {
        // One exact-capacity points buffer per series, handed to egui
        // by value (Shape::line owns it): one allocation per trace,
        // none per point.
        for (series_index, history) in series.iter().enumerate() {
            if history.is_empty() {
                continue;
            }
            let color = config.colors.get(series_index).copied().unwrap_or(palette.accent);
            let points = trace_points(history, drawable, step, maximum);
            if (series_index == 0 || config.fill_all) && !config.mini {
                // Soft fill under the first trace as ONE mesh: a
                // triangle strip between the trace and the baseline.
                // (A single concave polygon tessellates into stripes;
                // one convex polygon per segment was 149 allocations
                // per graph per frame, reviewer R14.)
                painter.add(fill_mesh(&points, drawable.bottom(), color.gamma_multiply(0.22)));
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

/// The soft fill under a trace as one mesh (a triangle strip between
/// the trace and the baseline): one shape, one allocation.
pub(crate) fn fill_mesh(points: &[Pos2], baseline: f32, color: Color32) -> egui::Shape {
    let mut mesh = egui::Mesh::default();
    if points.len() < 2 {
        return egui::Shape::Mesh(std::sync::Arc::new(mesh));
    }
    mesh.vertices.reserve(points.len() * 2);
    mesh.indices.reserve((points.len() - 1) * 6);
    for (index, point) in points.iter().enumerate() {
        mesh.colored_vertex(*point, color);
        mesh.colored_vertex(pos2(point.x, baseline), color);
        if index > 0 {
            let base = (index as u32 - 1) * 2;
            // Two triangles per segment: (top0, base0, top1), (top1, base0, base1).
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 2, base + 1, base + 3);
        }
    }
    egui::Shape::Mesh(std::sync::Arc::new(mesh))
}

/// A legend swatch + word per series, for the row above a graph: the
/// colour's meaning is stated where the colour is used.
pub fn legend(ui: &mut Ui, entries: &[(Color32, &str)]) {
    for (color, word) in entries {
        let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, *color);
        // A bare swatch (a run of thread colours) takes no label.
        if !word.is_empty() {
            ui.label(egui::RichText::new(*word).size(10.0).monospace().color(ui.visuals().weak_text_color()));
        }
    }
}

// ------------------------------------------------------ per-thread views

/// The thread a reader is singling out, shared by the thread lines and
/// the bars under them. The bars write it (one frame late at most);
/// pointing at a line overrides it for that frame.
fn bars_focus_id(focus_id: egui::Id) -> egui::Id {
    focus_id.with("bars")
}

/// Which thread's line is nearest the pointer at one sample.
fn nearest_thread(per_core: &[History], from_newest: usize, drawable: Rect, pointer_y: f32) -> Option<(usize, f64)> {
    per_core
        .iter()
        .enumerate()
        .filter_map(|(index, history)| history.value_from_newest(from_newest).map(|value| (index, value)))
        .min_by(|(_, a), (_, b)| {
            let y = |v: f64| drawable.bottom() - ((v / 100.0).min(1.0) as f32) * drawable.height();
            (y(*a) - pointer_y).abs().total_cmp(&(y(*b) - pointer_y).abs())
        })
}

/// Every CPU thread as its own coloured line on one field, the way
/// desktop task managers draw it (Ben, 3.6). Point at a line (or at a
/// bar under the graph) and that thread is drawn bold over the others,
/// which fade back; the readout names it and the busiest threads at
/// that moment.
pub fn thread_lines_graph(
    ui: &mut Ui,
    palette: &Palette,
    per_core: &[History],
    colors: &[Color32],
    height: f32,
    focus_id: egui::Id,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    {
        let busiest = per_core
            .iter()
            .enumerate()
            .filter_map(|(index, history)| history.latest().map(|value| (index, value)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let readout = match busiest {
            Some((index, value)) => format!("{} lines, busiest thread {index} {value:.0}%", per_core.len()),
            None => String::from("no samples yet"),
        };
        response.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("CPU threads: {readout}")));
    }
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    let pushed = per_core.iter().map(|s| s.pushed()).max().unwrap_or(0);
    let (drawable, step) = scope_field(ui, &painter, palette, rect, false, pushed);

    // The thread under the pointer, else the bar under it (last frame).
    let hovered = response.hover_pos().and_then(|pointer| {
        let from_newest = ((drawable.right() - pointer.x) / step).round().max(0.0) as usize;
        nearest_thread(per_core, from_newest, drawable, pointer.y).map(|(index, _)| (index, from_newest))
    });
    let focus = hovered
        .map(|(index, _)| index)
        .or_else(|| ui.ctx().data(|data| data.get_temp::<usize>(bars_focus_id(focus_id))));

    let color_of = |index: usize| colors.get(index).copied().unwrap_or(palette.accent);
    for (index, history) in per_core.iter().enumerate() {
        if history.is_empty() || Some(index) == focus {
            continue;
        }
        // With a thread singled out the rest step back, still there.
        let color = if focus.is_some() { color_of(index).gamma_multiply(0.22) } else { color_of(index).gamma_multiply(0.9) };
        painter.add(egui::Shape::line(trace_points(history, drawable, step, 100.0), Stroke::new(1.0, color)));
    }
    if let Some(index) = focus
        && let Some(history) = per_core.get(index)
        && !history.is_empty()
    {
        let points = trace_points(history, drawable, step, 100.0);
        painter.add(fill_mesh(&points, drawable.bottom(), color_of(index).gamma_multiply(0.18)));
        painter.add(egui::Shape::line(points, Stroke::new(2.0, color_of(index))));
        painter.text(
            drawable.left_top() + vec2(3.0, 1.0),
            egui::Align2::LEFT_TOP,
            format!("thread {index}"),
            egui::FontId::monospace(10.0),
            color_of(index),
        );
    }

    if let Some((index, from_newest)) = hovered {
        let value = per_core[index].value_from_newest(from_newest).unwrap_or(0.0);
        let mut at_that_moment: Vec<(usize, f64)> = per_core
            .iter()
            .enumerate()
            .filter_map(|(thread, history)| history.value_from_newest(from_newest).map(|v| (thread, v)))
            .collect();
        at_that_moment.sort_by(|a, b| b.1.total_cmp(&a.1));
        let busiest: Vec<String> = at_that_moment.iter().take(3).map(|(thread, v)| format!("{thread} {v:.0}%")).collect();
        let average = at_that_moment.iter().map(|(_, v)| v).sum::<f64>() / at_that_moment.len().max(1) as f64;
        let text = format!("thread {index}: {value:.0}%\nbusiest: {}\nall threads: {average:.0}%", busiest.join(" · "));
        response.clone().on_hover_ui_at_pointer(|ui| {
            ui.monospace(text);
        });
    }
    response
}

/// The per-thread grid: a small scope per thread, columns following the
/// width, /proc/stat order row-major (V3), each tagged with its number
/// and drawn in its thread colour. The grid stays near `budget` tall
/// but never squashes a cell below the floor.
pub fn per_thread_grid(
    ui: &mut Ui,
    palette: &Palette,
    per_core: &[History],
    colors: &[Color32],
    budget: f32,
) -> egui::Response {
    let core_count = per_core.len();
    let width = ui.available_width();
    let columns = per_thread_columns(width);
    let rows = core_count.div_ceil(columns).max(1);
    let gap = 2.0;
    let cell_width = (width - gap * (columns as f32 - 1.0)) / columns as f32;
    let cell_height = ((budget - gap * (rows as f32 - 1.0)) / rows as f32).max(MINI_GRAPH_MIN_HEIGHT);
    let total_height = cell_height * rows as f32 + gap * (rows as f32 - 1.0);
    let (outer, outer_response) = ui.allocate_exact_size(vec2(width, total_height), Sense::click());
    outer_response.widget_info(move || {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("CPU per-thread grid: {core_count} threads"))
    });
    let formatter = |values: &[f64]| format!("{:.0}%", values[0]);
    for (index, history) in per_core.iter().enumerate() {
        let row = index / columns;
        let column = index % columns;
        let min = outer.min + vec2(column as f32 * (cell_width + gap), row as f32 * (cell_height + gap));
        let cell = Rect::from_min_size(min, vec2(cell_width, cell_height));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cell).layout(egui::Layout::top_down(egui::Align::Min)));
        let tag = index.to_string();
        let name = format!("CPU thread {index}");
        let color = colors.get(index).copied().unwrap_or(palette.accent);
        scope_graph(
            &mut child,
            palette,
            &[history],
            &ScopeConfig {
                name: &name,
                height: cell_height,
                fixed_maximum: Some(100.0),
                minimum_autoscale: 100.0,
                colors: &[color],
                dashed_second: false,
                ceiling_formatter: &|v| format!("{v:.0}%"),
                hover_formatter: &formatter,
                tag: Some(&tag),
                mini: true,
                fill_all: false,
            },
        );
    }
    outer_response
}

/// Columns of the per-thread grid for a width (V9): never fewer than
/// 2, never more than 8, each at least ~96 px.
pub fn per_thread_columns(width: f32) -> usize {
    ((width / 96.0).floor() as usize).clamp(2, 8)
}

/// The row of per-thread bars under the CPU graph: each thread's load
/// now. With one colour per thread (the thread views) the bars are the
/// lines' legend too, and pointing at a bar singles that thread out in
/// the graph above (`focus_id`). One colour paints them all.
pub fn per_core_bars(ui: &mut Ui, palette: &Palette, percentages: &[f32], colors: &[Color32], focus_id: Option<egui::Id>) {
    if percentages.is_empty() {
        return;
    }
    let height = 40.0;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let count = percentages.len();
    let gap = 3.0;
    let bar_width = ((width - gap * (count as f32 - 1.0)) / count as f32).max(2.0);
    let hovered = response.hover_pos().and_then(|pointer| {
        let index = ((pointer.x - rect.left()) / (bar_width + gap)) as usize;
        (index < count).then_some(index)
    });
    if let Some(focus_id) = focus_id {
        let id = bars_focus_id(focus_id);
        let before = ui.ctx().data(|data| data.get_temp::<usize>(id));
        if before != hovered {
            ui.ctx().data_mut(|data| match hovered {
                Some(index) => data.insert_temp(id, index),
                None => data.remove::<usize>(id),
            });
            // The graph above was already drawn this frame.
            ui.ctx().request_repaint();
        }
    }
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    for (index, percent) in percentages.iter().enumerate() {
        let x = rect.left() + index as f32 * (bar_width + gap);
        let track = Rect::from_min_size(pos2(x, rect.top()), vec2(bar_width, height));
        painter.rect_filled(track, 0.0, palette.ink.gamma_multiply(0.08));
        let color = colors.get(index).or(colors.first()).copied().unwrap_or(palette.accent);
        let fill_height = (height * percent.min(100.0) / 100.0).max(1.5);
        let dimmed = hovered.is_some_and(|h| h != index) && colors.len() > 1;
        painter.rect_filled(
            Rect::from_min_max(pos2(x, rect.bottom() - fill_height), pos2(x + bar_width, rect.bottom())),
            0.0,
            color.gamma_multiply(if dimmed { 0.35 } else { 0.9 }),
        );
    }
    if let Some(index) = hovered {
        let percent = percentages[index];
        response.on_hover_ui_at_pointer(|ui| {
            ui.monospace(format!("Thread {index} — {percent:.0}%"));
        });
    }
}

// ------------------------------------------------------------ slim bars

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
    painter.add(fill_mesh(&points, rect.bottom(), color.gamma_multiply(0.18)));
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

    #[test]
    fn columns_follow_the_width_within_bounds() {
        assert_eq!(per_thread_columns(100.0), 2);
        assert_eq!(per_thread_columns(300.0), 3);
        assert_eq!(per_thread_columns(400.0), 4);
        assert_eq!(per_thread_columns(1200.0), 8);
        assert_eq!(per_thread_columns(5000.0), 8);
    }

    /// Pointing at the field picks the thread whose line is closest to
    /// the pointer at that sample — the one the eye is on.
    #[test]
    fn the_nearest_line_is_the_one_under_the_pointer() {
        let field = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 100.0));
        let mut per_core = vec![History::default(); 3];
        for (history, value) in per_core.iter_mut().zip([10.0, 50.0, 90.0]) {
            history.push(value);
        }
        // y = bottom − v% of height: 90 % sits at y 10, 10 % at y 90.
        assert_eq!(nearest_thread(&per_core, 0, field, 12.0).map(|t| t.0), Some(2));
        assert_eq!(nearest_thread(&per_core, 0, field, 52.0).map(|t| t.0), Some(1));
        assert_eq!(nearest_thread(&per_core, 0, field, 95.0).map(|t| t.0), Some(0));
        assert_eq!(nearest_thread(&per_core, 5, field, 50.0), None, "no sample that far back");
    }
}
