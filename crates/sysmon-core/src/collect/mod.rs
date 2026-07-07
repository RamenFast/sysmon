// SPDX-License-Identifier: GPL-3.0-or-later
//! The sampler: owns every collector's delta state and produces
//! [`SystemSnapshot`]s on demand. Collectors land wave by wave; each
//! is independent so one unreadable file never takes down the rest.

use std::time::Instant;

use crate::snapshot::{SystemSnapshot, Wants};

pub struct Sampler {
    previous_sample_at: Option<Instant>,
}

impl Sampler {
    pub fn new() -> Self {
        Sampler {
            previous_sample_at: None,
        }
    }

    /// Collect one snapshot of the requested sections. Rates cover
    /// the window since the previous `sample()` call on this sampler
    /// (0 on the first call).
    pub fn sample(&mut self, wants: Wants) -> SystemSnapshot {
        let now = Instant::now();
        let interval_seconds = self
            .previous_sample_at
            .map(|previous| now.duration_since(previous).as_secs_f64())
            .unwrap_or(0.0);
        self.previous_sample_at = Some(now);

        let mut snapshot = SystemSnapshot {
            ts: unix_now(),
            interval_seconds,
            ..Default::default()
        };
        let _ = wants; // collectors land in wave 2
        snapshot.ts = snapshot.ts.max(0.0);
        snapshot
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
