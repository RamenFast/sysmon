// SPDX-License-Identifier: GPL-3.0-or-later
//! UI interaction tests through egui_kittest: the real SysMonApp,
//! the real sampler (live /proc — the machine is the fixture),
//! driven through AccessKit exactly the way assistive tech sees it.
//! One test fn, sequential — the app mutates process-global state
//! (env, settings paths), so parallel harnesses would race.

use std::sync::Arc;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use sysmon_app::gui::app::{Page, SharedUi, SysMonApp};
use sysmon_app::gui::settings::Settings;

/// Settle the UI until `label` is actually present, instead of
/// guessing how many frames a popup needs.
///
/// The blind `run_steps(8)` this replaces at the context-menu step
/// was a real flake: the right-click opens a menu that egui builds
/// over a variable number of frames, and roughly one run in six the
/// assertion fired before "Open in process viewer" existed. A test
/// that fails one time in six teaches a maintainer to re-run instead
/// of to look, which is worse than no test.
#[track_caller]
fn settle_until(harness: &mut Harness<'_, SysMonApp>, label: &str) {
    for _ in 0..40 {
        if harness.query_by_label(label).is_some() {
            // present — give it one more frame to finish laying out
            harness.run_steps(1);
            return;
        }
        harness.run_steps(1);
    }
    panic!("`{label}` never appeared after 40 frames");
}

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
    // The test keeps its own handle so it can freeze sampling at the
    // one step that races live data (see the context-menu block).
    let sampler_control = shared.clone();
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
    harness.run_steps(8);
    assert_eq!(harness.state().page, Page::Processes, "page switched");
    assert!(
        harness.query_by_role(Role::TextInput).is_some(),
        "filter box present"
    );

    // ---- filter narrows the census ----------------------------------
    harness.get_by_role(Role::TextInput).click();
    harness.run_steps(8);
    harness.get_by_role(Role::TextInput).type_text("kthreadd");
    for _ in 0..3 {
        harness.run_steps(8);
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

    // ---- sortable headers: whole-cell click, any column --------------
    assert!(
        harness.query_by_label("Sort by CPU % (descending)").is_some(),
        "default sort is CPU descending and the header says so"
    );
    harness.get_by_label("Sort by Memory").click();
    harness.run_steps(8);
    assert!(
        matches!(
            harness.state().table_state.sort_column,
            sysmon_app::gui::processes::SortColumn::Memory
        ),
        "clicking the Memory header sorts by memory"
    );
    assert!(
        harness.state().table_state.sort_descending,
        "metric columns default to biggest-first"
    );
    harness.get_by_label("Sort by Memory (descending)").click();
    harness.run_steps(8);
    assert!(
        !harness.state().table_state.sort_descending,
        "second click flips the direction"
    );
    assert_eq!(
        harness.state().settings.sort_column,
        "memory",
        "sort choice persisted to settings"
    );
    assert!(!harness.state().settings.sort_descending);
    // VRAM too — the ask named it explicitly. At 430px it lives
    // beyond the horizontal scroll edge (the scrollbar Ben asked
    // for), so widen the window to reach it, then restore.
    harness.set_size(egui::vec2(1400.0, 780.0));
    harness.run_steps(3);
    harness.get_by_label("Sort by VRAM").click();
    harness.run_steps(8);
    assert_eq!(harness.state().settings.sort_column, "vram");
    harness.set_size(egui::vec2(430.0, 780.0));
    harness.run_steps(3);

    // ---- multi-select drives the Compare button + combined window ----
    {
        let pids: Vec<i32> = {
            let snapshot = std::process::id() as i32;
            // Use two pids that certainly exist: ourselves and pid 1.
            vec![1, snapshot]
        };
        harness.state_mut().table_state.selected_pids = pids;
    }
    harness.run_steps(2);
    harness.get_by_label("Compare (2)").click();
    harness.run_steps(2);
    assert_eq!(
        harness.state().combined_details().map(<[i32]>::len),
        Some(2),
        "Compare opened the combined-details selection"
    );

    // ---- Escape clears the multi-selection ---------------------------
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.state().table_state.selected_pids.is_empty(),
        "Escape cleared the selection"
    );

    // ---- back to overview; menu switches the theme ------------------
    harness.get_by_label("Overview").click();
    harness.run_steps(8);
    assert_eq!(harness.state().page, Page::Overview);

    // ---- overview right-click → "Open in process viewer" -------------
    //
    // Freeze the sampler first. The overview's top-process rows are
    // ranked live, so between the frame that reads a row's label and
    // the frame that clicks its menu item, a busier process can take
    // the slot — the click then lands on a different pid than the one
    // read, and the test fails about one run in six. That flake is
    // the *test* racing live data, not the app misbehaving, and a
    // test that fails one time in six teaches a maintainer to re-run
    // instead of to look. Pausing is the app's own affordance, so the
    // interaction under test is still the real one.
    sampler_control
        .paused
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // Pausing stops the *next* sample; one may already be in flight.
    // Outwait a full interval so the row order is genuinely frozen
    // before anything is read from it.
    std::thread::sleep(std::time::Duration::from_millis(900));
    harness.run_steps(3);
    {
        let target = harness
            .query_all_by_role(Role::Button)
            .find(|node| {
                node.accesskit_node()
                    .label()
                    .is_some_and(|l| l.contains("— PID "))
            })
            .expect("an overview top-process row");
        let label = target.accesskit_node().label().unwrap().to_string();
        let pid: i32 = label.rsplit("PID ").next().unwrap().trim().parse().unwrap();
        target.click_secondary();
        settle_until(&mut harness, "Open in process viewer");
        harness.get_by_label("Open in process viewer").click();
        harness.run_steps(8);
        assert_eq!(harness.state().page, Page::Processes, "jumped to the table");
        assert_eq!(
            harness.state().table_state.selected_pids,
            vec![pid],
            "the process arrived selected"
        );
    }
    sampler_control
        .paused
        .store(false, std::sync::atomic::Ordering::Relaxed);
    harness.get_by_label("Overview").click();
    harness.run_steps(8);

    // ---- the pin toggles from its button and from `P` -----------------
    assert!(!harness.state().settings.always_on_top);
    harness.get_by_label("Keep window on top (P)").click();
    harness.run_steps(8);
    assert!(
        harness.state().settings.always_on_top,
        "pin button pinned the window"
    );
    harness.key_press(egui::Key::P);
    harness.run_steps(8);
    assert!(
        !harness.state().settings.always_on_top,
        "`P` unpinned it (phosphor's shortcut)"
    );

    harness.get_by_label("Display options").click();
    harness.run_steps(8);
    harness.get_by_label("Funky Pink").click();
    for _ in 0..2 {
        harness.run_steps(8);
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
    harness.run_steps(8);
    harness.get_by_label("Pause updates").click();
    harness.run_steps(8);
    assert!(
        harness
            .query_by_label("Resume updates")
            .is_some(),
        "pause became resume"
    );
    harness.get_by_label("Resume updates").click();
    harness.run_steps(8);

    // ---- units through the menu -------------------------------------
    harness.get_by_label("Display options").click();
    harness.run_steps(8);
    harness.get_by_label("GiB (binary)").click();
    harness.run_steps(8);
    assert!(harness.state().settings.use_binary_units, "binary units applied");
    // Menu items close the popup on click — reopen per interaction.
    harness.get_by_label("Display options").click();
    harness.run_steps(8);
    harness.get_by_label("GB (decimal)").click();
    harness.run_steps(8);
    assert!(!harness.state().settings.use_binary_units, "decimal units restored");

    // ---- temperature scale through the menu ----------------------------
    use sysmon_core::units::TemperatureScale;
    for (label, scale) in [
        ("°F + °C", TemperatureScale::Both),
        ("°F", TemperatureScale::Fahrenheit),
        ("°C", TemperatureScale::Celsius),
    ] {
        harness.get_by_label("Display options").click();
        harness.run_steps(8);
        harness.get_by_label(label).click();
        harness.run_steps(8);
        assert_eq!(harness.state().settings.display().temperature, scale, "{label} chip applies");
    }

    // ---- compact mode through the menu -------------------------------
    harness.get_by_label("Display options").click();
    harness.run_steps(8);
    harness.get_by_label("Compact mode").click();
    harness.run_steps(8);
    assert!(harness.state().settings.compact_mode, "compact on");
    harness.get_by_label("Display options").click();
    harness.run_steps(8);
    harness.get_by_label("Compact mode").click();
    harness.run_steps(8);
    assert!(!harness.state().settings.compact_mode, "compact off");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(8);

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
