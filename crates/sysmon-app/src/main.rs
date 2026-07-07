// SPDX-License-Identifier: GPL-3.0-or-later
//! `sysmon` — one binary, subcommand-first, GUI as the default
//! command (the phosphor contract). Agent surface: `probe` (one-shot
//! state), `tap` (streaming NDJSON), `ctl` (drive the running GUI),
//! `schema` (the machine map), `serve` (headless sampling daemon),
//! `--background` (GUI on a private Xvfb display). All agent-grade:
//! JSON envelopes, errors that carry a `fix`, exit codes 0/2/3/4.

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let first = arguments.first().map(String::as_str);
    let code = match first {
        Some("--version") | Some("-V") => {
            println!("sysmon {} (v2)", sysmon_core::VERSION);
            0
        }
        Some("--help") | Some("-h") | Some("help") => {
            print_help();
            0
        }
        Some(other) => {
            eprintln!("sysmon: unknown command `{other}` (wave 1 scaffold — verbs land next)");
            eprintln!("fix: run `sysmon --help` for what exists right now");
            3
        }
        None => {
            eprintln!("sysmon: the GUI lands in wave 6 of the v2 rewrite");
            eprintln!("fix: `sysmon --version` works today; check back after the build");
            2
        }
    };
    ExitCode::from(code as u8)
}

fn print_help() {
    println!(
        "sysmon {} — compact system monitor with an agent-drivable API

USAGE
    sysmon                 launch the GUI (default)
    sysmon probe [section] one-shot system state as JSON
    sysmon tap [section]   stream NDJSON samples
    sysmon ctl <verb>      drive the running instance
    sysmon serve           headless sampling daemon
    sysmon schema          machine-readable map of all of the above
    sysmon --background    GUI on a private Xvfb display
    sysmon --version       print the version",
        sysmon_core::VERSION
    );
}
