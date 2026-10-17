// SPDX-License-Identifier: GPL-3.0-or-later
//! A hovered process row stays readable (Ben, 3.6: "upon hover over a
//! process the text is impossible to read").
//!
//! The cause: egui_extras paints a hovered table row with the style's
//! `widgets.hovered.bg_fill`, and the theme sets that to the INK so
//! scrollbar handles hold text-grade contrast. A hovered row was an
//! ink slab under ink text. The fix scopes the room's row tokens to
//! the table body; this test renders the real table, puts the pointer
//! on a row and reads the pixels back: the row's ground must be the
//! soft hover tint, never the ink, in a dark room and a light one.
//!
//! Isolation law: scratch XDG_CONFIG_HOME + XDG_RUNTIME_DIR +
//! XDG_STATE_HOME; the sampler is paused and the test owns the rows.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use egui::Color32;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use sysmon_app::gui::app::{SharedUi, SysMonApp};
use sysmon_app::gui::settings::Settings;
use sysmon_app::gui::theme::palette_by_id;
use sysmon_core::snapshot::{ProcessRecord, SystemSnapshot};

fn snapshot() -> SystemSnapshot {
    let row = |pid: i32, name: &str| ProcessRecord {
        pid,
        name: name.into(),
        display_name: name.into(),
        user: "ben".into(),
        threads: 1,
        ..Default::default()
    };
    SystemSnapshot {
        ts: 1.0,
        interval_seconds: 1.0,
        processes: Some(vec![row(1, "init"), row(2, "kiln"), row(3, "ember"), row(4, "loam")]),
        ..Default::default()
    }
}

/// `over` painted onto `under` the way egui blends a premultiplied
/// colour: out = over + under × (1 − alpha).
fn composite(over: Color32, under: Color32) -> [u8; 3] {
    let keep = 1.0 - over.a() as f32 / 255.0;
    let channel = |o: u8, u: u8| (o as f32 + u as f32 * keep).round().min(255.0) as u8;
    [channel(over.r(), under.r()), channel(over.g(), under.g()), channel(over.b(), under.b())]
}

fn distance(a: [u8; 3], b: [u8; 3]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - y as i32).abs()).max().unwrap_or(0)
}

/// The rendered colour under `at` (logical points; the harness renders
/// at one pixel per point).
fn pixel(harness: &mut Harness<'_, SysMonApp>, at: egui::Pos2) -> [u8; 3] {
    let image = harness.render().expect("the harness renders (wgpu)");
    let p = image.get_pixel(at.x.round() as u32, at.y.round() as u32);
    [p[0], p[1], p[2]]
}

#[test]
fn a_hovered_row_wears_the_soft_tint_and_never_the_ink() {
    let scratch = std::env::temp_dir().join(format!("sysmon-row-hover-{}", std::process::id()));
    let config = scratch.join("config");
    let runtime = scratch.join("run");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_STATE_HOME", config.join("state"));
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    }

    let settings = Settings {
        theme_mode: "amoled".to_string(),
        start_page: "processes".to_string(),
        // The real default: the Inspector docked beside the table (its
        // layout clips the table at the cell's left edge, which hid a
        // hover edge painted just outside it).
        show_inspector: true,
        sort_column: "pid".to_string(),
        sort_descending: false,
        ..Settings::default()
    };
    let shared = Arc::new(SharedUi::new(0.5));
    let control = shared.clone();
    let (_tx, rx) = std::sync::mpsc::channel();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 420.0))
        .build_eframe(move |cc| SysMonApp::new(cc, settings, shared, rx, None));
    control.paused.store(true, Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(900));
    *control.latest.write().unwrap() = Arc::new(snapshot());
    harness.run_steps(6);

    for room in ["amoled", "light"] {
        harness.state_mut().settings.theme_mode = room.to_string();
        harness.run_steps(4);
        let palette = palette_by_id(room).unwrap();

        // A spot inside the "kiln" row's name cell, past its text.
        let name = harness
            .query_all_by_role(Role::Label)
            .find(|n| n.accesskit_node().value().as_deref() == Some("kiln"))
            .map(|n| n.rect())
            .expect("the kiln row is on screen");
        let spot = egui::pos2(name.right() + 40.0, name.center().y);

        // Pointer elsewhere: the row shows its resting ground.
        harness.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(880.0, 410.0)));
        harness.run_steps(3);
        let resting = pixel(&mut harness, spot);

        // Pointer on the row: egui_extras learns the hovered row on
        // one frame and paints it on the next.
        harness.input_mut().events.push(egui::Event::PointerMoved(spot));
        harness.run_steps(3);
        let hovered = pixel(&mut harness, spot);

        let ink = [palette.ink.r(), palette.ink.g(), palette.ink.b()];
        let tinted: Vec<[u8; 3]> =
            [palette.plane, palette.surface_2].iter().map(|ground| composite(palette.row_hover(), *ground)).collect();
        assert!(
            distance(hovered, ink) > 60,
            "{room}: the hovered row is painted the ink colour {hovered:?} (its text would vanish)"
        );
        assert!(
            tinted.iter().any(|expected| distance(hovered, *expected) <= 8),
            "{room}: the hovered row is {hovered:?}, expected the soft tint (one of {tinted:?})"
        );
        assert_ne!(hovered, resting, "{room}: hover must still be visible against the resting row");

        // The thin accent edge on the left of the hovered row (and none at
        // rest): it is what sets a hovered row apart from a striped one.
        let image = harness.render().expect("the harness renders (wgpu)");
        let accent = [palette.accent.r(), palette.accent.g(), palette.accent.b()];
        let edge_found = (0..name.left().round() as u32)
            .any(|x| distance({ let p = image.get_pixel(x, spot.y.round() as u32); [p[0], p[1], p[2]] }, accent) <= 12);
        assert!(edge_found, "{room}: no accent edge left of the hovered row");

        // A live re-sort moves the process away from the pointer; the edge
        // must not vanish with it (it settles on the row under the
        // pointer, where egui_extras' tint is, within a frame).
        let mut moved = snapshot();
        if let Some(rows) = moved.processes.as_mut() {
            rows.iter_mut().filter(|r| r.name == "kiln").for_each(|r| r.pid = 9);
        }
        *control.latest.write().unwrap() = Arc::new(moved);
        harness.run_steps(4);
        let image = harness.render().expect("the harness renders (wgpu)");
        let edge_under_pointer = (0..name.left().round() as u32)
            .any(|x| distance({ let p = image.get_pixel(x, spot.y.round() as u32); [p[0], p[1], p[2]] }, accent) <= 12);
        assert!(edge_under_pointer, "{room}: after a re-sort the edge left the row under the pointer");
        *control.latest.write().unwrap() = Arc::new(snapshot());
        harness.run_steps(4);
        harness.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(880.0, 410.0)));
        harness.run_steps(3);
        let image = harness.render().expect("the harness renders (wgpu)");
        let edge_at_rest = (0..name.left().round() as u32)
            .any(|x| distance({ let p = image.get_pixel(x, spot.y.round() as u32); [p[0], p[1], p[2]] }, accent) <= 12);
        assert!(!edge_at_rest, "{room}: the accent edge stays after the pointer left");
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
