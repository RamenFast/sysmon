// SPDX-License-Identifier: GPL-3.0-or-later
//! UI interaction tests through egui_kittest: the real SysMonApp,
//! the real sampler (live /proc — the machine is the fixture),
//! driven through AccessKit exactly the way assistive tech sees it.
//! One test fn, sequential — the app mutates process-global state
//! (env, settings paths), so parallel harnesses would race.

use std::sync::Arc;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use sysmon_app::gui::app::{Page, SharedUi, SysMonApp};
use sysmon_app::gui::settings::Settings;

#[test]
fn ui_interactions_end_to_end() {
    // Never touch the developer's real settings file.
    let scratch = std::env::temp_dir().join(format!("sysmon-kittest-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
    }

    let settings = Settings::default();
    let shared = Arc::new(SharedUi::new(0.5));
    let (_command_tx, command_rx) = std::sync::mpsc::channel();

    let mut harness = Harness::builder()
        .with_size(egui::vec2(430.0, 780.0))
        .build_eframe(|cc| SysMonApp::new(cc, settings, shared, command_rx, None));

    // Let the sampler deliver a first snapshot.
    for _ in 0..12 {
        harness.run();
        std::thread::sleep(std::time::Duration::from_millis(80));
    }

    // ---- overview shows the section cards --------------------------
    assert!(harness.query_by_label("Memory").is_some(), "Memory card title");
    assert!(harness.query_by_label("Disks").is_some(), "Disks card title");

    // ---- switch to the processes page ------------------------------
    harness.get_by_label("Processes").click();
    harness.run();
    assert_eq!(harness.state().page, Page::Processes, "page switched");
    assert!(
        harness.query_by_role(Role::TextInput).is_some(),
        "filter box present"
    );

    // ---- filter narrows the census ----------------------------------
    harness.get_by_role(Role::TextInput).click();
    harness.run();
    harness.get_by_role(Role::TextInput).type_text("kthreadd");
    for _ in 0..3 {
        harness.run();
    }
    assert!(
        harness.query_by_label_contains("1 of").is_some(),
        "count label narrowed to one match"
    );
    assert_eq!(
        harness.state().table_state.filter,
        "kthreadd",
        "typed text landed in the filter"
    );

    // ---- back to overview; menu switches the theme ------------------
    harness.get_by_label("Overview").click();
    harness.run();
    assert_eq!(harness.state().page, Page::Overview);

    harness.get_by_label("Display options").click();
    harness.run();
    harness.get_by_label("Funky Pink").click();
    for _ in 0..2 {
        harness.run();
    }
    assert_eq!(
        harness.state().settings.theme_mode,
        "funky",
        "theme radio applied"
    );
    assert_eq!(
        harness.state().settings.graph_palette,
        "funky",
        "companion graph palette followed the theme"
    );
    assert_eq!(
        harness.ctx.style().visuals.panel_fill,
        egui::Color32::from_rgb(0xff, 0xdd, 0xee),
        "funky plane color actually applied to the style"
    );

    // ---- pause toggle through its glyph button ----------------------
    harness.key_press(egui::Key::Escape); // close the menu
    harness.run();
    harness.get_by_label("Pause updates").click();
    harness.run();
    assert!(
        harness
            .query_by_label("Resume updates")
            .is_some(),
        "pause became resume"
    );
}
