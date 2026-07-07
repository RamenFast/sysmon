// SPDX-License-Identifier: GPL-3.0-or-later
//! Tiny /proc & /sys read helpers. Everything returns Option — an
//! unreadable file is a normal Tuesday in procfs (a process exited,
//! a driver doesn't expose the node) and must never take a collector
//! down.

use std::fs;
use std::path::Path;

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
