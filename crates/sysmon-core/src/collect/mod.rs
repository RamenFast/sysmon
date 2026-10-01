// SPDX-License-Identifier: GPL-3.0-or-later
//! The sampler: owns every collector's delta state and produces
//! [`SystemSnapshot`]s on demand. Each subsystem is collected
//! independently, so one unreadable file never takes down the rest;
//! [`Wants`] keeps cheap queries cheap (`tap network` never pays for
//! a process scan).

pub mod cpu;
pub mod detail;
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
    /// When each rate-bearing section last ran: every collector keeps
    /// its own window, so a sampler shared by clients asking for
    /// different sections has a different window per section.
    section_sampled_at: [Option<Instant>; RATE_SECTIONS],
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

/// cpu, gpu, network, disks, processes, per-process network.
const RATE_SECTIONS: usize = 6;

fn rate_sections(wants: Wants) -> [bool; RATE_SECTIONS] {
    [
        wants.cpu,
        wants.gpu,
        wants.network,
        wants.disks,
        wants.processes,
        wants.per_process_net || wants.connections,
    ]
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
            section_sampled_at: [None; RATE_SECTIONS],
            cpu,
            memory: memory::MemoryCollector::new(),
            gpu: gpu::GpuCollector::new(),
            disk: disk::DiskCollector::new(),
            net: net::NetCollector::new(),
            net_process: net_process::NetProcessCollector::new(options.enable_nethogs, 1),
            process: process::ProcessCollector::new(),
            sensors: sensors::SensorsCollector::new(),
            boot_ts,
        }
    }

    pub fn gpu_available(&self) -> bool {
        self.gpu.is_available()
    }

    /// The window the wanted sections would share if sampled now: the
    /// seconds since they were last sampled, when that was together
    /// (within 10%). None when one was never sampled or they drifted
    /// apart (other clients asked for some of them more recently), so a
    /// shared sampler knows to open a fresh common window first.
    pub fn common_window(&self, wants: Wants) -> Option<f64> {
        let now = Instant::now();
        let mut ages = Vec::new();
        for (wanted, sampled_at) in rate_sections(wants).iter().zip(&self.section_sampled_at) {
            if *wanted {
                ages.push(now.duration_since((*sampled_at)?).as_secs_f64());
            }
        }
        let Some(oldest) = ages.iter().copied().reduce(f64::max) else {
            // No rate section wanted: any window is the window.
            return self.previous_sample_at.map(|at| now.duration_since(at).as_secs_f64());
        };
        let newest = ages.iter().copied().reduce(f64::min).unwrap_or(oldest);
        (newest >= oldest * 0.9).then_some(oldest)
    }

    /// Collect one snapshot of the requested sections. Rates cover
    /// the window since each section was last sampled (0 on its first
    /// call; callers wanting instant rates sample twice, e.g. `probe`).
    /// `interval_seconds` is the oldest wanted section's window; see
    /// [`Sampler::common_window`] to make them one window.
    pub fn sample(&mut self, wants: Wants) -> SystemSnapshot {
        let now = Instant::now();
        let age = |at: Option<Instant>| at.map(|at| now.duration_since(at).as_secs_f64());
        let wanted = rate_sections(wants);
        let interval_seconds = if wanted.contains(&true) {
            // The oldest wanted section's window; 0 if any has none yet.
            let mut ages = wanted
                .iter()
                .zip(&self.section_sampled_at)
                .filter(|(wanted, _)| **wanted)
                .map(|(_, sampled_at)| age(*sampled_at));
            ages.try_fold(0.0f64, |oldest, section| section.map(|s| oldest.max(s))).unwrap_or(0.0)
        } else {
            age(self.previous_sample_at).unwrap_or(0.0)
        };
        for (wanted, sampled_at) in wanted.iter().zip(&mut self.section_sampled_at) {
            if *wanted {
                *sampled_at = Some(now);
            }
        }
        self.previous_sample_at = Some(now);

        let mut snapshot = SystemSnapshot {
            ts: unix_now(),
            interval_seconds,
            ..Default::default()
        };

        if wants.system {
            snapshot.system = Some(self.system_info());
        }
        if wants.sensors {
            snapshot.sensors = Some(self.sensors.collect(now));
        }
        if wants.cpu {
            let mut cpu = self.cpu.collect(now);
            // The CPU card's temperature: from the full sweep when it
            // ran, else read just the CPU chip (a full sweep asks every
            // drive for SMART data — milliseconds the CPU card never
            // needed).
            let temperature = match &snapshot.sensors {
                Some(sensors) => sensors::SensorsCollector::cpu_temperature(sensors),
                None => self.sensors.cpu_temperature_only(now),
            };
            if let Some((celsius, source)) = temperature {
                cpu.temperature_celsius = Some(celsius);
                cpu.temperature_source = Some(source);
            }
            snapshot.cpu = Some(cpu);
        }
        if wants.memory {
            snapshot.memory = Some(self.memory.collect());
        }
        if wants.gpu {
            snapshot.gpu = Some(self.gpu.collect(now));
        }
        if wants.network {
            snapshot.network = Some(self.net.collect(now));
        }
        if wants.disks {
            snapshot.disks = Some(self.disk.collect(now));
        }
        if wants.processes {
            let mut records = self.process.collect(now, self.boot_ts);
            // Merge per-process GPU usage into the process records
            // (pid → index once, not a linear find per GPU client).
            if let Some(gpu) = &snapshot.gpu {
                let index: std::collections::HashMap<i32, usize> =
                    records.iter().enumerate().map(|(i, r)| (r.pid, i)).collect();
                for usage in &gpu.processes {
                    if let Some(&i) = index.get(&usage.pid) {
                        records[i].gpu_busy_percent = usage.busy_percent;
                        records[i].gpu_vram_bytes = usage.vram_bytes;
                    }
                }
            }
            snapshot.processes = Some(records);
        }
        if wants.per_process_net || wants.connections {
            let net_sample = self.net_process.collect(now, wants.connections);
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
