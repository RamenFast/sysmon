// SPDX-License-Identifier: GPL-3.0-or-later
//! sysmon-core — the engine. Everything that *knows* system state lives
//! here: /proc and /sys collectors with their delta bookkeeping, the
//! serde snapshot model (which IS the wire contract `sysmon probe`
//! serves), unit formatting, and the desktop-entry/icon index. No UI
//! dependencies; the GUI, the CLI, and the socket server are all just
//! clients of [`collect::Sampler`].

pub mod apps;
pub mod collect;
pub mod snapshot;
pub mod units;

/// One version string everywhere: workspace == --version == packages.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
