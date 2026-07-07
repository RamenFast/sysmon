// SPDX-License-Identifier: GPL-3.0-or-later
//! The sampler: owns every collector's delta state and produces
//! [`SystemSnapshot`]s on demand. Each subsystem is collected
//! independently, so one unreadable file never takes down the rest;
//! [`Wants`] keeps cheap queries cheap (`tap network` never pays for
//! a process scan).

pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod memory;
pub mod net;
pub mod process;
pub(crate) mod read;
pub mod sensors;

use std::time::Instant;

use crate::snapshot::{ProcessNetSource, SystemInfo, SystemSnapshot, Wants};

use read::read_trimmed;

pub struct Sampler {
    previous_sample_at: Option<Instant>,
    cpu: cpu::CpuCollector,
    memory: memory::MemoryCollector,
    gpu: gpu::GpuCollector,
    disk: disk::DiskCollector,
    net: net::NetCollector,
    process: process::ProcessCollector,
    sensors: sensors::SensorsCollector,
    boot_ts: f64,
}

impl Sampler {
    pub fn new() -> Self {
        let mut cpu = cpu::CpuCollector::new();
        let boot_ts = cpu.boot_ts();
        Sampler {
            previous_sample_at: None,
            cpu,
            memory: memory::MemoryCollector,
            gpu: gpu::GpuCollector::new(),
            disk: disk::DiskCollector::new(),
            net: net::NetCollector::new(),
            process: process::ProcessCollector::new(),
            sensors: sensors::SensorsCollector,
            boot_ts,
        }
    }

    pub fn gpu_available(&self) -> bool {
        self.gpu.is_available()
    }

    /// Collect one snapshot of the requested sections. Rates cover
    /// the window since the previous `sample()` call on this sampler
    /// (0 on the first call — callers wanting instant rates sample
    /// twice, e.g. `probe`'s two-sample mode).
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

        if wants.system {
            snapshot.system = Some(self.system_info());
        }
        if wants.sensors || wants.cpu {
            snapshot.sensors = Some(self.sensors.collect());
        }
        if wants.cpu {
            let mut cpu = self.cpu.collect(interval_seconds);
            cpu.temperature_celsius = snapshot
                .sensors
                .as_ref()
                .and_then(sensors::SensorsCollector::cpu_temperature);
            snapshot.cpu = Some(cpu);
        }
        if !wants.sensors {
            snapshot.sensors = None; // was only borrowed for the cpu temp
        }
        if wants.memory {
            snapshot.memory = Some(self.memory.collect());
        }
        if wants.gpu {
            snapshot.gpu = Some(self.gpu.collect(interval_seconds));
        }
        if wants.network {
            let mut network = self.net.collect(interval_seconds);
            // Per-process attribution lands in wave 4; until then the
            // section says so honestly instead of silently showing
            // nothing.
            network.process_source = ProcessNetSource::None;
            network.process_source_hint =
                Some("per-process network attribution lands in wave 4".to_string());
            snapshot.network = Some(network);
        }
        if wants.disks {
            snapshot.disks = Some(self.disk.collect(interval_seconds));
        }
        if wants.processes {
            let mut records = self.process.collect(interval_seconds, self.boot_ts);
            // Merge per-process GPU usage into the process records.
            if let Some(gpu) = &snapshot.gpu {
                for usage in &gpu.processes {
                    if let Some(record) = records.iter_mut().find(|r| r.pid == usage.pid) {
                        record.gpu_busy_percent = usage.busy_percent;
                        record.gpu_vram_bytes = usage.vram_bytes;
                    }
                }
            }
            snapshot.processes = Some(records);
        }

        snapshot
    }

    fn system_info(&self) -> SystemInfo {
        SystemInfo {
            hostname: read_trimmed("/proc/sys/kernel/hostname").unwrap_or_default(),
            kernel: read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_default(),
            uptime_seconds: read_trimmed("/proc/uptime")
                .and_then(|content| {
                    content
                        .split_ascii_whitespace()
                        .next()
                        .and_then(|v| v.parse().ok())
                })
                .unwrap_or(0.0),
            boot_ts: self.boot_ts,
        }
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
