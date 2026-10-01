// SPDX-License-Identifier: GPL-3.0-or-later
//! The contract, tested against the built binary.
//!
//! `sysmon schema` is a promise to an agent that has never seen this
//! tool. These tests keep the promise honest: every jq path the
//! schema prints is *run*, every enum it publishes is *exercised*,
//! and every verb it names is *invoked*.
//!
//! This exists because a documented command shipped for three
//! releases returning `null` — `jq .result.top_processes`, when the
//! field lives at `.result.network.top_processes`. A schema that can
//! be wrong is worse than no schema, because an agent will believe
//! it.
//!
//! Isolation: every invocation gets a private XDG_RUNTIME_DIR and
//! XDG_CONFIG_HOME, so a test run can never drive, or repaint the
//! saved settings of, the user's real instance.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn binary() -> PathBuf {
    // target/release/deps/<test> → target/release/sysmon
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("sysmon")
}

struct Sandbox {
    _runtime: tempdir::TempDir,
    _config: tempdir::TempDir,
    runtime_path: PathBuf,
    config_path: PathBuf,
}

/// A tiny private-directory helper — a whole crate would be a
/// dependency for four lines.
mod tempdir {
    use std::path::{Path, PathBuf};

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> TempDir {
            let unique = format!(
                "sysmon-test-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

impl Sandbox {
    fn new() -> Sandbox {
        let runtime = tempdir::TempDir::new("run");
        let config = tempdir::TempDir::new("cfg");
        let runtime_path = runtime.path().to_path_buf();
        let config_path = config.path().to_path_buf();
        Sandbox {
            _runtime: runtime,
            _config: config,
            runtime_path,
            config_path,
        }
    }

    fn run(&self, arguments: &[&str]) -> (Value, i32) {
        let output = Command::new(binary())
            .args(arguments)
            .env("XDG_RUNTIME_DIR", &self.runtime_path)
            .env("XDG_CONFIG_HOME", &self.config_path)
            .output()
            .unwrap_or_else(|error| panic!("running sysmon {arguments:?}: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed = serde_json::from_str(&stdout).unwrap_or_else(|error| {
            panic!("sysmon {arguments:?} did not print one JSON object ({error}): {stdout}")
        });
        (parsed, output.status.code().unwrap_or(-1))
    }
}

fn schema() -> Value {
    Sandbox::new().run(&["schema"]).0
}

/// Walk a dotted jq-style path (`result.network.top_processes`).
fn walk<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cursor = value;
    for segment in path.split('.') {
        cursor = cursor.get(segment)?;
    }
    Some(cursor)
}

#[test]
fn the_schema_carries_the_envelope() {
    let schema = schema();
    for field in ["status", "tool", "version", "ts"] {
        assert!(schema.get(field).is_some(), "schema is missing `{field}`");
    }
    assert_eq!(schema["status"], "ok");
    assert_eq!(schema["tool"], "sysmon");
    let ts = schema["ts"].as_str().expect("ts is a string");
    assert!(ts.contains('T') && ts.ends_with("+00:00"), "ts={ts}");
}

/// Every `jq` path printed anywhere in the schema or the docs must
/// actually resolve against a real probe. This is the test that
/// would have caught `.result.top_processes`.
#[test]
fn every_documented_jq_path_resolves_against_a_real_probe() {
    let schema_text = serde_json::to_string(&schema()).unwrap();
    let docs = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/API.md")
            .canonicalize()
            .expect("docs/API.md"),
    )
    .expect("read docs/API.md");

    // `jq .result.foo.bar` / `jq .result.foo[0]` in either surface
    let mut paths = Vec::new();
    for haystack in [schema_text.as_str(), docs.as_str()] {
        for fragment in haystack.split("jq ").skip(1) {
            let candidate: String = fragment
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '`' && *c != '\'' && *c != '"')
                .collect();
            if let Some(path) = candidate.strip_prefix(".result.") {
                // array indexing and pipes are out of scope; the
                // object path in front of them is what we verify
                let path = path.split(['[', '|']).next().unwrap_or("").trim_end_matches('.');
                if !path.is_empty() {
                    paths.push(path.to_string());
                }
            }
        }
    }
    assert!(
        !paths.is_empty(),
        "no documented jq paths found — the harvester is broken, not the docs"
    );

    let sandbox = Sandbox::new();
    let (probe, code) = sandbox.run(&["probe", "--json"]);
    assert_eq!(code, 0);
    let result = &probe["result"];

    let mut unresolved = Vec::new();
    for path in &paths {
        match walk(result, path) {
            Some(Value::Null) | None => unresolved.push(path.clone()),
            Some(_) => {}
        }
    }
    assert!(
        unresolved.is_empty(),
        "documented jq paths that resolve to nothing: {unresolved:?}"
    );
}

/// The schema publishes enums for the ctl verbs. Each one must be a
/// value the binary actually accepts — read from the enforcing code,
/// so this can only fail if someone hand-edits the schema.
#[test]
fn published_enums_match_the_code_that_enforces_them() {
    let schema = schema();
    let ctl = &schema["contract"]["verbs"]["ctl"]["verbs"];

    let themes: Vec<&str> = ctl["theme"]["value"]["enum"]
        .as_array()
        .expect("theme enum")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(themes.contains(&"system"));
    assert!(themes.contains(&"blossom_dark"), "the default must be listed");
    assert!(themes.contains(&"greyscale"), "the a11y floor must be listed");
    // every published theme resolves (except the synthetic "system")
    for theme in &themes {
        if *theme == "system" {
            continue;
        }
        assert!(
            sysmon_app::gui::theme::palette_by_id(theme).is_some(),
            "schema publishes theme `{theme}`, which the binary cannot resolve"
        );
    }

    let palettes: Vec<&str> = ctl["palette"]["value"]["enum"]
        .as_array()
        .expect("palette enum")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for palette in &palettes {
        assert!(
            sysmon_app::gui::theme::GRAPH_PALETTES
                .iter()
                .any(|candidate| candidate.id == *palette),
            "schema publishes graph palette `{palette}`, which does not exist"
        );
    }

    let popouts: Vec<&str> = ctl["popout"]["value"]["enum"]
        .as_array()
        .expect("popout enum")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for section in &popouts {
        assert!(
            sysmon_app::gui::settings::SECTION_KEYS.contains(section),
            "schema publishes pop-out section `{section}`, which does not exist"
        );
    }

    // Temperature scale: every published id parses, and every scale
    // the code has is published (a scale the GUI offers but the
    // schema hides is one an agent can't set).
    use sysmon_core::units::TemperatureScale;
    let scales: Vec<&str> = ctl["temperature"]["value"]["enum"]
        .as_array()
        .expect("temperature enum")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for id in &scales {
        assert!(TemperatureScale::from_id(id).is_some(), "schema publishes scale `{id}`, which doesn't parse");
    }
    for scale in [TemperatureScale::Celsius, TemperatureScale::Fahrenheit, TemperatureScale::Both] {
        assert!(scales.contains(&scale.id()), "scale `{}` exists but isn't published", scale.id());
    }
}

/// Every section the schema names must be probeable, and answer.
#[test]
fn every_published_section_probes() {
    let schema = schema();
    let sections: Vec<String> = schema["contract"]["verbs"]["probe"]["arguments"]["sections"]
        ["items"]["enum"]
        .as_array()
        .expect("section enum")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(sections.len() >= 9, "{sections:?}");

    let sandbox = Sandbox::new();
    for section in &sections {
        let (reply, code) = sandbox.run(&["probe", section, "--json"]);
        assert_eq!(code, 0, "probe {section} exited {code}");
        assert_eq!(reply["status"], "ok", "probe {section}: {reply}");
        assert!(
            reply["result"]["via"].is_string(),
            "probe {section} did not say how it was sampled"
        );
    }
}

/// Errors are parseable, fix-bearing, and self-classifying — the
/// whole point of the envelope.
#[test]
fn errors_are_parseable_and_carry_the_way_out() {
    let sandbox = Sandbox::new();
    for (arguments, expected_exit) in [
        (vec!["nonsuchverb"], 3),
        (vec!["probe", "nonsuchsection"], 3),
        (vec!["ctl"], 3),
        (vec!["ctl", "status"], 2), // nothing running in the sandbox
    ] {
        let (reply, code) = sandbox.run(&arguments);
        assert_eq!(code, expected_exit, "{arguments:?} → {reply}");
        assert_eq!(reply["status"], "error", "{arguments:?}");
        let fix = reply["fix"].as_str().unwrap_or("");
        assert!(!fix.is_empty(), "{arguments:?} carried no fix");
        assert_eq!(
            reply["exit"], expected_exit,
            "{arguments:?} did not classify itself"
        );
    }
}

/// The exits each verb publishes must be the ones it can produce.
#[test]
fn every_verb_publishes_its_exit_codes() {
    let schema = schema();
    for verb in ["probe", "tap", "ctl", "serve", "schema"] {
        let exits = schema["contract"]["verbs"][verb]["exits"]
            .as_array()
            .unwrap_or_else(|| panic!("`{verb}` does not publish its exits"));
        assert!(!exits.is_empty(), "`{verb}` publishes an empty exit list");
        for exit in exits {
            let code = exit.as_i64().expect("exit codes are integers");
            assert!(
                [0, 2, 3, 4].contains(&code),
                "`{verb}` publishes exit {code}, which is not in the standard's set"
            );
        }
    }
}

/// A stream line carries `event` **and** the same numbers it always
/// did.
///
/// The first attempt at `event` round-tripped each snapshot through
/// `serde_json::Value` to insert the field, which re-typed every f32
/// as f64: a GPU voltage that had always printed `0.825` started
/// printing `0.824999988079071`. Same value, different bytes, and a
/// consumer diffing or displaying that text would see it change for
/// no reason a user could explain. A field added for
/// self-identification must not rewrite the readings.
#[test]
fn stream_lines_self_identify_without_reformatting_the_numbers() {
    let sandbox = Sandbox::new();
    let output = Command::new(binary())
        .args(["tap", "sensors", "--interval", "0.3"])
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
        .env("XDG_CONFIG_HOME", &sandbox.config_path)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map(|mut child| {
            use std::io::{BufRead, BufReader};
            let stdout = child.stdout.take().expect("piped stdout");
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            // give the sampler a couple of windows
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while line.trim().is_empty() && std::time::Instant::now() < deadline {
                line.clear();
                let _ = reader.read_line(&mut line);
            }
            let _ = child.kill();
            let _ = child.wait();
            line
        })
        .expect("tap produced a line");

    let parsed: Value = serde_json::from_str(output.trim())
        .unwrap_or_else(|error| panic!("stream line is not JSON ({error}): {output}"));
    assert_eq!(parsed["event"], "snapshot", "the line must self-identify");

    // The tell: a float that was exact in the typed serializer comes
    // back with a ~1e-8 tail once it has been through Value. Sensor
    // readings are f32, so any of them is a probe.
    let long_tailed = output
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .filter(|token| {
            token
                .split_once('.')
                .is_some_and(|(_, fraction)| fraction.len() >= 12)
        })
        .count();
    // `ts` legitimately carries many digits; anything beyond a couple
    // of such numbers means the f32s were widened.
    assert!(
        long_tailed <= 3,
        "stream line shows {long_tailed} over-precise numbers — \
         the f32 readings were widened by a Value round-trip: {output}"
    );
}

/// Numbers in a JSON line that are f32 readings printed at f64
/// precision: exactly representable as an f32, yet longer than that
/// f32's shortest form ("0.6909999847412109" is f32 0.691 widened).
/// Values computed in f64 are almost never exactly f32-representable,
/// so they don't trip this — no length heuristic, no allowance.
fn widened_numbers(line: &str) -> Vec<String> {
    line.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == 'E'))
        .filter(|token| token.contains('.'))
        .filter(|token| {
            let Ok(value) = token.parse::<f64>() else { return false };
            let narrow = value as f32;
            // `{:?}` is f32's shortest round-trip form with the ".0"
            // kept ("8.0", "0.691"), the same shape serde prints.
            // (Display drops it — "8" — which flagged every integer.)
            narrow as f64 == value && format!("{narrow:?}").len() < token.len()
        })
        .map(str::to_string)
        .collect()
}

/// F19 (3.1 audit): `probe` printed `"overall_percent":1.6357687711715698`
/// for a reading `tap` printed as `1.5370705`. Every producer must emit
/// the typed serializer's bytes — direct probe, and through a live
/// instance's socket (snapshot and subscribe alike).
#[test]
fn every_producer_prints_f32_readings_exactly() {
    let sandbox = Sandbox::new();
    let direct = Command::new(binary())
        .args(["probe", "cpu", "sensors", "--json"])
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
        .env("XDG_CONFIG_HOME", &sandbox.config_path)
        .output()
        .expect("probe runs");
    let direct = String::from_utf8_lossy(&direct.stdout).to_string();
    assert_eq!(widened_numbers(&direct), Vec::<String>::new(), "direct probe widened f32s");

    // Through a live `serve` on the sandbox socket.
    let mut daemon = Command::new(binary())
        .arg("serve")
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
        .env("XDG_CONFIG_HOME", &sandbox.config_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("serve starts");
    let socket = sandbox.runtime_path.join("sysmon/ctl.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !socket.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let via_socket = Command::new(binary())
        .args(["probe", "cpu", "sensors", "--json"])
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
        .env("XDG_CONFIG_HOME", &sandbox.config_path)
        .output()
        .expect("probe via socket runs");
    let via_socket = String::from_utf8_lossy(&via_socket.stdout).to_string();
    let tapped = {
        use std::io::{BufRead, BufReader};
        let mut child = Command::new(binary())
            .args(["tap", "cpu", "sensors", "--interval", "0.3"])
            .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
            .env("XDG_CONFIG_HOME", &sandbox.config_path)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("tap via socket runs");
        let mut line = String::new();
        let _ = BufReader::new(child.stdout.take().unwrap()).read_line(&mut line);
        let _ = child.kill();
        let _ = child.wait();
        line
    };
    let _ = Command::new(binary())
        .args(["ctl", "quit"])
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
        .env("XDG_CONFIG_HOME", &sandbox.config_path)
        .output();
    let _ = daemon.kill();
    let _ = daemon.wait();

    let parsed: Value = serde_json::from_str(via_socket.trim()).expect("socket probe is JSON");
    assert_eq!(parsed["result"]["via"], "socket", "the second probe really rode the socket");
    assert_eq!(widened_numbers(&via_socket), Vec::<String>::new(), "socket probe widened f32s");
    assert!(tapped.contains("\"event\""), "the socket tap produced a stream line: {tapped}");
    assert_eq!(widened_numbers(&tapped), Vec::<String>::new(), "socket tap widened f32s");
}

#[test]
fn a_full_disk_is_not_a_clean_exit() {
    // Reviewer B #4: only a closed pipe means "enough". ENOSPC on
    // stdout lost the whole answer, and exit 0 told the caller it
    // arrived. It must be a runtime failure (4) with a fix on stderr.
    let sandbox = Sandbox::new();
    let full = std::fs::OpenOptions::new().write(true).open("/dev/full").expect("/dev/full");
    let output = Command::new(binary())
        .args(["probe", "memory", "--json"])
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
        .env("XDG_CONFIG_HOME", &sandbox.config_path)
        .stdout(full)
        .output()
        .expect("probe runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(4), "stdout write failed with ENOSPC; stderr: {stderr}");
    assert!(stderr.contains("fix"), "the failure names its fix: {stderr}");
}

/// F20: a consumer that closes the pipe early is normal (`| head`).
/// The CLI law allows exits 0/2/3/4 — never a panic's 101.
#[test]
fn a_closed_pipe_is_a_clean_exit() {
    let sandbox = Sandbox::new();
    for arguments in [&["probe", "cpu", "--json"][..], &["schema"][..], &["probe", "cpu"][..]] {
        let mut child = Command::new(binary())
            .args(arguments)
            .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
            .env("XDG_CONFIG_HOME", &sandbox.config_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn");
        drop(child.stdout.take()); // the reader is gone before a byte is written
        let output = child.wait_with_output().expect("wait");
        let code = output.status.code().unwrap_or(-1);
        assert!(
            [0, 2, 3, 4].contains(&code),
            "`sysmon {}` exited {code} on a closed pipe: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// F12: per-process CPU% from a 0.25 s window moved in 4% steps
/// (one 10 ms tick / 0.25 s). The one-shot window must be long enough
/// that one tick is ≤ 1% — i.e. ≥ 1 s.
#[test]
fn probe_processes_samples_over_a_full_second() {
    let (reply, code) = Sandbox::new().run(&["probe", "processes", "--json"]);
    assert_eq!(code, 0);
    let window = reply["result"]["interval_seconds"].as_f64().expect("interval_seconds");
    assert!(window >= 0.95, "processes window was {window:.3} s — CPU% quantized to {:.1}% steps", 1.0 / window);
}

/// F21: a one-shot probe can't run nethogs (it needs a long-lived
/// capture), so per-process UDP/QUIC is missing — and it must say so.
#[test]
fn a_one_shot_probe_discloses_its_network_coverage() {
    let (reply, code) = Sandbox::new().run(&["probe", "network", "--json"]);
    assert_eq!(code, 0);
    let network = &reply["result"]["network"];
    if network["process_source"] == "tcp_diag" {
        let hint = network["process_source_hint"].as_str().unwrap_or("");
        assert!(hint.contains("serve"), "tcp_diag-only probe must point at `sysmon serve`, got {hint:?}");
    }
}

/// A freshly started `serve` answered its first snapshot with a 0 s
/// window, so every rate in it (CPU %, network, disk) was a false
/// zero. The first answer must carry a real window, like a direct
/// probe's.
#[test]
fn a_fresh_serve_never_answers_with_an_empty_window() {
    let sandbox = Sandbox::new();
    let mut daemon = Command::new(binary())
        .arg("serve")
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
        .env("XDG_CONFIG_HOME", &sandbox.config_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("serve starts");
    let socket = sandbox.runtime_path.join("sysmon/ctl.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !socket.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let (reply, code) = sandbox.run(&["probe", "cpu", "--json"]);
    let _ = sandbox.run(&["ctl", "quit", "--json"]);
    let _ = daemon.wait();
    assert_eq!(code, 0);
    assert_eq!(reply["result"]["via"], "socket", "rode the fresh serve");
    let window = reply["result"]["interval_seconds"].as_f64().unwrap_or(0.0);
    assert!(window >= 0.2, "first answer from serve spans {window} s — its rates are all zero");
}

/// Serve kept warm by one client (a `tap network`) must still give a
/// second client's first ask for *other* sections a real window, and a
/// section last read long ago must not answer with that long average
/// under a short label (Reviewer B, P1). The check: a spinning `sh`
/// started just before the ask shows as busy, and overall CPU is not 0.
#[test]
fn a_warm_serve_measures_each_section_over_its_own_window() {
    let sandbox = Sandbox::new();
    let env = |command: &mut Command| {
        command
            .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
            .env("XDG_CONFIG_HOME", &sandbox.config_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    };
    let mut serve = Command::new(binary());
    serve.arg("serve");
    env(&mut serve);
    let daemon = GroupGuard::spawn(serve);
    let socket = sandbox.runtime_path.join("sysmon/ctl.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !socket.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let mut tap = Command::new(binary());
    tap.args(["tap", "network", "-i", "0.3"]);
    env(&mut tap);
    let warm = GroupGuard::spawn(tap);
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let mut spin_command = Command::new("sh");
    spin_command.args(["-c", "while :; do :; done"]);
    let spinner = GroupGuard::spawn(spin_command);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (reply, code) = sandbox.run(&["probe", "cpu", "processes", "--json"]);
    let spinner_pid = spinner.pid();
    drop(spinner);
    drop(warm);
    let _ = sandbox.run(&["ctl", "quit", "--json"]);
    drop(daemon);

    assert_eq!(code, 0);
    assert_eq!(reply["result"]["via"], "socket", "rode the warm serve");
    let overall = reply["result"]["cpu"]["overall_percent"].as_f64().unwrap_or(0.0);
    let spinner_cpu = reply["result"]["processes"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["pid"].as_i64() == Some(spinner_pid as i64)))
        .and_then(|row| row["cpu_percent"].as_f64());
    assert!(
        overall > 0.5 && spinner_cpu.is_some_and(|cpu| cpu > 50.0),
        "first cpu+processes ask on a warm serve: overall {overall}%, spinner {spinner_cpu:?}% \
         (window {})",
        reply["result"]["interval_seconds"]
    );
}

/// Every field a real probe emits is named in `schema.sections`. A
/// field an agent can see but can't look up is a field it will guess
/// about (3.1 added a dozen; this keeps the map from drifting again).
#[test]
fn every_live_field_is_documented_in_the_schema() {
    let schema = schema();
    let sections = &schema["sections"];
    let documented = |section: &str| -> String { sections[section].to_string() };
    let (probe, code) = Sandbox::new().run(&["probe", "all", "--json"]);
    assert_eq!(code, 0);
    let result = &probe["result"];
    let mut missing = Vec::new();
    let mut check = |section: &str, object: &Value| {
        let text = documented(section);
        if let Some(fields) = object.as_object() {
            for key in fields.keys() {
                if !text.contains(key.as_str()) {
                    missing.push(format!("{section}.{key}"));
                }
            }
        }
    };
    for section in ["system", "cpu", "memory", "gpu", "network"] {
        check(section, &result[section]);
    }
    check("memory", &result["memory"]["modules"][0]);
    check("network", &result["network"]["interfaces"][0]);
    check("disks", &result["disks"][0]);
    check("processes", &result["processes"][0]);
    if let Some(chips) = result["sensors"]["chips"].as_array() {
        for chip in chips {
            check("sensors", chip);
            for list in ["temps", "fans", "voltages", "power"] {
                if let Some(first) = chip[list].get(0) {
                    check("sensors", first);
                }
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(missing.is_empty(), "live fields the schema never names: {missing:?}");
}

/// `--background` runs the GUI on a private Xvfb display. It must work
/// when launched from a Wayland session (the inherited WAYLAND_DISPLAY
/// made winit dial a compositor that isn't there and exit 4), and it
/// must quit cleanly (an inherited LD_LIBRARY_PATH from a custom
/// compositor build mixed two libxkbcommons and segfaulted on exit).
#[test]
fn background_mode_starts_and_quits_cleanly_from_a_wayland_session() {
    if Command::new("xvfb-run").arg("--help").output().is_err() {
        eprintln!("xvfb-run not installed — skipping");
        return;
    }
    let sandbox = Sandbox::new();
    let mut command = Command::new(binary());
    command
        .arg("--background")
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
        .env("XDG_CONFIG_HOME", &sandbox.config_path)
        // What a launcher in a Wayland session hands down.
        .env("WAYLAND_DISPLAY", "wayland-1")
        .env("XDG_SESSION_TYPE", "wayland")
        .env("LD_LIBRARY_PATH", "/opt/swayfx-ux/lib/x86_64-linux-gnu:/opt/swayfx-ux/lib")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut gui = GroupGuard::spawn(command);
    let gui = &mut gui.child;
    let socket = sandbox.runtime_path.join("sysmon/ctl.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !socket.exists() && std::time::Instant::now() < deadline {
        if let Ok(Some(status)) = gui.try_wait() {
            let mut stderr = String::new();
            use std::io::Read;
            let _ = gui.stderr.take().unwrap().read_to_string(&mut stderr);
            panic!("--background exited {status} before serving: {stderr}");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let (status, code) = sandbox.run(&["ctl", "status", "--json"]);
    assert_eq!(code, 0, "{status}");
    assert_eq!(status["result"]["mode"], "gui");
    let (_, code) = sandbox.run(&["ctl", "quit", "--json"]);
    assert_eq!(code, 0);
    let status = gui.wait().expect("wait");
    let mut stderr = String::new();
    if let Some(mut pipe) = gui.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "--background exited {status:?} on quit: {stderr}"
    );
}

/// `--background` forks xvfb-run → Xvfb + the GUI. A failing test that
/// only killed its direct child left both orphaned to init. The guard
/// starts the tree in its own process group and kills the group on
/// drop — pass, fail or panic.
struct GroupGuard {
    child: std::process::Child,
}

impl GroupGuard {
    fn spawn(mut command: Command) -> GroupGuard {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        GroupGuard {
            child: command.spawn().expect("spawn --background"),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        let group = self.child.id() as i32;
        // Only signal a group that still exists (a clean quit already
        // reaped it).
        unsafe {
            if libc::kill(-group, 0) == 0 {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        let _ = self.child.wait();
    }
}

/// The GUI binds the socket before its window exists. A `quit` sent in
/// that gap replied `"quitting": true` and was then dropped: the window
/// lived on (found as orphaned test GUIs). A quit is a quit.
#[test]
fn a_quit_during_gui_startup_is_honored() {
    if Command::new("xvfb-run").arg("--help").output().is_err() {
        eprintln!("xvfb-run not installed — skipping");
        return;
    }
    let sandbox = Sandbox::new();
    let mut command = Command::new(binary());
    command
        .arg("--background")
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_path)
        .env("XDG_CONFIG_HOME", &sandbox.config_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut guard = GroupGuard::spawn(command);
    let gui = &mut guard.child;
    let socket = sandbox.runtime_path.join("sysmon/ctl.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !socket.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // The instant the socket exists: no waiting for the window.
    let (_, code) = sandbox.run(&["ctl", "quit", "--json"]);
    assert_eq!(code, 0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(status) = gui.try_wait().expect("try_wait") {
            assert!(status.success(), "GUI exited {status} after an early quit");
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("GUI still running 20 s after `quit` was acknowledged");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
