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
pub mod net_process;
pub mod process;
pub(crate) mod read;
pub mod sensors;

use std::time::Instant;

use crate::snapshot::{SystemInfo, SystemSnapshot, Wants};

use read::read_trimmed;

/// How a long-lived sampler differs from a one-shot one.
#[derive(Clone, Copy, Debug)]
pub struct SamplerOptions {
    /// Keep a nethogs child for full-protocol per-process rates.
    /// One-shot probes pass false (never leave children behind).
    pub enable_nethogs: bool,
}

impl Default for SamplerOptions {
    fn default() -> Self {
        SamplerOptions {
            enable_nethogs: true,
        }
    }
}

pub struct Sampler {
    previous_sample_at: Option<Instant>,
    cpu: cpu::CpuCollector,
    memory: memory::MemoryCollector,
    gpu: gpu::GpuCollector,
    disk: disk::DiskCollector,
    net: net::NetCollector,
    net_process: net_process::NetProcessCollector,
    process: process::ProcessCollector,
    sensors: sensors::SensorsCollector,
    boot_ts: f64,
}

impl Sampler {
    pub fn new() -> Self {
        Self::with_options(SamplerOptions::default())
    }

    pub fn with_options(options: SamplerOptions) -> Self {
        let mut cpu = cpu::CpuCollector::new();
        let boot_ts = cpu.boot_ts();
        Sampler {
            previous_sample_at: None,
            cpu,
            memory: memory::MemoryCollector,
            gpu: gpu::GpuCollector::new(),
            disk: disk::DiskCollector::new(),
            net: net::NetCollector::new(),
            net_process: net_process::NetProcessCollector::new(options.enable_nethogs, 1),
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
            snapshot.network = Some(self.net.collect(interval_seconds));
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
        if wants.per_process_net || wants.connections {
            let net_sample = self
                .net_process
                .collect(interval_seconds, wants.connections);
            if let Some(network) = &mut snapshot.network {
                network.process_source = net_sample.source;
                network.process_source_hint = net_sample.hint.clone();
                network.top_processes = net_sample.top.clone();
            }
            if let Some(records) = &mut snapshot.processes {
                for record in records.iter_mut() {
                    if let Some((rx, tx)) = net_sample.rates_by_pid.get(&record.pid) {
                        record.net_rx_bps = Some(*rx);
                        record.net_tx_bps = Some(*tx);
                    }
                }
            }
            if wants.connections {
                snapshot.connections = Some(net_sample.connections);
            }
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
