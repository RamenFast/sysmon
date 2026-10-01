// SPDX-License-Identifier: GPL-3.0-or-later
//! The Overview ↔ Processes bridge, end to end through AccessKit.
//!
//! Ben, 2026-09-30: "allow for deeper inspection of processes, and
//! nice UI/UX integration between overview and processes".
//!
//! Failure modes this walks (docs/dev/FAILURE-MODES.md):
//!   P5  a top-3 row click inspects the slot's pid after a re-rank
//!   P6  "all by memory" lands sorted by something else / ascending
//!   P7  a summary-strip chip lands on the Overview, not its card
//!   P8  group-by-app's ×N disagrees with the rows it summed
//!   P12 group-by / Inspector toggles don't survive a restart
//!
//! Like ui_menu_target, the sampler is paused and the test owns the
//! snapshot, so ranks are deterministic. Isolation law: scratch
//! XDG_CONFIG_HOME *and* XDG_RUNTIME_DIR. The run ends by writing a
//! receipt (target/receipts/ui_bridge.json) — the repeatable artifact.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use sysmon_app::gui::app::{Page, SharedUi, SysMonApp};
use sysmon_app::gui::processes::SortColumn;
use sysmon_app::gui::settings::Settings;
use sysmon_core::snapshot::{CpuSnapshot, MemorySnapshot, ProcessRecord, SystemSnapshot};

const ROOT: i32 = 910_001;
const HEAVY: i32 = 910_002;
const WORKER_A: i32 = 910_003;
const WORKER_B: i32 = 910_004;
const LONER: i32 = 910_005;

fn record(pid: i32, ppid: i32, exe: &str, cpu: f32, rss_mb: u64, started: f64) -> ProcessRecord {
    ProcessRecord {
        pid,
        ppid,
        name: exe.to_string(),
        display_name: exe.to_string(),
        exe_basename: Some(exe.to_string()),
        user: "ben".to_string(),
        state: "S".to_string(),
        state_word: "sleeping".to_string(),
        cpu_percent: cpu,
        memory_rss_bytes: rss_mb * 1_000_000,
        threads: 2,
        started_ts: started,
        command_line: format!("/usr/bin/{exe} --fixture"),
        ..Default::default()
    }
}

/// "orchard" is a 3-process app (root + two workers); "kiln" is busy;
/// "loner" is a single process. `cpu_leader` holds CPU rank 1.
fn snapshot(cpu_leader: i32, ts: f64) -> SystemSnapshot {
    let cpu = |pid: i32| if pid == cpu_leader { 80.0 } else { 10.0 };
    SystemSnapshot {
        ts,
        interval_seconds: 1.0,
        memory: Some(MemorySnapshot {
            total_bytes: 64_000_000_000,
            used_bytes: 8_000_000_000,
            available_bytes: 56_000_000_000,
            used_percent: 12.5,
            ..Default::default()
        }),
        cpu: Some(CpuSnapshot {
            overall_percent: 20.0,
            per_core_percent: vec![20.0; 4],
            core_count: 4,
            ..Default::default()
        }),
        processes: Some(vec![
            record(ROOT, 1, "orchard", cpu(ROOT), 400, 100.0),
            record(HEAVY, 1, "kiln", cpu(HEAVY), 900, 200.0),
            record(WORKER_A, ROOT, "orchard", cpu(WORKER_A), 300, 150.0),
            record(WORKER_B, ROOT, "orchard", cpu(WORKER_B), 200, 160.0),
            record(LONER, 1, "loner", cpu(LONER), 50, 300.0),
        ]),
        ..Default::default()
    }
}

fn row_for(harness: &Harness<'_, SysMonApp>, pid: i32) -> bool {
    harness
        .query_all_by_role(Role::Button)
        .any(|node| node.accesskit_node().label().is_some_and(|l| l.ends_with(&format!("PID {pid}"))))
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

#[test]
fn overview_and_processes_flow_both_ways() {
    let scratch = std::env::temp_dir().join(format!("sysmon-bridge-{}", std::process::id()));
    let config = scratch.join("config");
    let runtime = scratch.join("run");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    }
    let mut receipt = serde_json::Map::new();

    let shared = Arc::new(SharedUi::new(0.5));
    let control = shared.clone();
    let (_command_tx, command_rx) = std::sync::mpsc::channel();
    // Wide enough that the Inspector docks beside the table.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1200.0, 900.0))
        .build_eframe(|cc| SysMonApp::new(cc, Settings::default(), shared, command_rx, None));
    control.paused.store(true, Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(900));
    *control.latest.write().unwrap() = Arc::new(snapshot(ROOT, 1.0));
    harness.run_steps(6);

    // ---- P5: clicking the CPU rank-1 row inspects *that* pid, even
    // when the ranking moves before the app applies the click.
    let row = harness
        .query_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().is_some_and(|l| l.ends_with(&format!("PID {ROOT}"))))
        .expect("orchard's overview row");
    row.click();
    *control.latest.write().unwrap() = Arc::new(snapshot(HEAVY, 2.0));
    harness.run_steps(8);
    assert_eq!(harness.state().page, Page::Processes, "a row click opens Processes");
    assert_eq!(harness.state().table_state.inspector.pid, Some(ROOT), "P5: Inspector holds the clicked pid");
    assert_eq!(harness.state().table_state.selected_pids, vec![ROOT], "the table row is selected too");
    // The Inspector shows the parent chain and the children.
    settle_until(&mut harness, "Children (2)");
    receipt.insert("P5_inspect_clicked_pid".into(), true.into());

    // Drill down: a child row retargets the Inspector...
    harness.get_by_label(&format!("Inspect orchard — PID {WORKER_A}")).click();
    harness.run_steps(8);
    assert_eq!(harness.state().table_state.inspector.pid, Some(WORKER_A), "child row drills down");
    // ...and the breadcrumb climbs back to the parent.
    harness.get_by_label(&format!("Breadcrumb orchard — PID {ROOT}")).click();
    harness.run_steps(8);
    assert_eq!(harness.state().table_state.inspector.pid, Some(ROOT), "breadcrumb climbs back");
    receipt.insert("drill_down_and_back".into(), true.into());

    // ---- P7: a summary chip goes back to the Overview, to its card.
    harness.get_by_label("Overview memory").click();
    harness.run_steps(8);
    assert_eq!(harness.state().page, Page::Overview, "P7: chip returns to the Overview");
    receipt.insert("P7_chip_returns".into(), true.into());

    // ---- P6: "all by memory" lands sorted by memory, biggest first.
    harness.get_by_label("Processes by memory").click();
    harness.run_steps(8);
    assert_eq!(harness.state().page, Page::Processes);
    assert_eq!(harness.state().table_state.sort_column, SortColumn::Memory, "P6: sorted by memory");
    assert!(harness.state().table_state.sort_descending, "P6: biggest first");
    receipt.insert("P6_more_link_sort".into(), true.into());

    // ---- P8: group by app — orchard ×3 sums its three processes.
    harness.get_by_label("Group by app").click();
    harness.run_steps(8);
    assert!(harness.state().table_state.group_by_app, "grouping on");
    settle_until(&mut harness, "orchard ×3");
    assert!(harness.query_by_label("kiln").is_some(), "single-process apps keep their plain name");
    assert!(harness.query_by_label("orchard ×2").is_none(), "P8: the count covers every orchard process");
    receipt.insert("P8_group_count".into(), "orchard ×3".into());

    // ---- P12: toggles persist.
    harness.get_by_label("Inspector ›").click();
    harness.run_steps(8);
    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(config.join("sysmon/settings.json")).expect("settings written to scratch"),
    )
    .unwrap();
    assert_eq!(saved["group_by_app"], true, "P12: group-by persisted");
    assert_eq!(saved["show_inspector"], false, "P12: inspector toggle persisted");
    receipt.insert("P12_persisted".into(), true.into());

    // Ungroup: the per-process rows come back.
    harness.get_by_label("Grouped by app").click();
    harness.run_steps(8);
    assert!(row_for(&harness, WORKER_A) || harness.query_by_label("orchard ×3").is_none(), "ungrouped");

    // ---- receipt: the repeatable artifact.
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/receipts");
    std::fs::create_dir_all(&out_dir).unwrap();
    receipt.insert("test".into(), "ui_bridge::overview_and_processes_flow_both_ways".into());
    receipt.insert("status".into(), "pass".into());
    std::fs::write(
        out_dir.join("ui_bridge.json"),
        serde_json::to_string_pretty(&serde_json::Value::Object(receipt)).unwrap(),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
}
