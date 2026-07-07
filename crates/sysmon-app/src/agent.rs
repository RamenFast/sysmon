// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent-facing verbs: `probe` (one-shot state), `tap`
//! (streaming NDJSON), `ctl` (drive the live instance), `schema`
//! (the machine map). Everything answers with or without a running
//! daemon — probe/tap fall back to sampling in-process, so `sysmon
//! probe network --json` is always one command away from the truth.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::{Value, json};

use sysmon_core::collect::{Sampler, SamplerOptions};
use sysmon_core::snapshot::Wants;
use sysmon_core::units::{self, Units};

use crate::control;
use crate::envelope::{self, EXIT_BAD_ARGS, EXIT_OK, EXIT_RUNTIME, EXIT_UNAVAILABLE};

const SECTION_NAMES: &str =
    "all system cpu memory gpu network disks processes sensors connections";

fn parse_section_args(arguments: &[String]) -> Result<(Vec<String>, bool, Option<f64>), String> {
    let mut sections = Vec::new();
    let mut force_json = false;
    let mut interval = None;
    let mut cursor = arguments.iter().peekable();
    while let Some(argument) = cursor.next() {
        match argument.as_str() {
            "--json" => force_json = true,
            "--interval" | "-i" => {
                let value = cursor
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
                    .ok_or_else(|| "--interval needs a number of seconds".to_string())?;
                interval = Some(value);
            }
            section if !section.starts_with('-') => sections.push(section.to_string()),
            unknown => return Err(format!("unknown flag `{unknown}`")),
        }
    }
    Ok((sections, force_json, interval))
}

fn wants_for(sections: &[String]) -> Result<(Wants, Vec<String>), String> {
    if sections.is_empty() {
        return Ok((Wants::all(), vec!["all".to_string()]));
    }
    let mut wants = Wants::none();
    for section in sections {
        match Wants::from_section_name(section) {
            Some(section_wants) => wants = wants.union(section_wants),
            None => return Err(format!("unknown section `{section}` (try: {SECTION_NAMES})")),
        }
    }
    Ok((wants, sections.to_vec()))
}

// ------------------------------------------------------------------ probe

pub fn run_probe(arguments: &[String]) -> i32 {
    let (sections, force_json, _interval) = match parse_section_args(arguments) {
        Ok(parsed) => parsed,
        Err(message) => return envelope::fail(message, format!("sections: {SECTION_NAMES}"), EXIT_BAD_ARGS),
    };
    let (wants, section_names) = match wants_for(&sections) {
        Ok(parsed) => parsed,
        Err(message) => return envelope::fail(message, format!("sections: {SECTION_NAMES}"), EXIT_BAD_ARGS),
    };

    // A live instance answers with its own (longer, smoother) window;
    // otherwise sample twice in-process for real instantaneous rates.
    let (snapshot, via) = match control::request(&json!({
        "verb": "snapshot",
        "sections": section_names,
    })) {
        Some(reply) if reply["status"] == "ok" => (reply["result"].clone(), "socket"),
        _ => {
            let mut sampler = Sampler::with_options(SamplerOptions {
                enable_nethogs: false,
            });
            let _prime = sampler.sample(wants);
            std::thread::sleep(Duration::from_millis(250));
            match serde_json::to_value(sampler.sample(wants)) {
                Ok(value) => (value, "direct"),
                Err(serialize_error) => {
                    return envelope::fail(
                        format!("snapshot serialization failed: {serialize_error}"),
                        "this is a sysmon bug — please report it",
                        EXIT_RUNTIME,
                    );
                }
            }
        }
    };

    if envelope::json_wanted(force_json) {
        let mut result = snapshot;
        if let Some(object) = result.as_object_mut() {
            object.insert("via".to_string(), json!(via));
        }
        envelope::emit(&envelope::ok(result));
    } else {
        render_human(&snapshot, via);
    }
    EXIT_OK
}

// -------------------------------------------------------------------- tap

pub fn run_tap(arguments: &[String]) -> i32 {
    let (sections, _force_json, interval) = match parse_section_args(arguments) {
        Ok(parsed) => parsed,
        Err(message) => return envelope::fail(message, format!("sections: {SECTION_NAMES}"), EXIT_BAD_ARGS),
    };
    let default_sections = if sections.is_empty() {
        vec!["network".to_string()]
    } else {
        sections
    };
    let (wants, section_names) = match wants_for(&default_sections) {
        Ok(parsed) => parsed,
        Err(message) => return envelope::fail(message, format!("sections: {SECTION_NAMES}"), EXIT_BAD_ARGS),
    };
    let interval = interval.unwrap_or(1.0).clamp(0.2, 60.0);

    // Prefer the live instance's stream.
    if let Ok(stream) = UnixStream::connect(control::socket_path()) {
        let mut writer = match stream.try_clone() {
            Ok(writer) => writer,
            Err(_) => return EXIT_RUNTIME,
        };
        let subscribe = json!({
            "verb": "subscribe",
            "sections": section_names,
            "interval": interval,
        });
        if writeln!(writer, "{subscribe}").is_ok() {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if println_checked(&line).is_err() {
                            return EXIT_OK; // consumer closed the pipe
                        }
                    }
                    Err(_) => break,
                }
            }
            return EXIT_OK;
        }
    }

    // No daemon: sample locally, forever, at the asked cadence.
    let mut sampler = Sampler::new(); // long-lived: nethogs welcome
    let _prime = sampler.sample(wants);
    loop {
        std::thread::sleep(Duration::from_secs_f64(interval));
        let snapshot = sampler.sample(wants);
        match serde_json::to_string(&snapshot) {
            Ok(line) => {
                if println_checked(&line).is_err() {
                    return EXIT_OK;
                }
            }
            Err(_) => return EXIT_RUNTIME,
        }
    }
}

/// println! panics on EPIPE; a closed consumer is a normal way for a
/// tap to end.
fn println_checked(line: &str) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    writeln!(lock, "{line}")?;
    lock.flush()
}

// -------------------------------------------------------------------- ctl

pub fn run_ctl(arguments: &[String]) -> i32 {
    let mut positional: Vec<&String> = Vec::new();
    let mut force_json = false;
    for argument in arguments {
        if argument == "--json" {
            force_json = true;
        } else {
            positional.push(argument);
        }
    }
    let Some(verb) = positional.first().map(|s| s.as_str()) else {
        return envelope::fail(
            "ctl needs a verb",
            "verbs: status quit pause resume interval <s> raise page <overview|processes> \
             theme <id> palette <id> popout <section> popin <section> shot [path] \
             compact <on|off> units <decimal|binary>",
            EXIT_BAD_ARGS,
        );
    };

    let mut request = json!({"verb": verb});
    match verb {
        "interval" => {
            let Some(seconds) = positional.get(1).and_then(|v| v.parse::<f64>().ok()) else {
                return envelope::fail(
                    "interval needs seconds",
                    "e.g. `sysmon ctl interval 2`",
                    EXIT_BAD_ARGS,
                );
            };
            request["seconds"] = json!(seconds);
        }
        "page" | "theme" | "palette" | "popout" | "popin" | "compact" | "units" => {
            let Some(value) = positional.get(1) else {
                return envelope::fail(
                    format!("`{verb}` needs a value"),
                    format!("e.g. `sysmon ctl {verb} <value>`"),
                    EXIT_BAD_ARGS,
                );
            };
            request["value"] = json!(value);
        }
        "shot" => {
            if let Some(path) = positional.get(1) {
                request["path"] = json!(path);
            }
        }
        _ => {}
    }

    let Some(reply) = control::request(&request) else {
        return envelope::fail(
            "no running instance owns the control socket",
            "start one: `sysmon` (GUI), `sysmon --background` (headless GUI), or `sysmon serve`",
            EXIT_UNAVAILABLE,
        );
    };

    let ok = reply["status"] == "ok";
    if envelope::json_wanted(force_json) {
        envelope::emit(&reply);
    } else if ok {
        match serde_json::to_string_pretty(&reply["result"]) {
            Ok(pretty) => println!("{pretty}"),
            Err(_) => println!("{}", reply["result"]),
        }
    } else {
        eprintln!("sysmon: {}", reply["error"].as_str().unwrap_or("error"));
        eprintln!("fix: {}", reply["fix"].as_str().unwrap_or("—"));
    }
    if ok { EXIT_OK } else { EXIT_UNAVAILABLE }
}

// ----------------------------------------------------------------- schema

pub fn run_schema(_arguments: &[String]) -> i32 {
    let schema = build_schema();
    match serde_json::to_string_pretty(&schema) {
        Ok(pretty) => println!("{pretty}"),
        Err(_) => println!("{schema}"),
    }
    EXIT_OK
}

fn build_schema() -> Value {
    json!({
        "tool": "sysmon",
        "version": sysmon_core::VERSION,
        "contract": {
            "envelope": {
                "shape": {"status": "ok|error", "tool": "sysmon", "version": "semver",
                          "ts": "unix seconds", "result": "on ok", "error": "on error",
                          "fix": "ALWAYS present on error"},
                "exit_codes": {"0": "ok", "2": "unavailable (no daemon / feature absent)",
                                "3": "bad arguments", "4": "runtime failure"},
                "json_output": "automatic when stdout is piped; --json forces it",
            },
            "socket": {
                "path": control::socket_path(),
                "protocol": "one JSON object per line in; one envelope line out; `subscribe` upgrades to a raw NDJSON snapshot stream",
                "verbs": ["status", "snapshot", "subscribe", "pause", "resume", "interval",
                           "quit", "raise", "page", "theme", "palette", "popout", "popin",
                           "shot", "compact", "units"],
                "single_owner": "GUI or serve — whoever binds first; `sysmon serve` exits 2 if occupied",
            },
        },
        "commands": {
            "sysmon": "GUI (default command); plain re-launch raises the running instance",
            "sysmon --background": "GUI on a private Xvfb display — renders and serves the socket without touching your screen",
            "sysmon probe [sections…] [--json]": "one-shot snapshot; asks the live instance, else samples in-process (two samples, 250 ms apart, so rates are real)",
            "sysmon tap [sections…] [--interval N]": "NDJSON stream, one snapshot per line (default section: network, 1 s cadence)",
            "sysmon ctl <verb> […]": "drive the live instance",
            "sysmon serve": "headless daemon; samples only when asked — idle cost is zero",
            "sysmon schema": "this document",
        },
        "sections": {
            "system": {"hostname": "string", "kernel": "string",
                        "uptime_seconds": "f64", "boot_ts": "unix seconds"},
            "cpu": {
                "overall_percent": "f32 0–100, mean of cores over the window",
                "per_core_percent": "[f32]",
                "core_count": "logical cores",
                "frequency_mhz": "mean of per-core current frequencies (cpufreq)",
                "frequency_min_mhz | frequency_max_mhz": "hardware limits",
                "load_1m | load_5m | load_15m": "loadavg",
                "tasks_running | tasks_total": "loadavg 4th field (kernel tasks = threads)",
                "temperature_celsius": "CPU package (k10temp Tdie/Tctl or coretemp Package)",
                "context_switches_per_second": "machine-wide",
            },
            "memory": {
                "total_bytes | free_bytes | available_bytes": "MemTotal / MemFree / MemAvailable",
                "used_bytes": "total − available (psutil identity; htop shows a smaller figure by design)",
                "used_percent": "used / total",
                "cached_bytes": "Cached + SReclaimable",
                "buffers_bytes | dirty_bytes | shared_bytes": "meminfo verbatim",
                "swap_total_bytes | swap_used_bytes | swap_cached_bytes": "swap",
            },
            "gpu": {
                "available": "false ⇒ no amdgpu card (all other fields empty)",
                "device_name": "marketing name via lspci",
                "busy_percent": "gpu_busy_percent",
                "vram_used_bytes | vram_total_bytes": "dedicated VRAM",
                "gtt_used_bytes | gtt_total_bytes": "system RAM mapped by the GPU",
                "temperature_edge_celsius | temperature_junction_celsius | temperature_memory_celsius": "hwmon temp1/2/3",
                "power_draw_watts | power_cap_watts": "hwmon power1",
                "core_clock_mhz | memory_clock_mhz": "hwmon freq1/2",
                "fan_rpm | fan_max_rpm": "hwmon fan1",
                "processes": "[{pid, busy_percent, vram_bytes}] via DRM fdinfo, busiest first",
            },
            "network": {
                "download_bps | upload_bps": "physical interfaces only (a device symlink in /sys/class/net)",
                "total_received_bytes | total_sent_bytes": "physical, since interface counters reset",
                "interfaces": "[{name, kind: physical|loopback|virtual, is_up, ipv4[], ipv6[], rx_bps, tx_bps, rx_total_bytes, tx_total_bytes, speed_mbps?}]",
                "process_source": "nethogs (packet truth, all protocols) | tcp_diag (native TCP byte counters, own-UID) | none",
                "process_source_hint": "how to upgrade the source, when degraded",
                "top_processes": "[{pid, name, rx_bps, tx_bps}] busiest first — the bar's click-through",
            },
            "disks": "[{device, mount_point, display_name (label if any), fs_type, read_bps, write_bps, used_bytes, total_bytes, util_percent}] — real block devices, deduped",
            "processes": "[{pid, ppid, name, user, state, state_word, is_kernel_thread, cpu_percent (can exceed 100 when multithreaded), memory_rss_bytes, memory_virtual_bytes, threads, nice, started_ts, cpu_time_seconds, disk_read_bps?, disk_write_bps? (None = unreadable, not zero), gpu_busy_percent, gpu_vram_bytes, net_rx_bps?, net_tx_bps?, command_line, exe_basename?}]",
            "sensors": {
                "chips": "[{name, temps: [{label, celsius, max_celsius?, crit_celsius?}], fans: [{label, rpm}]}] — every hwmon chip",
                "battery": "{name, percent, status, power_draw_watts?, seconds_remaining?} when present",
            },
            "connections": "[{pid (-1 = unresolved), process_name?, protocol tcp|tcp6|udp|udp6, local_address, remote_address, state, rx_bps?, tx_bps? (TCP only)}] — the full socket table",
        },
        "notes": {
            "rates": "every *_bps/_percent rate spans `interval_seconds`, reported per snapshot; each collector tracks its own window so mixed-section clients never skew each other",
            "first_sample": "a fresh sampler's first snapshot has zero rates (nothing to delta against)",
            "bar_integration": "poll `sysmon tap network --interval 2`; on click, `sysmon probe network --json | jq .result.top_processes` and `sysmon probe connections`",
        },
    })
}

// ------------------------------------------------------------ human probe

fn value_f64(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}
fn value_u64(value: &Value) -> u64 {
    value.as_u64().unwrap_or(0)
}

/// A calm, aligned, terminal reading of a snapshot — for humans who
/// ran `sysmon probe` without a pipe.
fn render_human(snapshot: &Value, via: &str) {
    let units = Units::Decimal;
    let line = |label: &str, value: String| println!("  {label:<14} {value}");

    if let Some(system) = snapshot.get("system").filter(|s| !s.is_null()) {
        println!("system");
        line("hostname", system["hostname"].as_str().unwrap_or("?").to_string());
        line("kernel", system["kernel"].as_str().unwrap_or("?").to_string());
        line(
            "uptime",
            units::format_duration_seconds(value_f64(&system["uptime_seconds"])),
        );
    }
    if let Some(cpu) = snapshot.get("cpu").filter(|s| !s.is_null()) {
        println!("cpu");
        let mut headline = format!(
            "{} across {} threads",
            units::format_percent(value_f64(&cpu["overall_percent"]) as f32),
            value_u64(&cpu["core_count"]),
        );
        if let Some(freq) = cpu["frequency_mhz"].as_f64() {
            headline += &format!(" · {}", units::format_frequency_mhz(freq));
        }
        if let Some(temp) = cpu["temperature_celsius"].as_f64() {
            headline += &format!(" · {}", units::format_temperature(temp as f32));
        }
        line("busy", headline);
        line(
            "load",
            format!(
                "{:.2} · {:.2} · {:.2}",
                value_f64(&cpu["load_1m"]),
                value_f64(&cpu["load_5m"]),
                value_f64(&cpu["load_15m"])
            ),
        );
        line("tasks", format!("{}", value_u64(&cpu["tasks_total"])));
    }
    if let Some(memory) = snapshot.get("memory").filter(|s| !s.is_null()) {
        println!("memory");
        line(
            "used",
            format!(
                "{} of {} ({})",
                units::format_size(value_u64(&memory["used_bytes"]), units),
                units::format_size(value_u64(&memory["total_bytes"]), units),
                units::format_percent(value_f64(&memory["used_percent"]) as f32),
            ),
        );
        line(
            "available",
            units::format_size(value_u64(&memory["available_bytes"]), units),
        );
        line(
            "cached",
            units::format_size(value_u64(&memory["cached_bytes"]), units),
        );
    }
    if let Some(gpu) = snapshot.get("gpu").filter(|s| !s.is_null()) {
        println!("gpu");
        if gpu["available"].as_bool() == Some(true) {
            line("device", gpu["device_name"].as_str().unwrap_or("?").to_string());
            let mut busy = units::format_percent(value_f64(&gpu["busy_percent"]) as f32);
            if let Some(edge) = gpu["temperature_edge_celsius"].as_f64() {
                busy += &format!(" · {}", units::format_temperature(edge as f32));
            }
            if let Some(power) = gpu["power_draw_watts"].as_f64() {
                busy += &format!(" · {}", units::format_power(power as f32));
            }
            line("busy", busy);
            line(
                "vram",
                format!(
                    "{} of {}",
                    units::format_size(value_u64(&gpu["vram_used_bytes"]), units),
                    units::format_size(value_u64(&gpu["vram_total_bytes"]), units),
                ),
            );
        } else {
            line("device", "no AMD GPU on this machine".to_string());
        }
    }
    if let Some(network) = snapshot.get("network").filter(|s| !s.is_null()) {
        println!("network");
        line(
            "rates",
            format!(
                "↓ {}   ↑ {}",
                units::format_rate(value_f64(&network["download_bps"]), units),
                units::format_rate(value_f64(&network["upload_bps"]), units),
            ),
        );
        line(
            "totals",
            format!(
                "↓ {}   ↑ {}",
                units::format_size(value_u64(&network["total_received_bytes"]), units),
                units::format_size(value_u64(&network["total_sent_bytes"]), units),
            ),
        );
        if let Some(top) = network["top_processes"].as_array() {
            for entry in top.iter().take(3) {
                line(
                    entry["name"].as_str().unwrap_or("?"),
                    format!(
                        "↓ {}   ↑ {}",
                        units::format_rate(value_f64(&entry["rx_bps"]), units),
                        units::format_rate(value_f64(&entry["tx_bps"]), units),
                    ),
                );
            }
        }
    }
    if let Some(disks) = snapshot.get("disks").and_then(|d| d.as_array()) {
        println!("disks");
        for disk in disks {
            line(
                disk["display_name"].as_str().unwrap_or("?"),
                format!(
                    "{} · {} of {} · R {} W {}",
                    disk["mount_point"].as_str().unwrap_or("?"),
                    units::format_size(value_u64(&disk["used_bytes"]), units),
                    units::format_size(value_u64(&disk["total_bytes"]), units),
                    units::format_rate(value_f64(&disk["read_bps"]), units),
                    units::format_rate(value_f64(&disk["write_bps"]), units),
                ),
            );
        }
    }
    if let Some(processes) = snapshot.get("processes").and_then(|p| p.as_array()) {
        println!("processes ({} — top 5 by cpu)", processes.len());
        let mut sorted: Vec<&Value> = processes.iter().collect();
        sorted.sort_by(|a, b| {
            value_f64(&b["cpu_percent"])
                .partial_cmp(&value_f64(&a["cpu_percent"]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for process in sorted.iter().take(5) {
            line(
                process["name"].as_str().unwrap_or("?"),
                format!(
                    "pid {} · {:.1}% · {}",
                    process["pid"],
                    value_f64(&process["cpu_percent"]),
                    units::format_size(value_u64(&process["memory_rss_bytes"]), units),
                ),
            );
        }
    }
    if let Some(connections) = snapshot.get("connections").and_then(|c| c.as_array()) {
        println!("connections ({})", connections.len());
    }
    println!("(window {:.2}s · via {via})", value_f64(&snapshot["interval_seconds"]));
}
