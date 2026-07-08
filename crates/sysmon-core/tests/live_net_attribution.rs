// SPDX-License-Identifier: GPL-3.0-or-later
//! The traffic-attribution receipt: generate known WAN traffic with
//! curl and require SysMon to attribute it to curl's pid — first via
//! the native tcp_diag path (no nethogs), then via whatever source a
//! default long-lived sampler picks (nethogs on Ben's machine).
//! Needs the network; exits early (loudly) offline.

use std::process::{Command, Stdio};
use std::time::Duration;

use sysmon_core::collect::{Sampler, SamplerOptions};
use sysmon_core::snapshot::{ProcessNetSource, Wants};

// 20 MB stays under the endpoint's size cap (bigger asks 403); at
// 2 MB/s the transfer runs ~10 s — longer than the sampling window.
const DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down?bytes=20000000";
const RATE_LIMIT: &str = "2M";

fn spawn_curl() -> std::process::Child {
    Command::new("curl")
        .args([
            "--silent",
            "--fail", // HTTP errors must be exit-code errors
            "--output",
            "/dev/null",
            "--limit-rate",
            RATE_LIMIT,
            "--max-time",
            "25",
            DOWNLOAD_URL,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("curl exists")
}

fn online() -> bool {
    // A real (tiny) GET — HEAD lies about what a GET will do here.
    Command::new("curl")
        .args([
            "--silent",
            "--fail",
            "--output",
            "/dev/null",
            "--max-time",
            "4",
            "https://speed.cloudflare.com/__down?bytes=1000",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn tcp_diag_attributes_curl_traffic_without_nethogs() {
    if !online() {
        eprintln!("SKIP (offline): tcp_diag attribution needs real WAN traffic");
        return;
    }

    let mut sampler = Sampler::with_options(SamplerOptions {
        enable_nethogs: false,
    });
    let mut wants = Wants::none();
    wants.network = true;
    wants.per_process_net = true;

    let mut curl = spawn_curl();
    let curl_pid = curl.id() as i32;
    std::thread::sleep(Duration::from_millis(2500)); // let TCP ramp

    let _prime = sampler.sample(wants);
    let mut best_rx_bps = 0.0f64;
    let mut source_seen = ProcessNetSource::None;
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(1000));
        let snapshot = sampler.sample(wants);
        let network = snapshot.network.as_ref().expect("network");
        source_seen = network.process_source;
        if let Some(entry) = network.top_processes.iter().find(|e| e.pid == curl_pid) {
            best_rx_bps = best_rx_bps.max(entry.rx_bps);
        }
    }
    let _ = curl.kill();
    let _ = curl.wait();

    assert_eq!(
        source_seen,
        ProcessNetSource::TcpDiag,
        "with nethogs disabled the source must be the native tcp_diag"
    );
    // curl is rate-limited to 3 MB/s; require at least a third of it
    // in the best window (TCP ramp + window skew).
    assert!(
        best_rx_bps > 1_000_000.0,
        "tcp_diag saw only {best_rx_bps} B/s for curl (pid {curl_pid}), expected ~2 MB/s"
    );
    println!("tcp_diag best window: {:.1} MB/s for curl", best_rx_bps / 1e6);
}

#[test]
fn default_sampler_attributes_curl_and_names_its_source() {
    if !online() {
        eprintln!("SKIP (offline): attribution needs real WAN traffic");
        return;
    }

    let mut sampler = Sampler::new(); // nethogs allowed if usable
    let mut wants = Wants::none();
    wants.network = true;
    wants.per_process_net = true;

    let mut curl = spawn_curl();
    let curl_pid = curl.id() as i32;
    std::thread::sleep(Duration::from_millis(2500));

    let _prime = sampler.sample(wants);
    let mut best_rx_bps = 0.0f64;
    let mut source_seen = ProcessNetSource::None;
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(1000));
        let snapshot = sampler.sample(wants);
        let network = snapshot.network.as_ref().expect("network");
        if let Some(entry) = network.top_processes.iter().find(|e| e.pid == curl_pid)
            && entry.rx_bps > best_rx_bps
        {
            best_rx_bps = entry.rx_bps;
            source_seen = network.process_source;
        }
    }
    let _ = curl.kill();
    let _ = curl.wait();

    assert!(
        best_rx_bps > 1_000_000.0,
        "no source attributed curl's ~2 MB/s (best {best_rx_bps} B/s)"
    );
    assert_ne!(source_seen, ProcessNetSource::None);
    println!(
        "source {:?} best window: {:.1} MB/s for curl",
        source_seen,
        best_rx_bps / 1e6
    );
}

#[test]
fn connection_table_lists_own_sockets_with_process_names() {
    let mut sampler = Sampler::with_options(SamplerOptions {
        enable_nethogs: false,
    });
    let mut wants = Wants::none();
    wants.connections = true;
    wants.per_process_net = true;

    // Hold a listener open so at least one own socket exists.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let snapshot = sampler.sample(wants);
    let connections = snapshot.connections.as_ref().expect("connections");
    assert!(!connections.is_empty(), "no sockets at all?");

    let mine = connections
        .iter()
        .find(|c| c.local_address.ends_with(&format!(":{port}")))
        .expect("own listener in connection table");
    assert_eq!(mine.pid, std::process::id() as i32);
    assert_eq!(mine.state, "listen");
    assert!(mine.process_name.is_some());
}

/// Given a longer window, the nethogs merge must actually take over
/// as the rate source on a machine where nethogs has capture caps.
#[test]
fn nethogs_takes_over_given_time() {
    if !online() {
        eprintln!("SKIP (offline)");
        return;
    }
    // Only meaningful where nethogs is usable (it is on Ben's box).
    let caps = std::process::Command::new("sh")
        .args(["-c", "getcap $(command -v nethogs) 2>/dev/null"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    if !(caps.contains("cap_net_raw") && caps.contains("cap_net_admin")) {
        eprintln!("SKIP: nethogs not usable here");
        return;
    }

    let mut sampler = Sampler::new();
    let mut wants = Wants::none();
    wants.network = true;
    wants.per_process_net = true;

    let mut curl = spawn_curl();
    let curl_pid = curl.id() as i32;
    let _prime = sampler.sample(wants);

    let mut nethogs_best_rx = 0.0f64;
    for _ in 0..9 {
        std::thread::sleep(Duration::from_millis(1000));
        let snapshot = sampler.sample(wants);
        let network = snapshot.network.as_ref().unwrap();
        if network.process_source == ProcessNetSource::Nethogs
            && let Some(entry) = network.top_processes.iter().find(|e| e.pid == curl_pid)
        {
            nethogs_best_rx = nethogs_best_rx.max(entry.rx_bps);
        }
    }
    let _ = curl.kill();
    let _ = curl.wait();

    assert!(
        nethogs_best_rx > 700_000.0,
        "nethogs never became the source with usable caps (best {nethogs_best_rx} B/s)"
    );
    println!("nethogs best window: {:.1} MB/s for curl", nethogs_best_rx / 1e6);
}
