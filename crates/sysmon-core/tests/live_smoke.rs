// SPDX-License-Identifier: GPL-3.0-or-later
//! Live smoke: sample the machine this test runs on and check shape
//! sanity (the numeric cross-checks live in accuracy.rs). Run with
//! `-- --nocapture` to eyeball a full JSON snapshot.

use std::time::Duration;

use sysmon_core::collect::Sampler;
use sysmon_core::snapshot::Wants;

#[test]
fn live_snapshot_has_sane_shape() {
    let mut sampler = Sampler::new();
    let _prime = sampler.sample(Wants::all());
    std::thread::sleep(Duration::from_millis(250));
    let snapshot = sampler.sample(Wants::all());

    assert!(snapshot.interval_seconds > 0.2 && snapshot.interval_seconds < 5.0);
    assert!(snapshot.ts > 1.7e9, "unix time looks wrong");

    let cpu = snapshot.cpu.as_ref().expect("cpu section");
    let cores = std::thread::available_parallelism().unwrap().get();
    assert_eq!(cpu.core_count, cores);
    assert_eq!(cpu.per_core_percent.len(), cores);
    assert!(cpu.load_1m >= 0.0);
    assert!(cpu.tasks_total > 50, "task count implausibly low");

    let memory = snapshot.memory.as_ref().expect("memory section");
    assert!(memory.total_bytes > 1 << 30, "less than 1 GiB of RAM?");
    assert!(memory.available_bytes < memory.total_bytes);
    assert_eq!(
        memory.used_bytes,
        memory.total_bytes - memory.available_bytes,
        "used must be the v1 identity: total - available"
    );

    let disks = snapshot.disks.as_ref().expect("disks section");
    assert!(
        disks.iter().any(|d| d.mount_point == "/"),
        "root filesystem missing from {:?}",
        disks.iter().map(|d| &d.mount_point).collect::<Vec<_>>()
    );

    let network = snapshot.network.as_ref().expect("network section");
    assert!(
        !network.interfaces.is_empty(),
        "no network interfaces found"
    );

    let processes = snapshot.processes.as_ref().expect("processes section");
    assert!(processes.iter().any(|p| p.pid == 1), "pid 1 missing");
    let me = std::process::id() as i32;
    let self_record = processes
        .iter()
        .find(|p| p.pid == me)
        .expect("own process missing");
    assert!(self_record.memory_rss_bytes > 1 << 20, "own RSS under 1 MiB");
    assert!(!self_record.user.is_empty());
    assert!(self_record.started_ts > 1.7e9);

    let sensors = snapshot.sensors.as_ref().expect("sensors section");
    assert!(
        !sensors.chips.is_empty(),
        "no hwmon chips found on a desktop"
    );

    println!(
        "snapshot: {}",
        serde_json::to_string_pretty(&snapshot).unwrap()
    );
}
