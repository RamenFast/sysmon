// SPDX-License-Identifier: GPL-3.0-or-later
//! The Performance page, end to end through AccessKit
//! (docs/dev/PERFORMANCE-VIEW.md, FAILURE-MODES rows V1–V9).
//!
//!   V1 the meter and the graph's newest point are the same field
//!   V3 per-thread graphs are in /proc/stat order, each with its value
//!   V7 nothing clips at 430 px (every node inside the window)
//!   V9 the per-thread grid fits at 430 and 1240 px, cells ≥ 28 px
//!   + the toggle switches modes and survives a restart (settings file)
//!   + a meter click lands on the Overview card; a graph click on
//!     Processes sorted by that resource
//!   + the status bar is the snapshot
//!   + startup lands on Performance
//!
//! The sampler is paused and the test owns the snapshot and the
//! histories, so every number is known. Isolation law: scratch
//! XDG_CONFIG_HOME + XDG_RUNTIME_DIR. Ends with a receipt
//! (target/receipts/ui_performance.json).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use sysmon_app::gui::app::{Page, SharedUi, SysMonApp};
use sysmon_app::gui::performance::{MINI_GRAPH_MIN_HEIGHT, status_text};
use sysmon_app::gui::processes::SortColumn;
use sysmon_app::gui::settings::Settings;
use sysmon_core::snapshot::{CpuSnapshot, GpuSnapshot, MemorySnapshot, ProcessRecord, SystemSnapshot};

const CORES: usize = 8;

fn snapshot() -> SystemSnapshot {
    SystemSnapshot {
        ts: 1.0,
        interval_seconds: 1.0,
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
            per_core_percent: (0..CORES).map(|i| 10.0 + i as f32 * 10.0).collect(),
            core_count: CORES,
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
        processes: Some(vec![
            ProcessRecord { pid: 1, name: "init".into(), display_name: "init".into(), threads: 1, ..Default::default() },
            ProcessRecord { pid: 2, name: "kiln".into(), display_name: "kiln".into(), threads: 7, ..Default::default() },
        ]),
        ..Default::default()
    }
}

/// Feed the histories the way the sampler would, from the snapshot.
fn push_histories(shared: &SharedUi, snapshot: &SystemSnapshot) {
    let mut h = shared.histories.write().unwrap();
    let cpu = snapshot.cpu.as_ref().unwrap();
    h.cpu.push(cpu.overall_percent as f64);
    h.cpu_kernel.push(cpu.kernel_percent as f64);
    if h.per_core.len() != CORES {
        h.per_core = vec![Default::default(); CORES];
    }
    for (history, percent) in h.per_core.iter_mut().zip(&cpu.per_core_percent) {
        history.push(*percent as f64);
    }
    let memory = snapshot.memory.as_ref().unwrap();
    h.memory.push(memory.used_percent as f64);
    h.memory_cache.push(25.0);
    let gpu = snapshot.gpu.as_ref().unwrap();
    h.gpu.push(gpu.busy_percent as f64);
    h.cpu_temperature.push(71.0);
    h.gpu_temperature.push(39.0);
}

fn label_of(node: &egui_kittest::Node<'_>) -> String {
    node.accesskit_node().label().unwrap_or_default().to_string()
}

fn find_label(harness: &Harness<'_, SysMonApp>, prefix: &str) -> Option<String> {
    harness
        .query_all_by_role(Role::Button)
        .map(|n| label_of(&n))
        .find(|l| l.starts_with(prefix))
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
    let snap = snapshot();
    push_histories(&control, &snap);
    *control.latest.write().unwrap() = Arc::new(snap);
    (harness, control)
}

#[test]
fn the_performance_page_shows_the_snapshot_and_switches_modes() {
    let scratch = std::env::temp_dir().join(format!("sysmon-performance-{}", std::process::id()));
    let config = scratch.join("config");
    let runtime = scratch.join("run");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    }
    let mut receipt = serde_json::Map::new();

    // ---- startup lands on Performance (fresh settings).
    let (mut harness, _shared) = build(430.0, Settings::default());
    harness.run_steps(6);
    assert_eq!(harness.state().page, Page::Performance, "fresh settings open on Performance");

    // ---- V1: meter label == snapshot field == newest graph sample.
    let cpu_meter = find_label(&harness, "CPU meter:").expect("CPU meter");
    assert_eq!(cpu_meter, "CPU meter: 37%");
    let cpu_graph = find_label(&harness, "CPU history:").expect("combined CPU graph at 430 px (auto)");
    assert_eq!(cpu_graph, "CPU history: busy 37%  kernel 9%");
    assert_eq!(find_label(&harness, "Memory meter:").unwrap(), "Memory meter: 13%");
    assert_eq!(find_label(&harness, "GPU meter:").unwrap(), "GPU meter: 3%");
    assert_eq!(find_label(&harness, "Thermals meter:").unwrap(), "Thermals meter: 71°C");
    receipt.insert("v1_cpu_meter".into(), cpu_meter.into());

    // ---- status bar is the snapshot.
    let status = find_label(&harness, "status: ").expect("status bar");
    let expected = status_text(&snapshot(), Settings::default().display(), false);
    assert_eq!(status, format!("status: {expected}"));
    assert!(expected.contains("2 proc") && expected.contains("CPU 37%") && expected.contains("71°C"), "{expected}");
    receipt.insert("status".into(), expected.into());

    // ---- V7: nothing drawn outside the 430 px window.
    let window = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(430.0, 780.0));
    for node in harness.query_all_by_role(Role::Button) {
        let rect = node.rect();
        let label = label_of(&node);
        if label.starts_with("status: ") || label.contains("meter:") || label.contains("history:") {
            assert!(
                rect.min.x >= -0.5 && rect.max.x <= window.max.x + 0.5,
                "V7: `{label}` spans x {}..{} in a 430 px window",
                rect.min.x,
                rect.max.x
            );
        }
    }

    // ---- the toggle: click "per thread" (right half of the switch).
    let switch = harness
        .query_all_by_role(Role::Button)
        .find(|n| label_of(n).starts_with("CPU graph:"))
        .expect("stone switch");
    let rect = switch.rect();
    let right_half = egui::pos2(rect.right() - rect.width() * 0.25, rect.center().y);
    harness.input_mut().events.push(egui::Event::PointerMoved(right_half));
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: right_half,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: right_half,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    harness.run_steps(4);
    assert_eq!(harness.state().settings.cpu_graph_mode, "per_thread", "the switch wrote the choice");
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config.join("sysmon/settings.json")).unwrap()).unwrap();
    assert_eq!(saved["cpu_graph_mode"], "per_thread", "…and it survives a restart");

    // ---- V3 + V9 at 430 px: 8 thread graphs, in order, each with
    // its own value, inside the window, ≥ 28 px tall.
    let threads: Vec<(String, egui::Rect)> = harness
        .query_all_by_role(Role::Button)
        .filter(|n| label_of(n).starts_with("CPU thread "))
        .map(|n| (label_of(&n), n.rect()))
        .collect();
    assert_eq!(threads.len(), CORES, "one mini graph per thread");
    for (index, (label, rect)) in threads.iter().enumerate() {
        assert_eq!(*label, format!("CPU thread {index}: {}%", 10 + index * 10), "V3 order + value");
        assert!(rect.height() >= MINI_GRAPH_MIN_HEIGHT - 0.5, "V9: thread {index} is {} px tall", rect.height());
        assert!(rect.max.x <= 430.5 && rect.min.x >= -0.5, "V9: thread {index} outside the window: {rect:?}");
    }
    // Row-major: thread 1 is right of thread 0 on the same row.
    assert!(threads[1].1.min.x > threads[0].1.min.x && (threads[1].1.min.y - threads[0].1.min.y).abs() < 1.0);
    receipt.insert("threads_430".into(), threads.len().into());

    // ---- a meter click opens the Overview on that card.
    harness.get_by_label("Memory meter: 13%").click();
    harness.run_steps(3);
    assert_eq!(harness.state().page, Page::Overview, "meter → Overview");
    harness.get_by_label("Performance").click();
    harness.run_steps(3);
    assert_eq!(harness.state().page, Page::Performance);

    // ---- a graph click opens Processes sorted by that resource.
    harness.get_by_label("Memory history: used 13%  cache 25%").click();
    harness.run_steps(3);
    assert_eq!(harness.state().page, Page::Processes, "graph → Processes");
    assert_eq!(harness.state().table_state.sort_column, SortColumn::Memory);
    assert!(harness.state().table_state.sort_descending);

    // ---- V9 at 1240 px: the grid still holds every thread with the
    // floor height, and the layout goes two-column (two meters on one
    // row).
    drop(harness);
    let (mut harness, _shared) =
        build(1240.0, Settings { cpu_graph_mode: "per_thread".to_string(), ..Settings::default() });
    harness.run_steps(6);
    assert_eq!(harness.state().page, Page::Performance);
    let threads: Vec<egui::Rect> = harness
        .query_all_by_role(Role::Button)
        .filter(|n| label_of(n).starts_with("CPU thread "))
        .map(|n| n.rect())
        .collect();
    assert_eq!(threads.len(), CORES);
    for rect in &threads {
        assert!(rect.height() >= MINI_GRAPH_MIN_HEIGHT - 0.5 && rect.max.x <= 1240.5);
    }
    let cpu_meter = harness.get_by_label("CPU meter: 37%").rect();
    let disk_meter = harness
        .query_all_by_role(Role::Button)
        .find(|n| label_of(n).starts_with("Disk meter:"))
        .map(|n| n.rect());
    if let Some(disk_meter) = disk_meter {
        assert!((disk_meter.min.y - cpu_meter.min.y).abs() < 2.0, "wide: Disk sits beside CPU");
    }
    receipt.insert("threads_1240".into(), threads.len().into());

    // ---- receipt: the repeatable artifact.
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/receipts");
    std::fs::create_dir_all(&out_dir).unwrap();
    receipt.insert("test".into(), "ui_performance::the_performance_page_shows_the_snapshot_and_switches_modes".into());
    receipt.insert("status".into(), "pass".into());
    std::fs::write(
        out_dir.join("ui_performance.json"),
        serde_json::to_string_pretty(&serde_json::Value::Object(receipt)).unwrap(),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
}
