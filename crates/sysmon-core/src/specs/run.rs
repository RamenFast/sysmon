// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded helper commands. A spec sheet asks a few optional tools
//! (vulkaninfo, glxinfo, swaymsg, pactl) for what sysfs cannot say.
//! Each runs with a deadline and stdin closed, and its absence is a
//! missing line, never a failure: a machine without vulkaninfo still
//! gets a whole spec sheet.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one helper may take. vulkaninfo on a cold driver is the
/// slowest seen (≈0.15 s here); a hung GPU stack must not hang the
/// copy button.
pub const HELPER_DEADLINE: Duration = Duration::from_secs(3);

/// Output cap: these tools print kilobytes; anything past this is a
/// tool misbehaving, and the parsers only need the head.
const OUTPUT_CAP_BYTES: usize = 512 * 1024;

/// stdout of `program args…` when it exits 0 within `deadline`.
/// None when the program is missing, fails, or overruns (then it is
/// killed and reaped — a spec sheet never leaves children behind).
pub fn run_bounded(program: &str, args: &[&str], deadline: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Drain on a thread so a chatty tool can't block on a full pipe
    // while we wait for it to exit.
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = (&mut stdout).take(OUTPUT_CAP_BYTES as u64).read_to_end(&mut buffer);
        buffer
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if started.elapsed() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(Some(_)) => return None,
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                // The reader is not joined: a grandchild may still
                // hold the pipe open, and waiting on it would undo
                // the deadline. The thread ends when the pipe closes.
                return None;
            }
        }
    }
    let output = reader.join().ok()?;
    Some(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_program_is_none_not_a_panic() {
        assert_eq!(run_bounded("sysmon-no-such-helper", &[], HELPER_DEADLINE), None);
    }

    #[test]
    fn output_comes_back_and_failure_is_none() {
        assert_eq!(run_bounded("sh", &["-c", "printf hi"], HELPER_DEADLINE).as_deref(), Some("hi"));
        assert_eq!(run_bounded("sh", &["-c", "printf hi; exit 1"], HELPER_DEADLINE), None);
    }

    #[test]
    fn an_overrunning_helper_is_killed_at_the_deadline() {
        let started = Instant::now();
        assert_eq!(run_bounded("sleep", &["5"], Duration::from_millis(200)), None);
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
    }
}
