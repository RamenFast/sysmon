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
    let (command_tx, command_rx) = std::sync::mpsc::channel();

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
    harness.get_by_label("Resume updates").click();
    harness.run();

    // ---- units through the menu -------------------------------------
    harness.get_by_label("Display options").click();
    harness.run();
    harness.get_by_label("Binary (GiB)").click();
    harness.run();
    assert!(harness.state().settings.use_binary_units, "binary units applied");
    // Menu items close the popup on click — reopen per interaction.
    harness.get_by_label("Display options").click();
    harness.run();
    harness.get_by_label("Decimal (GB)").click();
    harness.run();
    assert!(!harness.state().settings.use_binary_units, "decimal units restored");

    // ---- compact mode through the menu -------------------------------
    harness.get_by_label("Display options").click();
    harness.run();
    harness.get_by_label("Compact mode").click();
    harness.run();
    assert!(harness.state().settings.compact_mode, "compact on");
    harness.get_by_label("Display options").click();
    harness.run();
    harness.get_by_label("Compact mode").click();
    harness.run();
    assert!(!harness.state().settings.compact_mode, "compact off");
    harness.key_press(egui::Key::Escape);
    harness.run();

    // ---- every palette actually applies (plane fill == token) -------
    for palette in sysmon_app::gui::theme::PALETTES.iter() {
        harness.state_mut().settings.theme_mode = palette.id.to_string();
        // Theme swaps re-layout the scroll area for a few frames —
        // step without demanding quiescence.
        harness.run_steps(4);
        assert_eq!(
            harness.ctx.style().visuals.panel_fill,
            palette.plane,
            "palette `{}` plane not applied",
            palette.id
        );
        assert_eq!(
            harness.ctx.style().visuals.dark_mode,
            palette.dark,
            "palette `{}` dark flag not applied",
            palette.id
        );
    }

    // ---- pop-out toggle from the card header --------------------------
    harness.state_mut().settings.theme_mode = "blossom_dark".to_string();
    harness.run_steps(4);
    let popout_button = harness.get_all_by_label("Pop out into its own window").next();
    popout_button.expect("at least one pop-out toggle").click();
    harness.run_steps(6);
    assert!(
        !harness.state().settings.popped_out_sections.is_empty(),
        "pop-out toggle registered a section"
    );
    let popped = harness.state().settings.popped_out_sections[0].clone();
    harness
        .get_all_by_label("Return to the main window")
        .next()
        .expect("popped card shows the return toggle")
        .click();
    harness.run_steps(6);
    assert!(
        !harness
            .state()
            .settings
            .popped_out_sections
            .contains(&popped),
        "pop-in returned the section"
    );

    // ---- the ctl command queue applies on the main thread ------------
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    command_tx
        .send(sysmon_app::gui::backend::GuiCommand {
            verb: "theme".to_string(),
            value: Some("amber".to_string()),
            path: None,
            reply: reply_tx,
        })
        .expect("queue alive");
    harness.run_steps(6);
    let reply = reply_rx.try_recv().expect("command answered");
    assert!(reply.is_ok(), "theme verb through the queue: {reply:?}");
    assert_eq!(harness.state().settings.theme_mode, "amber");
    assert_eq!(
        harness.state().settings.graph_palette,
        "amber",
        "companion graph palette followed the ctl theme"
    );
}
