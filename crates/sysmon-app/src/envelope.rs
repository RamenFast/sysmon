// SPDX-License-Identifier: GPL-3.0-or-later
//! The one-shot envelope — the phosphor contract, verbatim:
//! `{"status","tool","version","ts",…}` around every reply, errors
//! ALWAYS carrying a `fix`, JSON auto-on when stdout is a pipe.
//! Exit codes: 0 ok · 2 unavailable · 3 bad args · 4 runtime.

use std::io::IsTerminal;

use serde_json::{Value, json};

pub const EXIT_OK: i32 = 0;
pub const EXIT_UNAVAILABLE: i32 = 2;
pub const EXIT_BAD_ARGS: i32 = 3;
pub const EXIT_RUNTIME: i32 = 4;

pub fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn ok(result: Value) -> Value {
    json!({
        "status": "ok",
        "tool": "sysmon",
        "version": sysmon_core::VERSION,
        "ts": now_ts(),
        "result": result,
    })
}

pub fn error(message: impl Into<String>, fix: impl Into<String>) -> Value {
    json!({
        "status": "error",
        "tool": "sysmon",
        "version": sysmon_core::VERSION,
        "ts": now_ts(),
        "error": message.into(),
        "fix": fix.into(),
    })
}

/// JSON output is on when forced, or whenever stdout isn't a
/// terminal (agents pipe; humans get prose).
pub fn json_wanted(force_json: bool) -> bool {
    force_json || !std::io::stdout().is_terminal()
}

/// Print one envelope as a single NDJSON line.
pub fn emit(envelope: &Value) {
    println!("{envelope}");
}

/// Emit an error envelope (stderr gets the human reading when
/// stdout is a terminal) and hand back the exit code.
pub fn fail(message: impl Into<String>, fix: impl Into<String>, code: i32) -> i32 {
    let message = message.into();
    let fix = fix.into();
    if json_wanted(false) {
        emit(&error(&message, &fix));
    } else {
        eprintln!("sysmon: {message}");
        eprintln!("fix: {fix}");
    }
    code
}
