// SPDX-License-Identifier: GPL-3.0-or-later
//! The one Overview, end to end through AccessKit (3.6:
//! docs/dev/OVERVIEW-VIEW.md; the Performance page folded in).
//!
//!   V1 every graph's accessible readout is the snapshot it was fed
//!   V3 per-thread grid cells are in /proc/stat order, each with its value
//!   V7 nothing clips at 430 px (every node inside the window)
//!   V9 the grid fits at 430 and 1240 px, cells ≥ 28 px
//!   + startup lands on the Overview; no Performance tab is left
//!   + the CPU switch steps total · threads · grid and survives a
//!     restart (settings file); threads is the default
//!   + everything only the Performance page showed has a home here:
//!     kernel time, VRAM and cache history, disk and thermal history,
//!     commit charge + peak, kernel memory, threads, uptime
//!   + the status bar is the snapshot
//!   + wide windows lay the cards out in two columns
//!
//! The sampler is paused and the test owns the snapshot and the
//! histories, so every number is known. Isolation law: scratch
//! XDG_CONFIG_HOME + XDG_RUNTIME_DIR + XDG_STATE_HOME. Ends with a
//! receipt (target/receipts/ui_overview.json).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use sysmon_app::gui::app::{Page, SharedUi, SysMonApp};
use sysmon_app::gui::graphs::MINI_GRAPH_MIN_HEIGHT;
use sysmon_app::gui::overview::status_text;
use sysmon_app::gui::settings::Settings;
use sysmon_core::snapshot::{
    CpuSnapshot, DiskSnapshot, GpuSnapshot, MemorySnapshot, ProcessRecord, SystemInfo, SystemSnapshot,
};

/// Ben's machine: 32 threads, °F + °C.
const CORES: usize = 32;

fn snapshot() -> SystemSnapshot {
    SystemSnapshot {
        ts: 1.0,
        interval_seconds: 1.0,
        system: Some(SystemInfo { uptime_seconds: 3.0 * 86_400.0 + 4.0 * 3_600.0 + 12.0 * 60.0, ..Default::default() }),
        memory: Some(MemorySnapshot {
            total_bytes: 64_000_000_000,
            used_bytes: 8_000_000_000,
            available_bytes: 56_000_000_000,
            cached_bytes: 16_000_000_000,
            used_percent: 13.0,
            committed_bytes: 30_100_000_000,
            commit_limit_bytes: 67_500_000_000,
            slab_bytes: 1_200_000_000,
            page_tables_bytes: 210_400_000,
            kernel_stack_bytes: 48_600_000,
            ..Default::default()
        }),
        cpu: Some(CpuSnapshot {
            overall_percent: 37.0,
            kernel_percent: 9.0,
            // Distinct per thread so order is checkable (V3).
            per_core_percent: (0..CORES).map(|i| 3.0 + i as f32 * 3.0).collect(),
            core_count: CORES,
            tasks_total: 2_681,
            temperature_celsius: Some(71.0),
            frequency_busy_mhz: Some(4130.0),
            context_switches_per_second: 13_106.0,
            ..Default::default()
        }),
        gpu: Some(GpuSnapshot {
            available: true,
            busy_percent: 3.0,
            vram_used_bytes: 1_000_000_000,
            vram_total_bytes: 32_000_000_000,
            temperature_edge_celsius: Some(39.0),
            ..Default::default()
        }),
        disks: Some(vec![DiskSnapshot {
            device: "/dev/nvme0n1p2".into(),
            mount_point: "/".into(),
            display_name: "root".into(),
            fs_type: "btrfs".into(),
            read_bps: 2_000_000.0,
            write_bps: 500_000.0,
            used_bytes: 400_000_000_000,
            total_bytes: 1_000_000_000_000,
            util_percent: 4.0,
        }]),
        processes: Some(vec![
            ProcessRecord { pid: 1, name: "init".into(), display_name: "init".into(), threads: 1, ..Default::default() },
            ProcessRecord { pid: 2, name: "kiln".into(), display_name: "kiln".into(), threads: 7, ..Default::default() },
        ]),
        ..Default::default()
    }
}

/// Feed the histories through the SAME function the sampler uses
/// (reviewer R16: a hand-rolled push here would let a wrong sampler
/// push pass), under the same guard order the sampler publishes in.
fn push_histories(shared: &SharedUi, snapshot: &SystemSnapshot) {
    let mut histories = shared.histories.write().unwrap();
    histories.record(snapshot);
    *shared.latest.write().unwrap() = Arc::new(snapshot.clone());
}

/// A node's text: Buttons carry it as the label, Labels as the value.
fn label_of(node: &egui_kittest::Node<'_>) -> String {
    let node = node.accesskit_node();
    let label = node.label().unwrap_or_default().to_string();
    if label.is_empty() { node.value().unwrap_or_default().to_string() } else { label }
}

fn find_label(harness: &Harness<'_, SysMonApp>, prefix: &str) -> Option<String> {
    harness
        .query_all_by_role(Role::Button)
        .map(|n| label_of(&n))
        .find(|l| l.starts_with(prefix))
}

fn node_rect(harness: &Harness<'_, SysMonApp>, prefix: &str) -> egui::Rect {
    harness
        .query_all_by_role(Role::Button)
        .find(|n| label_of(n).starts_with(prefix))
        .map(|n| n.rect())
        .unwrap_or_else(|| panic!("no node starting `{prefix}`"))
}

fn build(width: f32, settings: Settings) -> (Harness<'static, SysMonApp>, Arc<SharedUi>) {
    let shared = Arc::new(SharedUi::new(0.5));
    let control = shared.clone();
    let (_tx, rx) = std::sync::mpsc::channel();
    // _tx dropped: the app's command receiver just stays empty.
    let harness = Harness::builder()
        .with_size(egui::vec2(width, 780.0))
        .build_eframe(move |cc| SysMonApp::new(cc, settings, shared, rx, None));
    control.paused.store(true, Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(900));
    // The live sampler may have recorded a sample or two before the
    // pause landed; drop them, so every graph holds only the fixture
    // (a series the fixture lacks, such as this machine's NVIDIA card,
    // would otherwise keep its live line).
    *control.histories.write().unwrap() = Default::default();
    let snap = snapshot();
    push_histories(&control, &snap);
    (harness, control)
}

/// Click a stone-switch position by fraction of its width (0..1).
fn click_switch_at(harness: &mut Harness<'_, SysMonApp>, fraction: f32) {
    let rect = node_rect(harness, "CPU graph:");
    let at = egui::pos2(rect.left() + rect.width() * fraction, rect.center().y);
    harness.input_mut().events.push(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        });
    }
    harness.run_steps(4);
}

#[test]
fn the_overview_holds_everything_and_switches_cpu_views() {
    let scratch = std::env::temp_dir().join(format!("sysmon-overview-{}", std::process::id()));
    let config = scratch.join("config");
    let runtime = scratch.join("run");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &config);
        // History is recorded by the sampler: never into the real one.
        std::env::set_var("XDG_STATE_HOME", config.join("state"));
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    }
    let mut receipt = serde_json::Map::new();

    // ---- startup lands on the Overview; the Performance tab is gone.
    let settings = Settings { temperature_scale: "both".to_string(), ..Settings::default() };
    let (mut harness, _shared) = build(430.0, settings.clone());
    harness.run_steps(6);
    assert_eq!(harness.state().page, Page::Overview, "fresh settings open on the Overview");
    assert!(harness.query_by_label("Performance").is_none(), "no Performance tab is left");
    assert!(harness.query_by_label("Overview").is_some() && harness.query_by_label("Processes").is_some());

    // ---- V1: each graph's readout is the snapshot. Threads is the
    // default CPU view: 32 lines, the busiest named.
    assert_eq!(find_label(&harness, "CPU threads:").unwrap(), "CPU threads: 32 lines, busiest thread 31 96%");
    assert!(find_label(&harness, "CPU graph:").unwrap().ends_with("(now: threads)"), "the switch names its state");
    assert_eq!(find_label(&harness, "GPU history:").unwrap(), "GPU history: busy 3%  VRAM 3%");
    assert_eq!(find_label(&harness, "Memory history:").unwrap(), "Memory history: used 13%  cache 25%");
    assert_eq!(find_label(&harness, "Disk history:").unwrap(), "Disk history: read 2.0 MB/s  write 500.0 kB/s");
    assert_eq!(find_label(&harness, "Thermal history:").unwrap(), "Thermal history: CPU 160°F · 71°C  GPU 102°F · 39°C");
    receipt.insert("v1_threads".into(), find_label(&harness, "CPU threads:").unwrap().into());

    // ---- status bar is the snapshot (430 px: the narrow line).
    let status = find_label(&harness, "status: ").expect("status bar");
    let expected = status_text(&snapshot(), settings.display(), false);
    assert_eq!(status, format!("status: {expected}"));
    assert!(expected.contains("2 proc") && expected.contains("CPU 37%") && expected.contains("160°F · 71°C"), "{expected}");
    receipt.insert("status".into(), expected.into());
    let wide_status = status_text(&snapshot(), settings.display(), true);
    assert!(wide_status.ends_with("Commit 30.1 GB / 67.5 GB │ up 3d 04:12"), "{wide_status}");

    // ---- what only the Performance page showed now has a home, and
    // V7: none of it is drawn outside the 430 px window.
    let window = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(430.0, 780.0));
    let folded_in = ["30.1 GB / 67.5 GB", "1.2 GB", "210.4 MB", "48.6 MB", "2681", "9%"];
    let mut seen: Vec<&str> = Vec::new();
    for node in harness.query_all_by_role(Role::Button).chain(harness.query_all_by_role(Role::Label)) {
        let rect = node.rect();
        let label = label_of(&node);
        if let Some(value) = folded_in.iter().find(|v| **v == label) {
            seen.push(value);
        }
        if label.starts_with("status: ")
            || label.contains("history:")
            || label.starts_with("CPU threads:")
            || folded_in.contains(&label.as_str())
        {
            assert!(
                rect.min.x >= -0.5 && rect.max.x <= window.max.x + 0.5,
                "V7: `{label}` spans x {}..{} in a 430 px window",
                rect.min.x,
                rect.max.x
            );
        }
    }
    for value in folded_in {
        assert!(seen.contains(&value), "the folded-in value `{value}` is on the Overview (saw {seen:?})");
    }
    assert!(harness.query_by_label("Peak*").is_some(), "the commit peak kept its asterisk");
    receipt.insert("folded_in".into(), seen.len().into());

    // ---- the switch: grid is the right third.
    click_switch_at(&mut harness, 0.85);
    assert_eq!(harness.state().settings.cpu_graph_mode, "grid", "the switch wrote the choice");
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config.join("sysmon/settings.json")).unwrap()).unwrap();
    assert_eq!(saved["cpu_graph_mode"], "grid", "…and it survives a restart");
    assert!(find_label(&harness, "CPU graph:").unwrap().ends_with("(now: grid)"));
    assert!(find_label(&harness, "CPU threads:").is_none(), "one CPU view at a time");

    // ---- V3 + V9 at 430 px: one cell per thread, in order, each with
    // its own value, inside the window, ≥ 28 px tall.
    let threads: Vec<(String, egui::Rect)> = harness
        .query_all_by_role(Role::Button)
        .filter(|n| label_of(n).starts_with("CPU thread "))
        .map(|n| (label_of(&n), n.rect()))
        .collect();
    assert_eq!(threads.len(), CORES, "one mini graph per thread");
    for (index, (label, rect)) in threads.iter().enumerate() {
        assert_eq!(*label, format!("CPU thread {index}: {}%", 3 + index * 3), "V3 order + value");
        assert!(rect.height() >= MINI_GRAPH_MIN_HEIGHT - 0.5, "V9: thread {index} is {} px tall", rect.height());
        assert!(rect.max.x <= 430.5 && rect.min.x >= -0.5, "V9: thread {index} outside the window: {rect:?}");
    }
    // Row-major: thread 1 is right of thread 0 on the same row.
    assert!(threads[1].1.min.x > threads[0].1.min.x && (threads[1].1.min.y - threads[0].1.min.y).abs() < 1.0);
    receipt.insert("threads_430".into(), threads.len().into());

    // ---- total is the left third: busy with kernel time under it.
    click_switch_at(&mut harness, 0.12);
    assert_eq!(harness.state().settings.cpu_graph_mode, "total");
    assert_eq!(find_label(&harness, "CPU history:").unwrap(), "CPU history: busy 37%  kernel 9%");
    // …and threads the middle.
    click_switch_at(&mut harness, 0.5);
    assert_eq!(harness.state().settings.cpu_graph_mode, "threads");

    // ---- wide: two columns of cards, the grid still holds every
    // thread at the floor height, and the status line grows its tail.
    drop(harness);
    let (mut harness, _shared) = build(1240.0, Settings { cpu_graph_mode: "grid".to_string(), ..Settings::default() });
    harness.run_steps(6);
    assert_eq!(harness.state().page, Page::Overview);
    let threads: Vec<egui::Rect> = harness
        .query_all_by_role(Role::Button)
        .filter(|n| label_of(n).starts_with("CPU thread "))
        .map(|n| n.rect())
        .collect();
    assert_eq!(threads.len(), CORES);
    for rect in &threads {
        assert!(rect.height() >= MINI_GRAPH_MIN_HEIGHT - 0.5 && rect.max.x <= 1240.5);
    }
    let gpu = node_rect(&harness, "GPU history:");
    let memory = node_rect(&harness, "Memory history:");
    assert!(
        (gpu.min.y - memory.min.y).abs() < 2.0 && memory.min.x > gpu.max.x,
        "wide: Memory sits beside GPU ({gpu:?} / {memory:?})"
    );
    assert!(find_label(&harness, "status: ").unwrap().contains("up 3d 04:12"), "a wide status bar names the uptime");
    receipt.insert("threads_1240".into(), threads.len().into());

    // ---- receipt: the repeatable artifact.
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/receipts");
    std::fs::create_dir_all(&out_dir).unwrap();
    receipt.insert("test".into(), "ui_overview::the_overview_holds_everything_and_switches_cpu_views".into());
    receipt.insert("status".into(), "pass".into());
    std::fs::write(
        out_dir.join("ui_overview.json"),
        serde_json::to_string_pretty(&serde_json::Value::Object(receipt)).unwrap(),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
}
