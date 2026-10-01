// SPDX-License-Identifier: GPL-3.0-or-later
//! The one-shot envelope — the phosphor contract, verbatim:
//! `{"status","tool","version","ts",…}` around every reply, errors
//! ALWAYS carrying a `fix`, JSON auto-on when stdout is a pipe.
//! Exit codes: 0 ok · 2 unavailable · 3 bad args · 4 runtime.
//!
//! `ts` is ISO-8601 with a UTC offset (workspace standard ruling R1).
//! The epoch float rides along as `ts_epoch` for arithmetic — an
//! extra never replaces the canonical field.

use std::io::IsTerminal;

use serde_json::{Value, json};

pub const EXIT_OK: i32 = 0;
pub const EXIT_UNAVAILABLE: i32 = 2;
pub const EXIT_BAD_ARGS: i32 = 3;
pub const EXIT_RUNTIME: i32 = 4;

/// The tool name, one place, so the envelope can never drift from it.
pub const TOOL: &str = "sysmon";

pub fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Unix seconds → `2026-08-01T22:14:07+00:00`.
///
/// Hand-rolled civil-from-days (Howard Hinnant's algorithm) rather
/// than a date crate: the whole need is one formatter, and a
/// dependency is a promise to maintain. UTC always, so the offset is
/// literal — a reader never has to guess the machine's zone.
pub fn iso8601(epoch_seconds: f64) -> String {
    let total = epoch_seconds.floor() as i64;
    let days = total.div_euclid(86_400);
    let seconds_of_day = total.rem_euclid(86_400);
    let (hour, minute, second) = (
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    );

    // days since 1970-01-01 → civil y/m/d
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+00:00")
}

/// The four fields every one-shot carries, on ok and on error alike.
fn envelope_head(status: &str) -> serde_json::Map<String, Value> {
    let epoch = now_ts();
    let mut head = serde_json::Map::new();
    head.insert("status".to_string(), json!(status));
    head.insert("tool".to_string(), json!(TOOL));
    head.insert("version".to_string(), json!(sysmon_core::VERSION));
    head.insert("ts".to_string(), json!(iso8601(epoch)));
    head.insert("ts_epoch".to_string(), json!(epoch));
    head
}

pub fn ok(result: Value) -> Value {
    let mut envelope = envelope_head("ok");
    envelope.insert("result".to_string(), result);
    Value::Object(envelope)
}

/// An error envelope. `exit` names the exit code this failure means,
/// so a caller reading the wire (the socket, where there is no
/// process to exit) classifies it exactly as the CLI does.
pub fn error_coded(message: impl Into<String>, fix: impl Into<String>, exit: i32) -> Value {
    let mut envelope = envelope_head("error");
    envelope.insert("error".to_string(), json!(message.into()));
    envelope.insert("fix".to_string(), json!(fix.into()));
    envelope.insert("exit".to_string(), json!(exit));
    Value::Object(envelope)
}

/// The common case: a malformed request is bad arguments.
pub fn error(message: impl Into<String>, fix: impl Into<String>) -> Value {
    error_coded(message, fix, EXIT_BAD_ARGS)
}

/// JSON output is on when forced, or whenever stdout isn't a
/// terminal (agents pipe; humans get prose).
pub fn json_wanted(force_json: bool) -> bool {
    force_json || !std::io::stdout().is_terminal()
}

/// Print one envelope as a single NDJSON line.
pub fn emit(envelope: &Value) {
    print_line(&envelope.to_string());
}

/// Every stdout line the CLI writes goes through here. `println!`
/// panics when the reader has gone (`sysmon probe | head -c 0`), and a
/// panic exits 101, which no caller can classify (3.1 audit F20). A
/// closed pipe is a normal way for a consumer to say "enough", so it
/// ends the process with 0.
pub fn print_line(line: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if writeln!(lock, "{line}").and_then(|()| lock.flush()).is_err() {
        std::process::exit(EXIT_OK);
    }
}

/// Emit an error envelope (stderr gets the human reading when
/// stdout is a terminal) and hand back the exit code.
pub fn fail(message: impl Into<String>, fix: impl Into<String>, code: i32) -> i32 {
    fail_forced(message, fix, code, false)
}

/// `fail`, honoring an explicit `--json` on the failing call.
///
/// The auto-switch alone is not enough on an error path: an agent on
/// a pty that passes `--json` was still getting prose on stderr and
/// nothing parseable on stdout, so the one case where a caller most
/// needs a machine answer — it went wrong — was the case that didn't
/// give one.
pub fn fail_forced(
    message: impl Into<String>,
    fix: impl Into<String>,
    code: i32,
    force_json: bool,
) -> i32 {
    let message = message.into();
    let fix = fix.into();
    if json_wanted(force_json) {
        emit(&error_coded(&message, &fix, code));
    } else {
        eprintln!("sysmon: {message}");
        eprintln!("fix: {fix}");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_matches_known_instants() {
        assert_eq!(iso8601(0.0), "1970-01-01T00:00:00+00:00");
        assert_eq!(iso8601(1_000_000_000.0), "2001-09-09T01:46:40+00:00");
        // a leap-year February, the classic off-by-one
        assert_eq!(iso8601(1_709_164_800.0), "2024-02-29T00:00:00+00:00");
        assert_eq!(iso8601(1_785_621_938.98), "2026-08-01T22:05:38+00:00");
    }

    #[test]
    fn every_envelope_carries_the_four_fields() {
        for envelope in [ok(json!({})), error("boom", "unboom")] {
            for field in ["status", "tool", "version", "ts"] {
                assert!(envelope.get(field).is_some(), "missing {field}");
            }
            assert_eq!(envelope["tool"], "sysmon");
        }
    }

    #[test]
    fn errors_always_carry_a_fix_and_an_exit_code() {
        let envelope = error_coded("boom", "unboom", EXIT_UNAVAILABLE);
        assert_eq!(envelope["status"], "error");
        assert_eq!(envelope["fix"], "unboom");
        assert_eq!(envelope["exit"], EXIT_UNAVAILABLE);
    }

    #[test]
    fn ts_is_iso8601_with_an_offset() {
        let ts = ok(json!({}))["ts"].as_str().unwrap().to_string();
        assert!(ts.contains('T'), "{ts}");
        assert!(ts.ends_with("+00:00"), "{ts}");
        assert_eq!(ts.len(), 25, "{ts}");
    }
}
