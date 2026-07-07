// SPDX-License-Identifier: GPL-3.0-or-later
//! `sysmon` — one binary, subcommand-first, GUI as the default
//! command (the phosphor contract). Agent surface: `probe` (one-shot
//! state), `tap` (streaming NDJSON), `ctl` (drive the running GUI),
//! `schema` (the machine map), `serve` (headless sampling daemon),
//! `--background` (GUI on a private Xvfb display). All agent-grade:
//! JSON envelopes, errors that carry a `fix`, exit codes 0/2/3/4.

pub mod agent;
pub mod control;
pub mod envelope;
pub mod gui;
pub mod serve;

/// The whole CLI: dispatch and exit code. `main` is one line over
/// this so integration tests can reach every module.
pub fn run_cli() -> i32 {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let first = arguments.first().map(String::as_str);
    match first {
        Some("--version") | Some("-V") => {
            println!("sysmon {} (v2)", sysmon_core::VERSION);
            0
        }
        Some("--help") | Some("-h") | Some("help") => {
            print_help();
            0
        }
        Some("probe") => agent::run_probe(&arguments[1..]),
        Some("tap") => agent::run_tap(&arguments[1..]),
        Some("ctl") => agent::run_ctl(&arguments[1..]),
        Some("schema") => agent::run_schema(&arguments[1..]),
        Some("serve") => serve::run(&arguments[1..]),
        None => gui::run(&arguments),
        Some("--background") => run_background(&arguments),
        Some(other) => {
            eprintln!("sysmon: unknown command `{other}`");
            eprintln!("fix: run `sysmon --help`");
            3
        }
    }
}

/// `--background`: re-exec the GUI under a private Xvfb display so it
/// renders (and serves the control socket) without ever mapping a
/// window on the user's screen — no focus steal, game-safe. The env
/// guard stops recursion once we're inside the virtual display.
/// (phosphor's exact pattern.)
fn run_background(arguments: &[String]) -> i32 {
    if std::env::var_os("SYSMON_BACKGROUND").is_some() {
        // Already wrapped: fall through to the GUI.
        let rest: Vec<String> = arguments
            .iter()
            .filter(|a| a.as_str() != "--background")
            .cloned()
            .collect();
        return gui::run(&rest);
    }
    let self_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("sysmon --background: cannot find own binary: {error}");
            return 4;
        }
    };
    let rest: Vec<&String> = arguments.iter().filter(|a| a.as_str() != "--background").collect();
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new("xvfb-run")
        .arg("-a")
        .args(["-s", "-screen 0 1280x900x24"])
        .arg(self_exe)
        .arg("--background")
        .args(rest)
        .env("SYSMON_BACKGROUND", "1")
        .exec();
    eprintln!("sysmon --background: could not launch xvfb-run: {error}");
    eprintln!("fix: install it (sudo apt install xvfb), or run sysmon on your display normally");
    2
}

fn print_help() {
    println!(
        "sysmon {} — compact system monitor with an agent-drivable API

USAGE
    sysmon                          launch the GUI (default; re-launch raises it)
    sysmon --background             GUI on a private Xvfb display (no focus steal)
    sysmon probe [sections…]        one-shot system state (JSON when piped)
    sysmon tap [sections…] [-i N]   stream NDJSON snapshots (default: network, 1s)
    sysmon ctl <verb> […]           drive the running instance
    sysmon serve                    headless daemon on the control socket
    sysmon schema                   machine-readable map of all of the above
    sysmon --version                print the version

SECTIONS
    all system cpu memory gpu network disks processes sensors connections

CTL VERBS
    status quit pause resume interval <s> raise page <p> theme <t>
    palette <p> popout <section> popin <section> shot [path]
    compact <on|off> units <decimal|binary>

Everything exits 0 on success, 2 when a needed piece isn't running,
3 on bad arguments, 4 on runtime failure. Piped output is always
JSON envelopes; errors always carry a `fix`.",
        sysmon_core::VERSION
    );
}
