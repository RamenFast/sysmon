// SPDX-License-Identifier: GPL-3.0-or-later
//! Tiny /proc & /sys read helpers. Everything returns Option — an
//! unreadable file is a normal Tuesday in procfs (a process exited,
//! a driver doesn't expose the node) and must never take a collector
//! down.

use std::fs;
use std::path::Path;
use std::time::Instant;

/// Per-collector measurement window. Every rate-producing collector
/// owns one so its denominators always span *its own* previous
/// collection — two API clients sampling different sections can
/// never corrupt each other's rates.
#[derive(Default)]
pub(crate) struct SelfInterval {
    previous: Option<Instant>,
}

impl SelfInterval {
    /// Elapsed seconds since this collector last ticked (0 first time).
    pub fn tick(&mut self, now: Instant) -> f64 {
        let elapsed = self
            .previous
            .map(|previous| now.duration_since(previous).as_secs_f64())
            .unwrap_or(0.0);
        self.previous = Some(now);
        elapsed
    }
}

pub fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

pub fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

pub fn read_f64(path: impl AsRef<Path>) -> Option<f64> {
    read_trimmed(path)?.parse().ok()
}
