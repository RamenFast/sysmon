// SPDX-License-Identifier: GPL-3.0-or-later
//! The right-click menu acts on the process that was right-clicked,
//! even when the live ranking moves underneath the open menu.
//!
//! Ben, 2026-09-30: "when on the main page and I right click select a
//! subprocess, please ensure that when I'm taking actions in that
//! submenu that it remembers the correct selected entry".
//!
//! The overview's top-3 rows re-rank on every sample. A menu opened on
//! rank 1 must keep naming, and acting on, the pid it was opened on;
//! before 3.1 it followed the *slot*, so a busier process taking rank 1
//! between the right-click and the click inherited the action (End
//! process included).
//!
//! The machine is not the fixture here: the sampler is paused and the
//! snapshot is written by the test, so the re-rank is deterministic.
//! Isolation law: scratch XDG_CONFIG_HOME *and* XDG_RUNTIME_DIR.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use sysmon_app::gui::app::{Page, SharedUi, SysMonApp};
use sysmon_app::gui::settings::Settings;
use sysmon_core::snapshot::{CpuSnapshot, MemorySnapshot, ProcessRecord, SystemSnapshot};

const FIRST: i32 = 900_001;
const SECOND: i32 = 900_002;
const THIRD: i32 = 900_003;

fn record(pid: i32, name: &str, cpu: f32, rss_mb: u64) -> ProcessRecord {
    ProcessRecord {
        pid,
        ppid: 1,
        name: name.to_string(),
        user: "ben".to_string(),
        state: "S".to_string(),
        state_word: "sleeping".to_string(),
        cpu_percent: cpu,
        memory_rss_bytes: rss_mb * 1_000_000,
        threads: 1,
        command_line: format!("/usr/bin/{name}"),
        ..Default::default()
    }
}

/// `leader` is busiest by both CPU and memory, so it holds rank 1 in
/// every top-3 list on the overview.
fn snapshot_led_by(leader: i32, ts: f64) -> SystemSnapshot {
    let busy = |pid: i32| if pid == leader { 90.0 } else { 20.0 };
    let big = |pid: i32| if pid == leader { 900 } else { 300 };
    SystemSnapshot {
        ts,
        interval_seconds: 1.0,
        // Cards draw their top-3 rows only when their own section exists.
        memory: Some(MemorySnapshot {
            total_bytes: 64_000_000_000,
            used_bytes: 8_000_000_000,
            available_bytes: 56_000_000_000,
            used_percent: 12.5,
            ..Default::default()
        }),
        cpu: Some(CpuSnapshot {
            overall_percent: 10.0,
            per_core_percent: vec![10.0; 4],
            core_count: 4,
            ..Default::default()
        }),
        processes: Some(vec![
            record(FIRST, "alpha", busy(FIRST), big(FIRST)),
            record(SECOND, "bravo", busy(SECOND), big(SECOND)),
            record(THIRD, "charlie", 5.0, 100),
        ]),
        ..Default::default()
    }
}

#[track_caller]
fn settle_until(harness: &mut Harness<'_, SysMonApp>, label: &str) {
    for _ in 0..40 {
        if harness.query_by_label(label).is_some() {
            harness.run_steps(1);
            return;
        }
        harness.run_steps(1);
    }
    panic!("`{label}` never appeared after 40 frames");
}

fn rank_one_row_pid(harness: &Harness<'_, SysMonApp>) -> i32 {
    let row = harness
        .query_all_by_role(Role::Button)
        .find(|node| {
            node.accesskit_node()
                .label()
                .is_some_and(|label| label.contains("— PID "))
        })
        .expect("an overview top-process row");
    let label = row.accesskit_node().label().unwrap().to_string();
    label.rsplit("PID ").next().unwrap().trim().parse().unwrap()
}

#[test]
fn context_menu_keeps_the_process_it_was_opened_on() {
    let scratch = std::env::temp_dir().join(format!("sysmon-menu-target-{}", std::process::id()));
    let config = scratch.join("config");
    let runtime = scratch.join("run");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    }

    let shared = Arc::new(SharedUi::new(0.5));
    let control = shared.clone();
    let (_command_tx, command_rx) = std::sync::mpsc::channel();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(600.0, 900.0))
        .build_eframe(|cc| SysMonApp::new(cc, Settings::default(), shared, command_rx, None));

    // Freeze the sampler, outwait an in-flight sample, then own the data.
    control.paused.store(true, Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(900));
    *control.latest.write().unwrap() = Arc::new(snapshot_led_by(FIRST, 1.0));
    harness.run_steps(6);
    assert_eq!(rank_one_row_pid(&harness), FIRST, "alpha leads before the click");

    // Right-click rank 1 (alpha) and let the menu open.
    harness
        .query_all_by_role(Role::Button)
        .find(|node| {
            node.accesskit_node()
                .label()
                .is_some_and(|label| label.contains(&format!("PID {FIRST}")))
        })
        .expect("alpha's row")
        .click_secondary();
    settle_until(&mut harness, "Open in process viewer");

    // The ranking moves while the menu is open: bravo takes rank 1.
    *control.latest.write().unwrap() = Arc::new(snapshot_led_by(SECOND, 2.0));
    harness.run_steps(6);
    assert_eq!(rank_one_row_pid(&harness), SECOND, "bravo now leads the live list");

    // The open menu still names alpha...
    assert!(
        harness
            .query_by_label_contains(&format!("(PID {FIRST})"))
            .is_some(),
        "the open menu must keep naming the process it was opened on"
    );

    // ...and its actions land on alpha, not on whoever holds the slot.
    harness.get_by_label("Open in process viewer").click();
    harness.run_steps(8);
    assert_eq!(harness.state().page, Page::Processes, "jumped to the table");
    assert_eq!(
        harness.state().table_state.selected_pids,
        vec![FIRST],
        "the menu's action went to the process it was opened on"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
