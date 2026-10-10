// SPDX-License-Identifier: GPL-3.0-or-later
//! Live specs: read the machine this test runs on and check what any
//! Linux desktop must have. Run with `-- --nocapture` to read the
//! sheet and the JSON it came from.

use sysmon_core::specs;

#[test]
fn live_specs_read_the_whole_machine() {
    let started = std::time::Instant::now();
    let specs = specs::collect();
    let elapsed = started.elapsed();
    assert!(elapsed.as_secs() < 8, "collect took {elapsed:?}: the helper deadlines aren't holding");

    let cpu = specs.cpu.as_ref().expect("cpu");
    assert_eq!(cpu.threads, std::thread::available_parallelism().unwrap().get());
    if let Some(cores) = cpu.cores {
        assert!(cores >= 1 && cores <= cpu.threads, "{cores} cores, {} threads", cpu.threads);
    }
    let memory = specs.memory.as_ref().expect("memory");
    assert!(memory.usable_bytes > 1 << 30);
    if let Some(installed) = memory.installed_bytes {
        assert!(installed >= memory.usable_bytes, "installed {installed} < usable {}", memory.usable_bytes);
    }
    assert!(!specs.drives.is_empty(), "no drives");
    assert!(
        specs.drives.iter().flat_map(|d| &d.filesystems).any(|f| f.mount_points.iter().any(|m| m == "/")),
        "the root filesystem's drive is missing: {:#?}",
        specs.drives
    );
    // Every drive is a physical device, listed once.
    let mut names: Vec<&str> = specs.drives.iter().map(|d| d.name.as_str()).collect();
    names.dedup();
    assert_eq!(names.len(), specs.drives.len());
    assert!(specs.drives.iter().all(|d| !d.name.starts_with("loop")));
    // A GPU listed is a real PCI device, never a software renderer.
    for gpu in &specs.gpus {
        assert!(!gpu.name.contains("llvmpipe"), "{gpu:?}");
        assert_eq!(gpu.pci_id.len(), 9, "{gpu:?}");
    }

    let text = specs::text::specs_text(&specs);
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    let hostname = hostname.trim();
    if hostname.len() > 3 {
        assert!(!text.contains(hostname), "the shared sheet carries the hostname:\n{text}");
    }
    println!("{text}");
    println!("collected in {elapsed:?}");
    println!("{}", serde_json::to_string_pretty(&specs).unwrap());
}
