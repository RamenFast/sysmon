// SPDX-License-Identifier: GPL-3.0-or-later
//! Hand-painted glyphs — SysMon's own little icon set. Painted with
//! egui strokes (no icon font: Hack has gaps and the proportional font
//! tofus most symbols), sharp-cornered, 1.2–1.5 px hairlines, drawn in
//! whatever ink the caller passes so every theme (greyscale included)
//! colours them on its own terms.
//!
//! Each glyph fits a square of side `size` centred on `center`; the
//! artwork is laid out on a 16-unit grid and scaled.

use std::f32::consts::{PI, TAU};

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Ui, Vec2, vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    // card headers
    Gpu,
    Memory,
    Cpu,
    Network,
    Disk,
    Sensors,
    // sensor rows
    Thermometer,
    Fan,
    Bolt,
    Battery,
    Drive,
    Board,
    // process inspector
    Tree,
    Clock,
    Plug,
    Folder,
    Layers,
    Gauge,
    // process states
    Running,
    Sleeping,
    DiskWait,
    Zombie,
    Stopped,
    // chrome
    Chevron,
    ChevronDown,
    ArrowRight,
    Group,
    Close,
    Sparkle,
}

/// Paint `glyph` in `color`, filling a `size`-sided square at `center`.
pub fn paint(painter: &Painter, glyph: Glyph, center: Pos2, size: f32, color: Color32) {
    let unit = size / 16.0;
    // 16-unit grid → screen, origin at the square's top-left.
    let origin = center - vec2(size / 2.0, size / 2.0);
    let p = |x: f32, y: f32| origin + vec2(x * unit, y * unit);
    let line = Stroke::new((unit * 1.25).clamp(1.0, 1.6), color);
    let thin = Stroke::new((unit * 0.9).clamp(0.8, 1.2), color);
    let seg = |a: Pos2, b: Pos2, stroke: Stroke| painter.line_segment([a, b], stroke);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32, stroke: Stroke| {
        painter.rect_stroke(
            Rect::from_min_max(p(x0, y0), p(x1, y1)),
            0.0,
            stroke,
            egui::StrokeKind::Middle,
        );
    };
    let fill = |x0: f32, y0: f32, x1: f32, y1: f32, color: Color32| {
        painter.rect_filled(Rect::from_min_max(p(x0, y0), p(x1, y1)), 0.0, color);
    };
    let dot = |x: f32, y: f32, r: f32| painter.circle_filled(p(x, y), r * unit, color);
    let ring = |x: f32, y: f32, r: f32, stroke: Stroke| {
        painter.circle_stroke(p(x, y), r * unit, stroke);
    };
    let arc = |x: f32, y: f32, r: f32, from: f32, to: f32, stroke: Stroke| {
        let steps = 14;
        let points: Vec<Pos2> = (0..=steps)
            .map(|i| {
                let angle = from + (to - from) * i as f32 / steps as f32;
                p(x + r * angle.cos(), y + r * angle.sin())
            })
            .collect();
        painter.add(Shape::line(points, stroke));
    };
    let poly = |points: &[(f32, f32)], stroke: Stroke| {
        painter.add(Shape::line(points.iter().map(|&(x, y)| p(x, y)).collect(), stroke));
    };
    let soft = color.gamma_multiply(0.35);

    match glyph {
        // A card with a fan hub: the GPU.
        Glyph::Gpu => {
            rect(1.5, 4.0, 14.5, 12.0, line);
            ring(10.0, 8.0, 2.4, thin);
            dot(10.0, 8.0, 0.7);
            for x in [3.5, 5.5] {
                seg(p(x, 6.0), p(x, 10.0), thin);
            }
            seg(p(3.0, 12.0), p(3.0, 14.0), line); // bracket
        }
        // A DIMM: chips on a stick, notch and contacts below.
        Glyph::Memory => {
            rect(1.0, 4.5, 15.0, 11.0, line);
            for x in [3.0, 6.0, 9.0, 12.0] {
                fill(x, 6.3, x + 1.8, 9.2, soft);
            }
            for x in (0..7).map(|i| 2.5 + i as f32 * 1.8) {
                if (x - 8.0).abs() > 0.8 {
                    seg(p(x, 11.0), p(x, 12.6), thin);
                }
            }
        }
        // A die with pins on every side.
        Glyph::Cpu => {
            rect(4.0, 4.0, 12.0, 12.0, line);
            fill(6.3, 6.3, 9.7, 9.7, soft);
            for t in [5.8, 8.0, 10.2] {
                seg(p(t, 1.5), p(t, 4.0), thin);
                seg(p(t, 12.0), p(t, 14.5), thin);
                seg(p(1.5, t), p(4.0, t), thin);
                seg(p(12.0, t), p(14.5, t), thin);
            }
        }
        // Down and up arrows, side by side.
        Glyph::Network => {
            seg(p(5.0, 2.0), p(5.0, 13.0), line);
            poly(&[(2.2, 10.0), (5.0, 13.5), (7.8, 10.0)], line);
            seg(p(11.0, 14.0), p(11.0, 3.0), line);
            poly(&[(8.2, 6.0), (11.0, 2.5), (13.8, 6.0)], line);
        }
        // A stack of platters (cylinder).
        Glyph::Disk => {
            // A cylinder: top ellipse, two sides, and half-ellipse rims
            // (the band and the base) — rx 6, ry 2, so it stays inside
            // the 16-unit square.
            let half_ellipse = |cy: f32, stroke: Stroke| {
                let points: Vec<Pos2> = (0..=14)
                    .map(|i| {
                        let a = PI * i as f32 / 14.0;
                        p(8.0 + 6.0 * a.cos(), cy + 2.0 * a.sin())
                    })
                    .collect();
                painter.add(Shape::line(points, stroke));
            };
            painter.add(Shape::ellipse_stroke(p(8.0, 4.0), vec2(6.0, 2.0) * unit, line));
            seg(p(2.0, 4.0), p(2.0, 12.0), line);
            seg(p(14.0, 4.0), p(14.0, 12.0), line);
            half_ellipse(12.0, line);
            half_ellipse(8.0, thin);
        }
        // A thermometer beside a wave: "the room's vital signs".
        Glyph::Sensors => {
            paint_thermometer(painter, p(5.0, 8.0), size * 0.9, color);
            poly(&[(9.0, 10.0), (10.5, 6.0), (12.0, 11.0), (13.5, 5.0), (15.0, 8.0)], thin);
        }
        Glyph::Thermometer => paint_thermometer(painter, center, size, color),
        // Three swept blades round a hub.
        Glyph::Fan => {
            ring(8.0, 8.0, 6.8, thin);
            for k in 0..3 {
                let a = k as f32 * TAU / 3.0 - PI / 2.0;
                arc(
                    8.0 + 2.6 * a.cos(),
                    8.0 + 2.6 * a.sin(),
                    2.6,
                    a + 0.2,
                    a + 2.4,
                    line,
                );
            }
            dot(8.0, 8.0, 1.2);
        }
        Glyph::Bolt => {
            painter.add(Shape::convex_polygon(
                vec![p(9.5, 1.5), p(4.0, 9.0), p(7.6, 9.0), p(6.5, 14.5), p(12.0, 7.0), p(8.4, 7.0)],
                Color32::TRANSPARENT,
                line,
            ));
        }
        Glyph::Battery => {
            rect(1.5, 5.0, 13.0, 11.0, line);
            fill(13.0, 7.0, 14.5, 9.0, color);
            fill(3.0, 6.5, 9.0, 9.5, soft);
        }
        // A 2.5" drive: case, platter, actuator arm.
        Glyph::Drive => {
            rect(2.5, 1.5, 13.5, 14.5, line);
            ring(8.0, 7.0, 3.6, thin);
            dot(8.0, 7.0, 0.8);
            seg(p(11.5, 12.5), p(9.0, 8.5), line);
            dot(4.5, 12.8, 0.6);
        }
        // A motherboard: traces to a socket.
        Glyph::Board => {
            rect(1.5, 1.5, 14.5, 14.5, line);
            rect(8.5, 3.5, 12.5, 7.5, thin);
            poly(&[(3.5, 4.0), (6.0, 4.0), (6.0, 5.5), (8.5, 5.5)], thin);
            poly(&[(3.5, 12.5), (10.5, 12.5), (10.5, 7.5)], thin);
            for y in [9.5, 11.0] {
                seg(p(3.5, y), p(7.5, y), thin);
            }
        }
        // A parent with two children.
        Glyph::Tree => {
            rect(5.5, 1.5, 10.5, 5.0, line);
            seg(p(8.0, 5.0), p(8.0, 8.0), thin);
            seg(p(4.0, 8.0), p(12.0, 8.0), thin);
            seg(p(4.0, 8.0), p(4.0, 10.5), thin);
            seg(p(12.0, 8.0), p(12.0, 10.5), thin);
            rect(1.5, 10.5, 6.5, 14.5, line);
            rect(9.5, 10.5, 14.5, 14.5, line);
        }
        Glyph::Clock => {
            ring(8.0, 8.0, 6.5, line);
            seg(p(8.0, 8.0), p(8.0, 4.0), line);
            seg(p(8.0, 8.0), p(11.0, 9.5), line);
        }
        // A two-prong plug: sockets.
        Glyph::Plug => {
            seg(p(6.0, 1.5), p(6.0, 5.0), line);
            seg(p(10.0, 1.5), p(10.0, 5.0), line);
            poly(&[(3.5, 5.0), (12.5, 5.0), (12.5, 8.5), (10.0, 11.0), (6.0, 11.0), (3.5, 8.5), (3.5, 5.0)], line);
            seg(p(8.0, 11.0), p(8.0, 14.5), line);
        }
        Glyph::Folder => {
            poly(&[(1.5, 13.5), (1.5, 3.0), (6.0, 3.0), (7.5, 5.0), (14.5, 5.0), (14.5, 13.5), (1.5, 13.5)], line);
            seg(p(1.5, 7.0), p(14.5, 7.0), thin);
        }
        // Stacked sheets: memory maps.
        Glyph::Layers => {
            for (i, y) in [3.0, 6.5, 10.0].iter().enumerate() {
                let stroke = if i == 0 { line } else { thin };
                poly(&[(8.0, *y), (14.5, y + 2.2), (8.0, y + 4.4), (1.5, y + 2.2), (8.0, *y)], stroke);
            }
        }
        Glyph::Gauge => {
            arc(8.0, 10.0, 6.5, PI, TAU, line);
            seg(p(8.0, 10.0), p(11.5, 5.5), line);
            dot(8.0, 10.0, 1.1);
        }
        // States: a play wedge, a crescent moon, an hourglass, a little
        // ghost, a pause.
        Glyph::Running => {
            painter.add(Shape::convex_polygon(
                vec![p(5.0, 3.0), p(13.0, 8.0), p(5.0, 13.0)],
                color,
                Stroke::NONE,
            ));
        }
        Glyph::Sleeping => {
            arc(8.0, 8.0, 5.5, 0.35 * PI, 1.65 * PI, line);
            arc(10.0, 7.0, 4.2, 0.55 * PI, 1.45 * PI, thin);
        }
        Glyph::DiskWait => {
            poly(&[(4.0, 2.0), (12.0, 2.0), (4.0, 14.0), (12.0, 14.0), (4.0, 2.0)], line);
            fill(6.5, 11.5, 9.5, 13.0, soft);
        }
        Glyph::Zombie => {
            arc(8.0, 7.0, 5.0, PI, TAU, line);
            poly(&[(3.0, 7.0), (3.0, 14.0), (5.0, 12.5), (6.8, 14.0), (8.6, 12.5), (10.4, 14.0), (13.0, 12.5), (13.0, 7.0)], line);
            dot(6.2, 7.0, 0.9);
            dot(9.8, 7.0, 0.9);
        }
        Glyph::Stopped => {
            fill(4.5, 3.5, 6.8, 12.5, color);
            fill(9.2, 3.5, 11.5, 12.5, color);
        }
        Glyph::Chevron => poly(&[(6.0, 3.5), (10.5, 8.0), (6.0, 12.5)], line),
        Glyph::ChevronDown => poly(&[(3.5, 6.0), (8.0, 10.5), (12.5, 6.0)], line),
        Glyph::ArrowRight => {
            seg(p(2.5, 8.0), p(13.0, 8.0), line);
            poly(&[(9.0, 4.0), (13.5, 8.0), (9.0, 12.0)], line);
        }
        // Three stacked rows with a bracket: group by app.
        Glyph::Group => {
            for y in [3.5, 8.0, 12.5] {
                seg(p(6.0, y), p(14.5, y), line);
            }
            poly(&[(4.0, 2.5), (2.0, 2.5), (2.0, 13.5), (4.0, 13.5)], thin);
        }
        Glyph::Close => {
            seg(p(3.5, 3.5), p(12.5, 12.5), line);
            seg(p(12.5, 3.5), p(3.5, 12.5), line);
        }
        // A four-point twinkle — the "nothing wrong here" mark.
        Glyph::Sparkle => {
            painter.add(Shape::convex_polygon(
                vec![p(8.0, 1.5), p(9.4, 6.6), p(14.5, 8.0), p(9.4, 9.4), p(8.0, 14.5), p(6.6, 9.4), p(1.5, 8.0), p(6.6, 6.6)],
                color,
                Stroke::NONE,
            ));
        }
    }
}

fn paint_thermometer(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let unit = size / 16.0;
    let origin = center - vec2(size / 2.0, size / 2.0);
    let p = |x: f32, y: f32| origin + vec2(x * unit, y * unit);
    let line = Stroke::new((unit * 1.2).clamp(1.0, 1.5), color);
    // Stem as two sides and a cap, bulb as a ring with a filled core.
    painter.line_segment([p(6.6, 2.6), p(6.6, 10.2)], line);
    painter.line_segment([p(9.4, 2.6), p(9.4, 10.2)], line);
    let cap: Vec<Pos2> = (0..=8)
        .map(|i| {
            let a = PI + PI * i as f32 / 8.0;
            p(8.0 + 1.4 * a.cos(), 2.6 + 1.4 * a.sin())
        })
        .collect();
    painter.add(Shape::line(cap, line));
    painter.circle_stroke(p(8.0, 12.2), 2.6 * unit, line);
    painter.circle_filled(p(8.0, 12.2), 1.4 * unit, color);
    painter.line_segment([p(8.0, 6.0), p(8.0, 11.0)], Stroke::new(line.width * 1.1, color));
}

/// Allocate a `size`-square, paint the glyph, return the response
/// (hover → tooltip by the caller).
pub fn show(ui: &mut Ui, glyph: Glyph, size: f32, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), glyph, rect.center(), size, color);
    }
    response
}

/// The glyph a kernel process state wears.
pub fn for_process_state(state: &str) -> Glyph {
    match state.chars().next().unwrap_or('?') {
        'R' => Glyph::Running,
        'D' => Glyph::DiskWait,
        'Z' => Glyph::Zombie,
        'T' | 't' => Glyph::Stopped,
        _ => Glyph::Sleeping,
    }
}

/// The glyph for a card's section key.
pub fn for_section(section: &str) -> Glyph {
    match section {
        "gpu" => Glyph::Gpu,
        "memory" => Glyph::Memory,
        "cpu" => Glyph::Cpu,
        "network" => Glyph::Network,
        "disks" => Glyph::Disk,
        _ => Glyph::Sensors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    /// Every glyph paints something, inside its square (one stroke of
    /// slack), without panicking — at the small sizes it really ships.
    #[test]
    fn every_glyph_paints_inside_its_square() {
        let all = [
            Glyph::Gpu, Glyph::Memory, Glyph::Cpu, Glyph::Network, Glyph::Disk,
            Glyph::Sensors, Glyph::Thermometer, Glyph::Fan, Glyph::Bolt, Glyph::Battery,
            Glyph::Drive, Glyph::Board, Glyph::Tree, Glyph::Clock, Glyph::Plug,
            Glyph::Folder, Glyph::Layers, Glyph::Gauge, Glyph::Running, Glyph::Sleeping,
            Glyph::DiskWait, Glyph::Zombie, Glyph::Stopped, Glyph::Chevron, Glyph::ChevronDown,
            Glyph::ArrowRight, Glyph::Group, Glyph::Close, Glyph::Sparkle,
        ];
        let ctx = egui::Context::default();
        for size in [10.0f32, 14.0, 24.0] {
            for glyph in all {
                let center = pos2(50.0, 50.0);
                let output = ctx.run(egui::RawInput::default(), |ctx| {
                    // Straight to a layer painter: a fresh Area's
                    // first pass is an invisible sizing pass, which
                    // would hide whichever glyph happens to run first.
                    let painter = ctx.layer_painter(egui::LayerId::background());
                    paint(&painter, glyph, center, size, Color32::WHITE);
                });
                let bounds = Rect::from_center_size(center, Vec2::splat(size)).expand(2.0);
                let shapes: Vec<_> = output
                    .shapes
                    .iter()
                    .filter(|clipped| clipped.shape.visual_bounding_rect().is_positive())
                    .collect();
                assert!(!shapes.is_empty(), "{glyph:?} painted nothing at {size}");
                for clipped in shapes {
                    let rect = clipped.shape.visual_bounding_rect();
                    assert!(bounds.contains_rect(rect), "{glyph:?} at {size} spills: {rect:?}");
                }
            }
        }
    }

    #[test]
    fn states_and_sections_map() {
        assert_eq!(for_process_state("R"), Glyph::Running);
        assert_eq!(for_process_state("D"), Glyph::DiskWait);
        assert_eq!(for_process_state("Z"), Glyph::Zombie);
        assert_eq!(for_process_state("S"), Glyph::Sleeping);
        assert_eq!(for_section("memory"), Glyph::Memory);
    }
}
