// SPDX-License-Identifier: GPL-3.0-or-later
//! `sysmon` — one binary, subcommand-first, GUI as the default
//! command (the phosphor contract). Agent surface: `probe` (one-shot
//! state), `tap` (streaming NDJSON), `ctl` (drive the running GUI),
//! `schema` (the machine map), `serve` (headless sampling daemon),
//! `--background` (GUI on a private Xvfb display). All agent-grade:
//! JSON envelopes, errors that carry a `fix`, exit codes 0/2/3/4.

use std::process::ExitCode;

mod agent;
mod control;
mod envelope;
mod gui;
mod serve;

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
        Some("probe") => agent::run_probe(&arguments[1..]),
        Some("tap") => agent::run_tap(&arguments[1..]),
        Some("ctl") => agent::run_ctl(&arguments[1..]),
        Some("schema") => agent::run_schema(&arguments[1..]),
        Some("serve") => serve::run(&arguments[1..]),
        None => gui::run(&arguments),
        Some("--background") => {
            eprintln!("sysmon: --background lands in wave 7 (xvfb-run re-exec)");
            eprintln!("fix: run `sysmon` on a display, or `sysmon serve` headless");
            2
        }
        Some(other) => {
            eprintln!("sysmon: unknown command `{other}`");
            eprintln!("fix: run `sysmon --help`");
            3
        }
    };
    ExitCode::from(code as u8)
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
