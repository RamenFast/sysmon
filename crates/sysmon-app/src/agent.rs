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
    "all system cpu memory gpu nvidia network disks processes sensors connections occupants";

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
                // `NaN` and `inf` parse as f64 and then panic inside
                // Duration::from_secs_f64 — a raw backtrace and exit
                // 101, which is not in the standard's set. Out-of-range
                // values used to be silently clamped, which quietly
                // ignored what the caller asked for. Both are refused
                // here, as bad arguments, with the range in the fix.
                if !value.is_finite() || !(0.2..=60.0).contains(&value) {
                    return Err(format!(
                        "--interval must be a number of seconds between 0.2 and 60 (got `{value}`)"
                    ));
                }
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
            None if section == "threads" => {
                return Err("threads is one process's read: `sysmon probe threads --pid <pid>`".to_string());
            }
            None => return Err(format!("unknown section `{section}` (try: {SECTION_NAMES})")),
        }
    }
    Ok((wants, sections.to_vec()))
}

// ------------------------------------------------------------------ probe

pub fn run_probe(arguments: &[String]) -> i32 {
    if arguments.iter().any(|argument| argument == "threads") {
        return run_probe_threads(arguments);
    }
    // A parse failure happens before `force_json` is known, so read the
    // flag straight off the argv: `--json` must be honored on the very
    // call that got the arguments wrong.
    let asked_for_json = arguments.iter().any(|argument| argument == "--json");
    let (sections, force_json, _interval) = match parse_section_args(arguments) {
        Ok(parsed) => parsed,
        Err(message) => {
            return envelope::fail_forced(
                message,
                format!("sections: {SECTION_NAMES}"),
                EXIT_BAD_ARGS,
                asked_for_json,
            );
        }
    };
    let (wants, section_names) = match wants_for(&sections) {
        Ok(parsed) => parsed,
        Err(message) => {
            return envelope::fail_forced(
                message,
                format!("sections: {SECTION_NAMES}"),
                EXIT_BAD_ARGS,
                force_json,
            );
        }
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
            // Per-process CPU comes from 10 ms clock ticks, so the
            // window sets the step size: 0.25 s moved in 4% jumps
            // (3.1 audit F12). A full second makes one tick 1%; the
            // system-wide sections are fine on the short window.
            let window = if wants.processes || wants.cpu_occupants { 1000 } else { 250 };
            std::thread::sleep(Duration::from_millis(window));
            match sampler.sample(wants).to_json_value() {
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

/// `sysmon probe threads --pid N`: one process, thread by thread, and
/// which CPUs its time landed on. Always read in-process (any user's
/// task list is readable; a running window has nothing to add), over
/// a one-second window: thread CPU comes in 10 ms ticks.
fn run_probe_threads(arguments: &[String]) -> i32 {
    const FIX: &str = "usage: sysmon probe threads --pid <pid> [--json] (threads is one process's read; it doesn't combine with other sections)";
    let force_json = arguments.iter().any(|argument| argument == "--json");
    let mut pid = None;
    let mut cursor = arguments.iter();
    while let Some(argument) = cursor.next() {
        match argument.as_str() {
            "threads" | "--json" => {}
            "--pid" | "-p" => match cursor.next().and_then(|value| value.parse::<i32>().ok()).filter(|pid| *pid > 0) {
                Some(value) => pid = Some(value),
                None => return envelope::fail_forced("--pid needs a process id (a positive number)", FIX, EXIT_BAD_ARGS, force_json),
            },
            other => {
                return envelope::fail_forced(format!("`{other}` doesn't go with threads"), FIX, EXIT_BAD_ARGS, force_json);
            }
        }
    }
    let Some(pid) = pid else {
        return envelope::fail_forced("threads needs --pid <pid>", FIX, EXIT_BAD_ARGS, force_json);
    };
    let gone = || {
        envelope::fail_forced(
            format!("no process {pid}"),
            "find a pid with `sysmon probe processes` (or `pgrep <name>`); it may have exited",
            EXIT_RUNTIME,
            force_json,
        )
    };
    let mut sampler = sysmon_core::collect::threads::ThreadSampler::new();
    if sampler.sample(pid).is_none() {
        return gone();
    }
    std::thread::sleep(Duration::from_millis(1000));
    let Some(threads) = sampler.sample(pid) else {
        return gone();
    };
    if envelope::json_wanted(force_json) {
        envelope::emit(&envelope::ok(json!({ "threads": threads, "via": "direct" })));
    } else {
        render_threads_human(&threads);
    }
    EXIT_OK
}

fn render_threads_human(threads: &sysmon_core::collect::threads::ProcessThreads) {
    out!(
        "{} (pid {}) · {:.1}% · {} thread{} · {:.2} of {} CPUs",
        threads.name,
        threads.pid,
        threads.cpu_percent,
        threads.threads.len(),
        if threads.threads.len() == 1 { "" } else { "s" },
        threads.cpu_percent / 100.0,
        threads.cpu_count
    );
    if let Some(allowed) = &threads.allowed_cpus {
        out!("  pinned to CPUs {allowed}");
    }
    let mut busy: Vec<(usize, f32)> =
        threads.per_cpu_percent.iter().copied().enumerate().filter(|(_, percent)| *percent >= 0.5).collect();
    busy.sort_by(|a, b| b.1.total_cmp(&a.1));
    if busy.is_empty() {
        out!("  on CPUs        (idle this second)");
    } else {
        let list: Vec<String> = busy.iter().map(|(cpu, percent)| format!("cpu{cpu} {percent:.0}%")).collect();
        out!("  on CPUs        {}", list.join("  "));
    }
    out!("  {:>8}  {:<16} {:>6}  {:>4}  state", "tid", "thread", "cpu%", "cpu");
    let shown = threads.threads.iter().filter(|thread| thread.cpu_percent >= 0.5).take(24).count().max(5);
    for thread in threads.threads.iter().take(shown) {
        out!(
            "  {:>8}  {:<16} {:>6.1}  {:>4}  {}",
            thread.tid,
            thread.name,
            thread.cpu_percent,
            thread.cpu.map(|cpu| cpu.to_string()).unwrap_or("—".into()),
            thread.state_word
        );
    }
    if threads.threads.len() > shown {
        out!("  … and {} quieter threads (--json for all)", threads.threads.len() - shown);
    }
}

// -------------------------------------------------------------------- tap

pub fn run_tap(arguments: &[String]) -> i32 {
    let (sections, force_json, interval) = match parse_section_args(arguments) {
        Ok(parsed) => parsed,
        Err(message) => {
            return envelope::fail_forced(
                message,
                format!("sections: {SECTION_NAMES}; --interval takes 0.2–60 seconds"),
                EXIT_BAD_ARGS,
                arguments.iter().any(|a| a == "--json"),
            );
        }
    };
    let _ = force_json; // a stream is always NDJSON; --json is accepted for symmetry
    let default_sections = if sections.is_empty() {
        vec!["network".to_string()]
    } else {
        sections
    };
    let (wants, section_names) = match wants_for(&default_sections) {
        Ok(parsed) => parsed,
        Err(message) => {
            return envelope::fail_forced(
                message,
                format!("sections: {SECTION_NAMES}"),
                EXIT_BAD_ARGS,
                arguments.iter().any(|a| a == "--json"),
            );
        }
    };
    let interval = interval.unwrap_or(1.0).clamp(0.2, 60.0); // validated above

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
                    // A closed consumer ends the process with 0.
                    Ok(line) => envelope::print_line(&line),
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
        // The same line shape either way — a consumer cannot tell
        // (and must not care) whether a live instance or this
        // process sampled it.
        match control::stream_line(sampler.sample(wants).to_json_value()) {
            Ok(line) => envelope::print_line(&line),
            Err(_) => return EXIT_RUNTIME,
        }
    }
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
        return envelope::fail_forced(
            "ctl needs a verb",
            "verbs: status quit pause resume interval <s> raise page <overview|processes> \
             theme <id> palette <id> popout <section> popin <section> shot [path] \
             compact <on|off> units <decimal|binary> temperature <celsius|fahrenheit|both> \
             cpugraph <total|threads|grid> inspect <pid> details <pid>",
            EXIT_BAD_ARGS,
            force_json,
        );
    };

    let mut request = json!({"verb": verb});
    match verb {
        "interval" => {
            let Some(seconds) = positional.get(1).and_then(|v| v.parse::<f64>().ok()) else {
                return envelope::fail_forced(
                    "interval needs seconds between 0.2 and 60",
                    "e.g. `sysmon ctl interval 2`",
                    EXIT_BAD_ARGS,
                    force_json,
                );
            };
            request["seconds"] = json!(seconds);
        }
        "page" | "theme" | "palette" | "popout" | "popin" | "compact" | "units" | "temperature"
        | "cpugraph" | "inspect" | "details" => {
            let Some(value) = positional.get(1) else {
                return envelope::fail_forced(
                    format!("`{verb}` needs a value"),
                    format!("e.g. `sysmon ctl {verb} <value>`"),
                    EXIT_BAD_ARGS,
                    force_json,
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
        return envelope::fail_forced(
            "no running instance owns the control socket",
            "start one: `sysmon` (GUI), `sysmon --background` (headless GUI), or `sysmon serve`",
            EXIT_UNAVAILABLE,
            force_json,
        );
    };

    let ok = reply["status"] == "ok";
    if envelope::json_wanted(force_json) {
        envelope::emit(&reply);
    } else if ok {
        match serde_json::to_string_pretty(&reply["result"]) {
            Ok(pretty) => out!("{pretty}"),
            Err(_) => out!("{}", reply["result"]),
        }
    } else {
        eprintln!("sysmon: {}", reply["error"].as_str().unwrap_or("error"));
        eprintln!("fix: {}", reply["fix"].as_str().unwrap_or("—"));
    }
    if ok {
        EXIT_OK
    } else {
        // The instance classified its own failure (`exit` on the
        // wire): a bad theme name is bad arguments (3), a GUI-only
        // verb against `serve` is unavailable (2). Older instances
        // omit the field; unavailable stays the honest default.
        reply["exit"]
            .as_i64()
            .map(|code| code as i32)
            .unwrap_or(EXIT_UNAVAILABLE)
    }
}

// ------------------------------------------------------------------ specs

const SPEC_SECTION_NAMES: &str = "system cpu memory graphics storage network audio";

/// `sysmon specs [section] [--json]`: what this machine *is*. On a
/// terminal, the same plain-text sheet the copy button copies; piped,
/// the envelope with every structured field plus `result.text`, so
/// `sysmon specs --json | jq -j .result.text` is byte-for-byte the
/// clipboard. Never needs a running instance.
pub fn run_specs(arguments: &[String]) -> i32 {
    let mut force_json = false;
    let mut section: Option<&str> = None;
    for argument in arguments {
        match argument.as_str() {
            "--json" => force_json = true,
            name if !name.starts_with('-') && section.is_none() => section = Some(name),
            other => {
                let message = if other.starts_with('-') {
                    format!("unknown flag `{other}`")
                } else {
                    format!("specs takes one section, got a second: `{other}`")
                };
                return envelope::fail_forced(
                    message,
                    format!("usage: sysmon specs [section] [--json]; sections: {SPEC_SECTION_NAMES}"),
                    EXIT_BAD_ARGS,
                    arguments.iter().any(|a| a == "--json"),
                );
            }
        }
    }
    let section = match section {
        Some(name) => match sysmon_core::specs::SPEC_SECTIONS.iter().find(|s| **s == name) {
            Some(known) => Some(*known),
            None => {
                return envelope::fail_forced(
                    format!("unknown spec section `{name}`"),
                    format!("sections: {SPEC_SECTION_NAMES}"),
                    EXIT_BAD_ARGS,
                    force_json,
                );
            }
        },
        None => None,
    };

    let specs = sysmon_core::specs::collect();
    let text = match section {
        Some(section) => sysmon_core::specs::text::section_text(section, &specs),
        None => sysmon_core::specs::text::specs_text(&specs),
    };
    if !envelope::json_wanted(force_json) {
        for line in text.lines() {
            out!("{line}");
        }
        return EXIT_OK;
    }
    let mut result = match specs.to_json_value() {
        Ok(value) => value,
        Err(serialize_error) => {
            return envelope::fail(
                format!("specs serialization failed: {serialize_error}"),
                "this is a sysmon bug — please report it",
                EXIT_RUNTIME,
            );
        }
    };
    if let Some(object) = result.as_object_mut() {
        object.insert("section".to_string(), json!(section.unwrap_or("all")));
        object.insert("text".to_string(), json!(text));
    }
    envelope::emit(&envelope::ok(result));
    EXIT_OK
}

// ---------------------------------------------------------------- history

const HISTORY_SECTION_NAMES: &str = "all cpu memory gpu nvidia network disks sensors";

/// "90s", "15m", "2h", "7d", "1y", a bare number of seconds, or a unix time
/// (a number above 1e9) → seconds back from `now`.
fn parse_when(raw: &str, now: f64) -> Option<f64> {
    let raw = raw.trim();
    let (number, scale) = match raw.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((index, _)) => {
            let scale = match &raw[index..] {
                "s" | "sec" => 1.0,
                "m" | "min" => 60.0,
                "h" => 3_600.0,
                "d" => 86_400.0,
                "w" => 604_800.0,
                "y" => 31_536_000.0,
                _ => return None,
            };
            (&raw[..index], scale)
        }
        None => (raw, 1.0),
    };
    let value = number.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)?;
    if scale == 1.0 && value > 1e9 {
        return Some((now - value).max(0.0)); // an absolute unix time
    }
    Some(value * scale)
}

/// The store's management ops: `--info`, `--export`, `--clear`, `--pack`.
enum StoreOp {
    Info,
    Export(String),
    Clear,
    Pack,
}

/// `sysmon history [section] [--since D] [--until D] [--specs]
/// [--records] [--blocks N] [--raw] [--json]`: the usage history SysMon
/// keeps on disk, summarized (min/avg/p95/max, a timeline, the GPU's
/// programs), from both tiers. Reads the files directly: needs no
/// running instance. `--info`, `--export PATH`, `--clear --yes` and
/// `--pack` manage the store itself.
pub fn run_history(arguments: &[String]) -> i32 {
    let asked_for_json = arguments.iter().any(|a| a == "--json");
    let usage = format!(
        "usage: sysmon history [section] [--since 1h] [--until 0s] [--specs] [--records] [--blocks 48] [--raw] [--json]; \
         sysmon history --info | --export PATH [--force] | --clear --yes | --pack; \
         sections: {HISTORY_SECTION_NAMES}; times: 90s 15m 2h 7d 1y, or a unix time"
    );
    let bad = |message: String| envelope::fail_forced(message, usage.clone(), EXIT_BAD_ARGS, asked_for_json);
    let now = envelope::now_ts();
    let mut section: Option<&str> = None;
    let mut since = 3_600.0;
    let mut until = 0.0;
    let mut with_specs = false;
    let mut records = false;
    let mut raw = false;
    let mut yes = false;
    let mut force = false;
    let mut store_op: Option<StoreOp> = None;
    let mut blocks = sysmon_core::history::summary::TIMELINE_BUCKETS;
    let mut cursor = arguments.iter();
    while let Some(argument) = cursor.next() {
        let op = match argument.as_str() {
            "--info" => Some(StoreOp::Info),
            "--clear" => Some(StoreOp::Clear),
            "--pack" => Some(StoreOp::Pack),
            "--export" => match cursor.next() {
                Some(path) if !path.starts_with("--") => Some(StoreOp::Export(path.clone())),
                _ => return bad("--export needs a path: a file (…/history.tar.zst) or a directory".to_string()),
            },
            _ => None,
        };
        if let Some(op) = op {
            if store_op.is_some() {
                return bad("one of --info, --export, --clear, --pack at a time".to_string());
            }
            store_op = Some(op);
            continue;
        }
        match argument.as_str() {
            "--json" => {}
            "--specs" => with_specs = true,
            "--records" => records = true,
            "--raw" => raw = true,
            "--yes" => yes = true,
            "--force" => force = true,
            flag @ ("--since" | "--until") => {
                let Some(value) = cursor.next() else {
                    return bad(format!("{flag} needs a time, e.g. {flag} 2h"));
                };
                let Some(seconds) = parse_when(value, now) else {
                    return bad(format!("{flag}: can't read `{value}` as a time"));
                };
                if flag == "--since" { since = seconds } else { until = seconds }
            }
            "--blocks" => {
                match cursor.next().and_then(|v| v.parse::<usize>().ok()).filter(|n| (1..=1_000).contains(n)) {
                    Some(n) => blocks = n,
                    None => return bad("--blocks needs a whole number from 1 to 1000".to_string()),
                }
            }
            name if !name.starts_with('-') && section.is_none() => section = Some(name),
            other if other.starts_with('-') => return bad(format!("unknown flag `{other}`")),
            other => return bad(format!("history takes one section, got a second: `{other}`")),
        }
    }
    if let Some(op) = store_op {
        if section.is_some() || with_specs || records || raw {
            return bad("--info/--export/--clear/--pack act on the whole store: no section or summary flags".to_string());
        }
        if yes && !matches!(op, StoreOp::Clear) || force && !matches!(op, StoreOp::Export(_)) {
            return bad("--yes goes with --clear, --force with --export".to_string());
        }
        return history_store_op(op, yes, force, asked_for_json);
    }
    if yes || force {
        return bad("--yes goes with --clear, --force with --export".to_string());
    }
    let section = section.unwrap_or("all");
    if section != "all" && !sysmon_core::history::HISTORY_SECTIONS.contains(&section) {
        return bad(format!("unknown history section `{section}`"));
    }
    if with_specs && section == "all" {
        return bad("--specs needs a section (it copies that section's specs, like its pop-out)".to_string());
    }
    if until >= since {
        return bad(format!("--until ({until:.0} s ago) must be later than --since ({since:.0} s ago)"));
    }
    let (from_ts, to_ts) = (now - since, now - until);

    let dir = sysmon_core::history::default_dir();
    let store = sysmon_core::history::Recording::info(&dir);
    if store.hot_files == 0 && store.archive_days == 0 {
        return envelope::fail_forced(
            format!("no usage history in {}", dir.display()),
            "history is recorded while the SysMon window runs (Display options → Usage history), \
             or by `sysmon serve --record`",
            EXIT_UNAVAILABLE,
            asked_for_json,
        );
    }

    if records {
        // NDJSON, one record per line, like `tap`: an agent plots or
        // filters what the summary condensed.
        let history_section = if section == "all" { None } else { Some(section) };
        sysmon_core::history::Recording::for_each(&dir, from_ts, to_ts, |record| {
            let mut value = serde_json::to_value(&record).unwrap_or(Value::Null);
            if let Some(object) = value.as_object_mut() {
                if let Some(keep) = history_section {
                    let wanted: &[&str] = match keep {
                        "sensors" => &["temps", "fans"],
                        other => &[other][..],
                    };
                    let wanted: Vec<&str> = wanted.to_vec();
                    object.retain(|key, _| key == "ts" || key == "interval_seconds" || wanted.contains(&key.as_str()));
                }
                object.insert("event".to_string(), json!("history"));
            }
            envelope::print_line(&value.to_string());
        });
        return EXIT_OK;
    }

    let summary = sysmon_core::history::summarize_dir(&dir, section, from_ts, to_ts, blocks, !raw);
    let style = sysmon_core::history::text::TextStyle::default();
    let text = if with_specs {
        let spec_section = crate::gui::specs::spec_section_for_popout(section);
        let specs_text = match spec_section {
            Some(spec_section) => sysmon_core::specs::text::section_text(spec_section, &sysmon_core::specs::collect()),
            None => {
                // Sensors: its readings now, as its pop-out copies them.
                let mut sampler = Sampler::with_options(SamplerOptions { enable_nethogs: false });
                let mut wants = Wants::none();
                wants.sensors = true;
                crate::gui::specs::sensor_readings_text(&sampler.sample(wants))
            }
        };
        sysmon_core::history::text::section_report(
            crate::gui::specs::section_title(section),
            Some(&specs_text),
            &summary,
            style,
        )
    } else {
        sysmon_core::history::text::summary_text(&summary, style)
    };
    if !envelope::json_wanted(asked_for_json) {
        for line in text.lines() {
            out!("{line}");
        }
        return EXIT_OK;
    }
    let mut result = match serde_json::to_value(&summary) {
        Ok(value) => value,
        Err(serialize_error) => {
            return envelope::fail(
                format!("history serialization failed: {serialize_error}"),
                "this is a sysmon bug — please report it",
                EXIT_RUNTIME,
            );
        }
    };
    if let Some(object) = result.as_object_mut() {
        object.insert("range_seconds".to_string(), json!(to_ts - from_ts));
        object.insert("store".to_string(), json!(store));
        object.insert(
            "recorder".to_string(),
            json!(sysmon_core::history::store::current_writer(&dir)),
        );
        if summary.samples == 0 {
            object.insert(
                "hint".to_string(),
                json!("nothing was recorded in this range; widen --since, or check `store.newest_ts`"),
            );
        }
        object.insert("text".to_string(), json!(text));
    }
    envelope::emit(&envelope::ok(result));
    EXIT_OK
}


/// "1.2 GB" (decimal, like the menu).
fn human_bytes(bytes: f64) -> String {
    sysmon_core::units::format_size(bytes.max(0.0).round() as u64, sysmon_core::units::Units::Decimal)
}

/// The store, in a few lines a person reads: where, how big, how far
/// back, how fast it grows, what removes it.
fn store_text(info: &sysmon_core::history::StoreInfo, limits: &sysmon_core::history::Limits, writer: Option<&str>, now: f64) -> String {
    use sysmon_core::history::text::{local_range, range_label};
    let mut lines = vec![format!("Usage history in {}", info.dir.display())];
    let span = match (info.oldest_ts, info.newest_ts) {
        (Some(oldest), Some(newest)) => format!(", {} back ({})", range_label(((now - oldest) / 60.0).round() * 60.0), local_range(oldest, newest)),
        _ => String::new(),
    };
    lines.push(format!("{} on disk{span}", human_bytes(info.bytes as f64)));
    lines.push(format!(
        "  hourly (last {}): {} in {} file{}",
        range_label(limits.hot_seconds),
        human_bytes(info.hot_bytes as f64),
        info.hot_files,
        if info.hot_files == 1 { "" } else { "s" }
    ));
    lines.push(format!(
        "  archive: {} in {} day file{}",
        human_bytes(info.archive_bytes as f64),
        info.archive_days,
        if info.archive_days == 1 { "" } else { "s" }
    ));
    match info.cadence_seconds.and_then(|c| sysmon_core::history::growth(info, c)) {
        Some(growth) => lines.push(format!(
            "Grows ~{} a month, ~{} a year at one sample every {} ({:.0} B a sample, {})",
            human_bytes(growth.per_month_bytes),
            human_bytes(growth.per_year_bytes),
            range_label(growth.cadence_seconds),
            growth.bytes_per_sample,
            if growth.basis == "archive" { "measured on the archive" } else { "estimated until a day is packed" }
        )),
        None => lines.push("Growth: not enough recorded yet to say".to_string()),
    }
    let keep = match limits.keep_seconds {
        Some(keep) => format!("days older than {} are removed", range_label(keep)),
        None => "nothing is removed by age".to_string(),
    };
    let cap = match limits.max_bytes {
        Some(cap) => format!("the oldest data goes past {}", human_bytes(cap as f64)),
        None => "no size cap".to_string(),
    };
    lines.push(format!("Kept: {keep}; {cap} (☰ → Usage history)"));
    lines.push(format!("Recorder: {}", writer.unwrap_or("none running")));
    lines.join("\n") + "\n"
}

fn history_store_op(op: StoreOp, yes: bool, force: bool, asked_for_json: bool) -> i32 {
    use sysmon_core::history::{manage, OpenError, Recording};
    let dir = sysmon_core::history::default_dir();
    let now = envelope::now_ts();
    let settings = crate::gui::settings::Settings::load();
    let limits = settings.history_limits();
    let writer = sysmon_core::history::store::current_writer(&dir);
    let reply = |result: Value, text: String| -> i32 {
        if !envelope::json_wanted(asked_for_json) {
            for line in text.lines() {
                out!("{line}");
            }
            return EXIT_OK;
        }
        let mut result = result;
        result["text"] = json!(text);
        envelope::emit(&envelope::ok(result));
        EXIT_OK
    };
    // Ask the recorder, when one holds the lock: (reply result) or a failure exit.
    let via_recorder = |op_name: &str| -> Result<Value, i32> {
        let request = json!({"verb": "history", "op": op_name, "yes": true});
        let reply = std::os::unix::net::UnixStream::connect(crate::control::socket_path())
            .ok()
            .and_then(|stream| crate::control::request_on_waiting(stream, &request, std::time::Duration::from_secs(600)));
        match reply {
            Some(reply) if reply["status"] == "ok" => Ok(reply["result"].clone()),
            Some(reply) => Err(envelope::fail_forced(
                reply["error"].as_str().unwrap_or("the recorder refused").to_string(),
                reply["fix"].as_str().unwrap_or("try again").to_string(),
                reply["exit"].as_i64().map_or(EXIT_UNAVAILABLE, |e| e as i32),
                asked_for_json,
            )),
            None => Err(envelope::fail_forced(
                format!("{} holds the history and did not answer", writer.as_deref().unwrap_or("another SysMon")),
                "it may be an older SysMon (3.4) that cannot clear or pack; close it (or `systemctl --user stop sysmon-record`), then run this again",
                EXIT_UNAVAILABLE,
                asked_for_json,
            )),
        }
    };
    match op {
        StoreOp::Info => {
            let info = Recording::info(&dir);
            let growth = info.cadence_seconds.and_then(|c| sysmon_core::history::growth(&info, c));
            let text = store_text(&info, &limits, writer.as_deref(), now);
            reply(json!({"op": "info", "store": info, "growth": growth, "limits": limits, "recorder": writer, "warn_bytes": sysmon_core::history::store::WARN_BYTES}), text)
        }
        StoreOp::Export(raw_path) => {
            let mut path = std::path::PathBuf::from(&raw_path);
            if path.is_dir() {
                path = path.join(manage::default_export_name(now));
            }
            if path.starts_with(&dir) {
                return envelope::fail_forced(
                    "an export inside the history directory would be part of the history",
                    "write it somewhere else, e.g. `--export ~/sysmon-history.tar.zst`",
                    EXIT_BAD_ARGS,
                    asked_for_json,
                );
            }
            match manage::export(&dir, &path, force) {
                Ok(report) => {
                    let text = format!(
                        "Exported {} files ({} of history) to {} ({})\nRestore: stop SysMon, then `tar --zstd -xf {} -C {}`\n",
                        report.files,
                        human_bytes(report.source_bytes as f64),
                        report.path.display(),
                        human_bytes(report.bytes as f64),
                        report.path.display(),
                        dir.parent().map_or_else(|| dir.display().to_string(), |p| p.display().to_string()),
                    );
                    reply(json!({"op": "export", "export": report}), text)
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => envelope::fail_forced(
                    format!("{} already exists", path.display()),
                    "pick another name, or add --force to replace it",
                    EXIT_BAD_ARGS,
                    asked_for_json,
                ),
                Err(error) => envelope::fail_forced(
                    format!("export to {} failed: {error}", path.display()),
                    "check the target directory exists, is writable and has room",
                    EXIT_RUNTIME,
                    asked_for_json,
                ),
            }
        }
        StoreOp::Clear => {
            let preview = manage::clear_preview(&dir);
            if !yes {
                return envelope::fail_forced(
                    format!(
                        "--clear deletes the whole usage history: {} files, {} in {}",
                        preview.removed_files,
                        human_bytes(preview.removed_bytes as f64),
                        dir.display()
                    ),
                    "add --yes to confirm (`sysmon history --export PATH` first keeps a copy)",
                    EXIT_BAD_ARGS,
                    asked_for_json,
                );
            }
            let cleared = match manage::clear_unlocked(&dir) {
                Ok(report) => json!({"op": "clear", "cleared": report, "by": "cli"}),
                Err(OpenError::Busy(_)) => match via_recorder("clear") {
                    Ok(result) => result,
                    Err(exit) => return exit,
                },
                Err(OpenError::Io(error)) => {
                    return envelope::fail_forced(
                        format!("clearing {} failed: {error}", dir.display()),
                        "check the directory's permissions",
                        EXIT_RUNTIME,
                        asked_for_json,
                    );
                }
            };
            let removed = cleared["cleared"]["removed_files"].as_u64().unwrap_or(0);
            let bytes = cleared["cleared"]["removed_bytes"].as_f64().unwrap_or(0.0);
            let unknown = cleared["cleared"]["left_unknown"].as_u64().unwrap_or(0);
            let mut text = format!("Cleared the usage history: {removed} files, {} removed from {}\n", human_bytes(bytes), dir.display());
            if unknown > 0 {
                text.push_str(&format!("{unknown} other files in that directory are not SysMon's and were left alone\n"));
            }
            reply(cleared, text)
        }
        StoreOp::Pack => {
            let result = match sysmon_core::history::Recorder::open(&dir, limits, "pack") {
                Ok(mut recorder) => {
                    let report = recorder.maintain_blocking(sysmon_core::history::PackScope::AllSealed);
                    json!({"op": "pack", "packed": report, "store": recorder.status(), "by": "cli"})
                }
                Err(OpenError::Busy(_)) => match via_recorder("pack") {
                    Ok(result) => result,
                    Err(exit) => return exit,
                },
                Err(OpenError::Io(error)) => {
                    return envelope::fail_forced(
                        format!("cannot open {}: {error}", dir.display()),
                        "check the directory's permissions",
                        EXIT_RUNTIME,
                        asked_for_json,
                    );
                }
            };
            let packed = &result["packed"];
            let mut text = format!(
                "Packed {} hours into {} day files: {} of hourly files → archive now {}\n",
                packed["packed_hours"].as_u64().unwrap_or(0),
                packed["packed_days"].as_u64().unwrap_or(0),
                human_bytes(packed["packed_from_bytes"].as_f64().unwrap_or(0.0)),
                human_bytes(result["store"]["archive_bytes"].as_f64().unwrap_or(0.0)),
            );
            if let Some(problems) = packed["problems"].as_array() {
                for problem in problems {
                    text.push_str(&format!("problem: {}\n", problem.as_str().unwrap_or("")));
                }
            }
            reply(result, text)
        }
    }
}

// ----------------------------------------------------------------- schema

pub fn run_schema(_arguments: &[String]) -> i32 {
    let schema = build_schema();
    match serde_json::to_string_pretty(&schema) {
        Ok(pretty) => out!("{pretty}"),
        Err(_) => out!("{schema}"),
    }
    EXIT_OK
}

/// The machine map. `schema` is a one-shot, so it carries the
/// envelope like every other one-shot (workspace standard ruling
/// R5) — the four head fields sit alongside the contract, so a
/// reader that already parses `.tool` / `.commands` is unaffected
/// and a reader that checks `.status` / `.ts` is now satisfied too.
fn build_schema() -> Value {
    let epoch = envelope::now_ts();
    // The enums are READ from the code that enforces them, never
    // retyped here — a schema that can drift from the binary is
    // worse than no schema, because an agent will believe it.
    let section_enum: Vec<&str> = SECTION_NAMES.split(' ').collect();
    let theme_enum: Vec<&str> = std::iter::once("system")
        .chain(crate::gui::theme::PALETTES.iter().map(|palette| palette.id))
        .collect();
    let palette_enum: Vec<&str> = crate::gui::theme::GRAPH_PALETTES
        .iter()
        .map(|palette| palette.id)
        .collect();
    let popout_enum: Vec<&str> = crate::gui::settings::SECTION_KEYS.to_vec();
    let spec_section_enum: Vec<&str> = sysmon_core::specs::SPEC_SECTIONS.to_vec();
    let history_section_enum: Vec<&str> =
        std::iter::once("all").chain(sysmon_core::history::HISTORY_SECTIONS.iter().copied()).collect();
    // Its own value: one json! for the whole map passes the macro's
    // recursion limit.
    let history_verb = json!({
                    "usage": "sysmon history [section] [--since D] [--until D] [--specs] [--records] [--blocks N] [--raw] [--json] | sysmon history --info | --export PATH [--force] | --clear --yes | --pack",
                    "kind": "one-shot (NDJSON stream with --records)",
                    "description": "what the machine DID over a time range, from the usage history on disk (recorded while the GUI runs, or by `sysmon serve --record`). Two tiers, read as one: hourly gzip files for the last 7 days (settings: history_hot_hours), then one losslessly packed zstd file per day under archive/, kept until the user removes them (nothing is deleted by age or size unless a keep or cap is set). Per reading: min, avg, p95, max, latest and a timeline; for the GPU, every program that held it with peak/avg VRAM and busy share. A terminal gets the plain text a pop-out's copy button copies; piped, the envelope. Needs no running instance. --info/--export/--clear/--pack manage the whole store (see store_ops); when a recorder holds the writer lock, --clear and --pack reach it over the socket's `history` verb ({\"verb\":\"history\",\"op\":\"clear\"|\"pack\",\"yes\":true}).",
                    "arguments": {
                        "section": {"enum": history_section_enum, "optional": true, "default": "all"},
                        "--since": {"type": "string", "default": "1h", "description": "how far back: 90s 15m 2h 7d 1y, plain seconds, or a unix time"},
                        "--until": {"type": "string", "default": "0s", "description": "the range's end, as far back as --since reads"},
                        "--specs": {"type": "flag", "description": "put the section's specs above the usage: exactly what its pop-out's copy button copies (needs a section)"},
                        "--records": {"type": "flag", "description": "instead of a summary, every raw record in the range as NDJSON, one per line, each with `event`: \"history\" (scoped to the section's keys)"},
                        "--blocks": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 48, "description": "timeline resolution"},
                        "--raw": {"type": "flag", "description": "summarize record by record instead of merging sealed hours from their rollups (same numbers, slower: a check, not a mode)"},
                        "--info": {"type": "flag", "description": "the store: size per tier, how far back, growth per month and year at the recorded cadence, what removes data"},
                        "--export": {"type": "string", "description": "write the whole history to one .tar.zst (a file path, or a directory for sysmon-history-<date>.tar.zst); restore with `tar --zstd -xf FILE -C ~/.local/state/sysmon`"},
                        "--force": {"type": "flag", "description": "with --export: replace an existing file"},
                        "--clear": {"type": "flag", "description": "delete the whole history (hourly files, rollups, archive days; nothing else in the directory, nothing outside it). Refused without --yes"},
                        "--yes": {"type": "flag", "description": "confirms --clear"},
                        "--pack": {"type": "flag", "description": "pack every sealed hour into the archive now, instead of when its day leaves the hot window"},
                        "--json": {"type": "flag", "description": "force the envelope on a terminal"},
                    },
                    "store_ops": {
                        "info": "result {op, store: {dir, bytes, files, hot_bytes, hot_files, archive_bytes, archive_days, oldest_ts?, newest_ts?, cadence_seconds?, packed_bytes_per_sample?, packed_basis?: archive|estimate}, growth: {cadence_seconds, bytes_per_sample, per_month_bytes, per_year_bytes, basis} | null, limits: {hot_seconds, keep_seconds | null, max_bytes | null}, recorder, warn_bytes, text}",
                        "export": "result {op, export: {path, files, source_bytes, bytes, oldest_ts?, newest_ts?}, text}; exit 3 if the file exists (no --force) or lies inside the history directory",
                        "clear": "result {op, cleared: {dir, removed_files, removed_bytes, left_unknown}, by: cli|recorder|files, text}; exit 3 without --yes (the error names what would go)",
                        "pack": "result {op, packed: {packed_hours, packed_days, packed_from_bytes, archive_written_bytes, rollups_built, torn_lines, removed_by_keep, removed_by_cap, removed_bytes, cancelled, problems?}, store, by, text}",
                    },
                    "result": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["section", "from_ts", "to_ts", "range_seconds", "samples", "covered_seconds", "timeline_block_seconds", "metrics", "skipped_lines", "rollup_hours", "store", "recorder", "text"],
                        "properties": {
                            "section": {"type": "string"},
                            "from_ts | to_ts": {"type": "number", "description": "the range asked for, unix seconds"},
                            "range_seconds": {"type": "number"},
                            "samples": {"type": "integer", "description": "records in the range"},
                            "covered_seconds": {"type": "number", "description": "how much of the range the samples cover (each record's window, never more than the gap before it): less than range_seconds means SysMon was not recording part of the time"},
                            "cadence_seconds": {"type": "number", "description": "median gap between samples"},
                            "first_ts | last_ts": {"type": "number"},
                            "timeline_block_seconds": {"type": "number"},
                            "metrics": {"type": "array", "description": "[{key (`<section>.<reading>_<unit>`, e.g. gpu.vram_used_bytes, sensors.k10temp Tctl_celsius), label, unit (percent|bytes|bps|celsius|watts|mhz|rpm|load), samples, min, avg, p95, max, latest, total_bytes? (rates: bytes moved over the range), ceiling? (100 for a percent, the card/RAM size for memory), timeline: [mean per block | null where nothing was recorded]}]. min/avg/max/latest exact; p95 from a log-bucket sketch, within 0.5% of the exact nearest-rank p95"},
                            "gpu_processes": {"type": "array", "description": "[{name, pids[], samples, present_percent, vram_avg_bytes, vram_p95_bytes, vram_max_bytes, busy_avg_percent, busy_p95_percent, busy_max_percent, gtt_max_bytes}] — programs grouped by name per sample, biggest peak VRAM first. A buffer two programs share counts in both"},
                            "nvidia_processes": {"type": "array", "description": "the NVIDIA card's programs, the same shape (sections nvidia and all; new in 3.7)"},
                            "skipped_lines": {"type": "integer", "description": "unreadable lines on disk (a torn tail after a crash), reported rather than hidden"},
                            "rollup_hours": {"type": "integer", "description": "hours merged whole from their rollups instead of read record by record (0 with --raw); the numbers are the same either way"},
                            "store": {"type": "object", "description": "the whole history on disk: as `store` in store_ops.info"},
                            "recorder": {"type": ["string", "null"], "description": "who writes the history right now (\"pid 4242 gui\"), null when nothing records"},
                            "hint": {"type": "string", "description": "present when the range holds no samples"},
                            "text": {"type": "string", "description": "the plain-text summary; with --specs, byte-for-byte a pop-out's copy (the clipboard)"},
                        },
                    },
                    "exits": [0, 2, 3, 4],
                    "exit_2": "no history on disk at all (nothing has recorded yet); the fix names how to start recording. --clear/--pack: a recorder holds the lock and could not act (an older SysMon, or a paused window)",
                });
    json!({
        "status": "ok",
        "tool": envelope::TOOL,
        "version": sysmon_core::VERSION,
        "ts": envelope::iso8601(epoch),
        "ts_epoch": epoch,
        "contract": {
            "convention": "https://github.com/RamenFast/sysmon — workspace agent-first CLI standard (envelope, fix-bearing errors, exits 0/2/3/4, isatty auto-switch, strict schema)",
            "envelope": {
                "applies_to": "every one-shot REPLY (probe, ctl, and the socket's line replies). `schema` is the one-shot that describes the tool rather than answering about it: it carries the same four head fields, then its own documented top-level keys (contract, commands, sections, notes) in place of `result`.",
                "shape": {"status": "ok|error", "tool": "sysmon", "version": "semver",
                          "ts": "ISO-8601 with UTC offset, e.g. 2026-08-01T22:14:07+00:00",
                          "ts_epoch": "the same instant as unix seconds (extra, never a replacement)",
                          "result": "on ok", "error": "on error",
                          "fix": "ALWAYS present on error",
                          "exit": "on error: the exit code this failure means"},
                "json_schema": {
                    "$comment": "one reply envelope. `result` is present exactly when status is ok; `error`, `fix` and `exit` exactly when it is error — see allOf.",
                    "type": "object",
                    "required": ["status", "tool", "version", "ts"],
                    "additionalProperties": false,
                    "allOf": [
                        {"if": {"properties": {"status": {"const": "ok"}}},
                          "then": {"required": ["result"]}},
                        {"if": {"properties": {"status": {"const": "error"}}},
                          "then": {"required": ["error", "fix", "exit"]}},
                    ],
                    "properties": {
                        "status": {"enum": ["ok", "error"]},
                        "tool": {"const": "sysmon"},
                        "version": {"type": "string"},
                        "ts": {"type": "string", "format": "date-time"},
                        "ts_epoch": {"type": "number"},
                        "result": {"type": "object"},
                        "error": {"type": "string"},
                        "fix": {"type": "string"},
                        "exit": {"type": "integer", "enum": [2, 3, 4]},
                    },
                },
                "exit_codes": {"0": "ok", "2": "unavailable (no daemon / feature absent)",
                                "3": "bad arguments", "4": "runtime failure"},
                "json_output": "automatic when stdout is piped; --json forces it",
            },
            "streams": {
                "shape": "NDJSON — one raw snapshot object per line, no envelope around stream lines",
                "event": "every line carries `event`: \"snapshot\"",
                "json_schema": {
                    "$comment": "additionalProperties is true on purpose and declared as such: the section keys present on a line are exactly the sections the caller subscribed to, so they cannot be enumerated in advance. The named fields below are on every line.",
                    "type": "object",
                    "required": ["event", "ts", "interval_seconds"],
                    "additionalProperties": true,
                    "properties": {
                        "event": {"const": "snapshot"},
                        "ts": {"type": "number", "description": "the sample instant, unix seconds"},
                        "interval_seconds": {"type": "number", "description": "the window every rate in this line spans"},
                    },
                },
                "producers": ["sysmon tap [sections…] [--interval N]",
                               "{\"verb\":\"subscribe\",\"sections\":[…],\"interval\":N} on the socket"],
            },
            "verbs": {
                "probe": {
                    "usage": "sysmon probe [sections…] [--json]",
                    "kind": "one-shot",
                    "arguments": {
                        "sections": {"type": "array", "items": {"enum": section_enum},
                                      "default": ["all"]},
                        "--json": {"type": "flag", "description": "force the envelope on a terminal"},
                    },
                    "result": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["ts", "interval_seconds", "via"],
                        "properties": {
                            "ts": {"type": "number"},
                            "interval_seconds": {"type": "number"},
                            "via": {"enum": ["socket", "direct"],
                                     "description": "socket = a live instance answered (longer, smoother window); direct = sampled in-process, twice, 250 ms apart"},
                            "system": {"type": ["object", "null"]},
                            "cpu": {"type": ["object", "null"]},
                            "memory": {"type": ["object", "null"]},
                            "gpu": {"type": ["object", "null"]},
                            "nvidia": {"type": ["object", "null"]},
                            "network": {"type": ["object", "null"]},
                            "disks": {"type": ["array", "null"]},
                            "processes": {"type": ["array", "null"]},
                            "sensors": {"type": ["object", "null"]},
                            "connections": {"type": ["array", "null"]},
                        },
                    },
                    "exits": [0, 3, 4],
                },
                "probe threads": {
                    "usage": "sysmon probe threads --pid <pid> [--json]",
                    "kind": "one-shot",
                    "since": "3.8.0",
                    "description": "one process, thread by thread, and which logical CPUs its time landed on; always read in-process over a 1 s window",
                    "arguments": {
                        "--pid": {"type": "integer", "required": true},
                        "--json": {"type": "flag", "description": "force the envelope on a terminal"},
                    },
                    "result": {
                        "type": "object",
                        "required": ["threads", "via"],
                        "properties": {
                            "via": {"enum": ["direct"]},
                            "threads": {
                                "type": "object",
                                "required": ["pid", "name", "interval_seconds", "cpu_count", "cpu_percent", "per_cpu_percent", "cpu_attribution", "threads"],
                                "properties": {
                                    "pid": {"type": "integer"},
                                    "name": {"type": "string"},
                                    "interval_seconds": {"type": "number"},
                                    "cpu_count": {"type": "integer", "description": "logical CPUs (configured); the length of per_cpu_percent"},
                                    "cpu_percent": {"type": "number", "description": "the threads' sum; 100 = one whole CPU"},
                                    "per_cpu_percent": {"type": "array", "items": {"type": "number"}, "description": "this process's share of each logical CPU, by cpu_attribution"},
                                    "cpu_attribution": {"enum": ["last_cpu"], "description": "each thread's time over the window is placed on the CPU it last ran on (stat field 39, as ps -o psr / htop); the kernel keeps no per-CPU split"},
                                    "allowed_cpus": {"type": "string", "description": "Cpus_allowed_list, present only when it narrows the CPUs (a pinned / taskset process)"},
                                    "threads": {"type": "array", "description": "busiest first: {tid, name, state, state_word, cpu_percent, cpu?, cpu_time_seconds}"},
                                },
                            },
                        },
                    },
                    "exits": [0, 3, 4],
                },
                "tap": {
                    "usage": "sysmon tap [sections…] [--interval N]",
                    "kind": "stream",
                    "arguments": {
                        "sections": {"type": "array", "items": {"enum": section_enum},
                                      "default": ["network"]},
                        "--interval | -i": {"type": "number", "minimum": 0.2, "maximum": 60.0,
                                             "default": 1.0, "unit": "seconds"},
                    },
                    "output": "NDJSON, see contract.streams",
                    "exits": [0, 3, 4],
                },
                "ctl": {
                    "usage": "sysmon ctl <verb> [value] [--json]",
                    "kind": "one-shot",
                    "needs": "a running instance (GUI or `sysmon serve`) — else exit 2",
                    "verbs": {
                        "status":   {"value": null, "needs_gui": false,
                                      "result": {"type": "object",
                                                  "$comment": "additionalProperties is true by declaration: each backend merges its own status fields (a GUI adds renderer, serve adds sampling).",
                                                  "additionalProperties": true,
                                                  "required": ["running", "pid", "version", "mode"],
                                                  "properties": {
                                                      "running": {"const": true},
                                                      "pid": {"type": "integer"},
                                                      "version": {"type": "string"},
                                                      "mode": {"enum": ["gui", "serve"]},
                                                      "history": {"type": "object", "description": "the usage history recorder: {recording, dir, bytes, hot_bytes, archive_bytes, archive_days, oldest_ts?, newest_ts?, hot_seconds, keep_seconds (null: forever), max_bytes (null: no cap), growth?: {cadence_seconds, bytes_per_sample, per_month_bytes, per_year_bytes, basis}, packing, problem?, warning? (past 1 GB with no cap)}. GUI always; serve only with --record"},
                                                      "renderer": {"type": "string", "description": "GUI only: the adapter the window renders on"},
                                                      "renderer_hint": {"type": "string", "description": "present only when degraded to a CPU rasterizer — the fix"},
                                                  }}},
                        "quit":     {"value": null, "needs_gui": false},
                        "pause":    {"value": null, "needs_gui": true},
                        "resume":   {"value": null, "needs_gui": true},
                        "interval": {"value": {"type": "number", "minimum": 0.2, "maximum": 60.0}, "needs_gui": true},
                        "raise":    {"value": null, "needs_gui": true},
                        "page":     {"value": {"enum": ["overview", "processes"]}, "needs_gui": true,
                                      "note": "3.6 folded the Performance page into the Overview; `performance` exits 3 with the fix"},
                        "cpugraph": {"value": {"enum": ["total", "threads", "grid"]}, "needs_gui": true,
                                      "note": "the Overview CPU card's view: total = one trace (busy + kernel); threads = a coloured line per thread (default); grid = a small graph per thread"},
                        "inspect":  {"value": {"type": "integer", "description": "a live pid"}, "needs_gui": true,
                                      "note": "3.8: the Processes page with the Inspector on that pid (its Threads & CPUs included); exits 3 when the pid isn't in the latest sample"},
                        "details":  {"value": {"type": "integer", "description": "a live pid"}, "needs_gui": true,
                                      "note": "3.8: that pid's own details window (identity, Threads & CPUs, connections)"},
                        "theme":    {"value": {"enum": theme_enum}, "needs_gui": true},
                        "palette":  {"value": {"enum": palette_enum}, "needs_gui": true},
                        "popout":   {"value": {"enum": popout_enum}, "needs_gui": true},
                        "popin":    {"value": {"enum": popout_enum}, "needs_gui": true},
                        "shot":     {"value": {"type": "string", "optional": true, "description": "output path; defaults under $XDG_RUNTIME_DIR/sysmon/shots/"},
                                      "needs_gui": true,
                                      "note": "BLOCKS until the PNG exists; result.path names it. Captures the main viewport only — pop-outs are separate OS windows"},
                        "compact":  {"value": {"enum": ["on", "off"]}, "needs_gui": true},
                        "units":    {"value": {"enum": ["decimal", "binary"]}, "needs_gui": true},
                        "temperature": {"value": {"enum": ["celsius", "fahrenheit", "both"]}, "needs_gui": true,
                                         "note": "display only: every JSON reading stays in °C (`*_celsius`)"},
                    },
                    "exits": [0, 2, 3, 4],
                },
                "serve": {
                    "usage": "sysmon serve [--record [--interval N]]",
                    "kind": "daemon",
                    "description": "headless owner of the control socket; samples only when asked — idle cost is zero. --record also keeps the usage history on disk (its own sampler at --interval, default the settings' update interval), for `sysmon history` without a window open",
                    "arguments": {
                        "--record": {"type": "flag", "description": "write the usage history (one writer at a time: exit 2 if a window or another serve already records)"},
                        "--interval | -i": {"type": "number", "minimum": 0.5, "maximum": 60.0, "unit": "seconds", "description": "the recorder's cadence"},
                    },
                    "exits": [0, 2, 3, 4],
                },
                "history": history_verb,
                "schema": {
                    "usage": "sysmon schema",
                    "kind": "one-shot",
                    "description": "this document — always current, generated from the binary",
                    "exits": [0],
                },
                "specs": {
                    "usage": "sysmon specs [section] [--json]",
                    "kind": "one-shot",
                    "description": "what this machine IS (hardware, firmware, driver stack), not what it is doing. A terminal gets the plain-text sheet the GUI's copy buttons copy; piped, the envelope. Read on demand from sysfs/udev plus optional helpers (vulkaninfo, glxinfo, swaymsg, pactl) under a 3 s deadline each; needs no running instance. A missing source leaves its field absent, never zero; a section that cannot be read at all is listed in `unavailable`.",
                    "arguments": {
                        "section": {"enum": spec_section_enum, "optional": true,
                                     "description": "scopes `result.text` to one section; the structured fields are always whole"},
                        "--json": {"type": "flag", "description": "force the envelope on a terminal"},
                    },
                    "result": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["ts", "gpus", "displays", "drives", "network", "unavailable", "section", "text"],
                        "properties": {
                            "ts": {"type": "number", "description": "unix seconds the specs were read"},
                            "section": {"type": "string", "description": "the section `text` covers, or \"all\""},
                            "text": {"type": "string", "description": "the shareable sheet, exactly what the copy button copies. `sysmon specs --json | jq -j .result.text` reproduces the clipboard. Leaves out the hostname, serials, addresses and mount paths"},
                            "system": {"type": "object", "description": "os, os_base?, kernel, architecture, hostname (JSON only, never in text), session_type?, desktop?, model?, board?, firmware?, firmware_date? (ISO), boot_mode (UEFI|BIOS), secure_boot?"},
                            "cpu": {"type": "object", "description": "model?, sockets?, cores? (physical), threads (logical), base_mhz? (ACPI CPPC nominal), max_mhz?, boost_enabled?, cache_l1d_bytes? | cache_l1i_bytes? | cache_l2_bytes? | cache_l3_bytes? (machine totals, shared caches counted once), frequency_driver?, governor?, energy_preference?, features[]"},
                            "memory": {"type": "object", "description": "modules[] (the memory section's module shape, plus form_factor?, rank?, manufacturer?), slots?, installed_bytes?, usable_bytes (MemTotal), swap_bytes"},
                            "gpus": {"type": "array", "description": "[{name, pci_id, pci_address, driver?, vram_bytes?, architecture? (AMD ISA e.g. gfx1201, NVIDIA compute capability e.g. sm_86), family? (e.g. RDNA 4, Ampere), compute_units? (AMD), cuda_cores? (NVIDIA, 3.7), pcie_link? {generation, width, device_max_generation?, device_max_width?} (the slowest hop to the CPU), power_cap_watts?, power_cap_max_watts?, vulkan? {version, driver}, opengl? {version, driver}, compute_stack?}] — every PCI display device; software renderers are never listed"},
                            "displays": {"type": "array", "description": "[{connector, make?, model?, diagonal_inches?, width_px?, height_px?, refresh_hz?, scale?, mode_source: active (the compositor's running mode) | preferred (EDID only — the panel's preference, not necessarily in use)}]"},
                            "drives": {"type": "array", "description": "[{name, model?, kind (NVMe SSD | SATA SSD | SATA HDD | USB | SD/eMMC), size_bytes, rpm?, link?, removable, filesystems: [{device, fs_type?, mount_points[]}]}] — physical drives, mounted or not"},
                            "network": {"type": "array", "description": "[{name, kind (Ethernet | Wi-Fi), model?, driver?, link_up, speed_mbps?, duplex?}] — physical interfaces only, no addresses"},
                            "audio": {"type": "object", "description": "server?, devices[] (ALSA card names)"},
                            "unavailable": {"type": "array", "description": "[{section, reason}] for sections that could not be read at all"},
                        },
                    },
                    "exits": [0, 3, 4],
                },
            },
            "socket": {
                "path": control::socket_path(),
                "protocol": "one JSON object per line in; one envelope line out; `subscribe` upgrades to a raw NDJSON snapshot stream",
                "verbs": ["status", "snapshot", "subscribe", "pause", "resume", "interval",
                           "quit", "raise", "page", "theme", "palette", "popout", "popin",
                           "shot", "compact", "units", "temperature", "cpugraph", "inspect", "details"],
                "single_owner": "GUI or serve — whoever binds first; `sysmon serve` exits 2 if occupied",
            },
        },
        "commands": {
            "sysmon": "GUI (default command); plain re-launch raises the running instance",
            "sysmon --background": "GUI on a private Xvfb display — renders and serves the socket without touching your screen",
            "sysmon probe [sections…] [--json]": "one-shot snapshot; asks the live instance, else samples in-process (two samples 250 ms apart — 1 s when `processes` is asked for, so per-process CPU% moves in 1% steps, not 4%)",
            "sysmon tap [sections…] [--interval N]": "NDJSON stream, one snapshot per line (default section: network, 1 s cadence)",
            "sysmon ctl <verb> […]": "drive the live instance",
            "sysmon serve": "headless daemon; samples only when asked — idle cost is zero",
            "sysmon schema": "this document",
            "sysmon specs [section] [--json]": "what this machine is: the shareable spec sheet (the copy button's text), or every field as JSON when piped",
            "sysmon history [section] [--since 1h] [--specs] [--records]": "what this machine did: usage history from disk, summarized (min/avg/p95/max, timeline, the GPU's programs); --specs is a pop-out's copy byte for byte",
            "sysmon serve --record": "headless daemon that also keeps the usage history",
        },
        "sections": {
            "system": {"hostname": "string", "kernel": "string",
                        "uptime_seconds": "f64", "boot_ts": "unix seconds"},
            "cpu": {
                "overall_percent": "f32 0–100, mean of cores over the window",
                "per_core_percent": "[f32]",
                "per_core_top": "[[{pid, name, cpu_percent}]] per logical CPU, up to 3 busiest processes (threads summed, each thread on the CPU it last ran on). Only with section `occupants` (3.8): it walks every thread on the machine",
                "core_count": "logical cores",
                "frequency_mhz": "mean of per-core current frequencies (cpufreq)",
                "frequency_busy_mhz": "the clock the working cores ran at: each core's delivered clock over the window (ACPI CPPC, the APERF/MPERF ratio turbostat's Bzy_MHz reads) weighted by its busy share",
                "frequency_busy_source": "\"delivered over the window\" or \"instant read\" (no CPPC: busy-weighted scaling_cur_freq, which misses clock changes inside the window)",
                "frequency_min_mhz | frequency_max_mhz": "hardware limits",
                "load_1m | load_5m | load_15m": "loadavg",
                "tasks_running | tasks_total": "loadavg 4th field (kernel tasks = threads)",
                "temperature_celsius": "CPU package (k10temp Tdie/Tctl or coretemp Package)",
                "temperature_source": "which sensor that is, e.g. \"k10temp Tctl\"",
                "iowait_percent": "idle time spent waiting on storage, 0–100 — not included in overall_percent",
                "kernel_percent": "the kernel's own share of the window (system + irq + softirq), 0–100 — inside overall_percent, never above it",
                "context_switches_per_second": "machine-wide",
            },
            "memory": {
                "total_bytes | free_bytes | available_bytes": "MemTotal / MemFree / MemAvailable",
                "installed_bytes": "physical RAM fitted (sum of DMI memory devices); total_bytes is what the kernel can use after firmware/GPU reservations",
                "modules": "[{locator, size_bytes, kind (DDR4…), rated_speed_mts, configured_speed_mts, part_number, form_factor?, rank?, manufacturer? (the firmware's name, else the maker its JEDEC id belongs to)}] from /run/udev/data/+dmi:id; empty when DMI is unreadable",
                "used_bytes": "total − available (psutil identity; htop shows a smaller figure by design)",
                "used_percent": "used / total",
                "cached_bytes": "Cached + SReclaimable",
                "buffers_bytes | dirty_bytes | shared_bytes": "meminfo verbatim",
                "swap_total_bytes | swap_used_bytes | swap_cached_bytes": "swap",
                "committed_bytes | commit_limit_bytes": "Committed_AS / CommitLimit — the commit charge; the limit is advisory under the default overcommit heuristic",
                "slab_bytes | kernel_stack_bytes | page_tables_bytes": "kernel memory: Slab (whole; its reclaimable part is also inside cached_bytes) / KernelStack / PageTables",
            },
            "gpu": {
                "available": "false ⇒ no amdgpu card (all other fields empty)",
                "device_name": "marketing name via lspci",
                "busy_percent": "gpu_busy_percent",
                "vram_used_bytes | vram_total_bytes": "dedicated VRAM",
                "gtt_used_bytes | gtt_total_bytes": "system RAM mapped by the GPU",
                "temperature_edge_celsius | temperature_junction_celsius | temperature_memory_celsius": "amdgpu hwmon, matched by label (edge / junction / mem), never by index",
                "temperature_vrm_gfx_celsius | temperature_vrm_soc_celsius | temperature_vrm_mem_celsius": "voltage-regulator temperatures from gpu_metrics (absent when the table doesn't report them)",
                "power_draw_watts | power_cap_watts": "hwmon power1",
                "core_clock_mhz | memory_clock_mhz": "mean across the sample window (the clock changes every few ms, 0 when gated)",
                "clock_source": "\"mean of N reads\" or \"instant read\" (a window's first sample)",
                "fan_rpm | fan_max_rpm": "hwmon fan1",
                "processes": "[{pid, busy_percent, vram_bytes, gtt_bytes, vram_shared_bytes, vram_evicted_bytes}] via DRM fdinfo, each client once, busiest first. vram_bytes = drm-total-vram − amd-evicted-vram (buffers in VRAM now; never drm-memory-vram, which drifts to terabytes on kernel 7.2). A shared buffer counts in every process that maps it, so the sum can exceed vram_used_bytes by the shared part",
            },
            "nvidia": {
                "(shape)": "the gpu section's shape, read through NVML (libnvidia-ml.so.1, opened at run time); the first NVIDIA card. New in 3.7",
                "available": "false ⇒ no NVIDIA card, driver or libnvidia-ml (all other fields empty)",
                "device_name": "NVML's name, verbatim (\"NVIDIA GeForce RTX 3070\")",
                "busy_percent": "nvmlDeviceGetUtilizationRates .gpu: share of the driver's last sample period a kernel ran (nvidia-smi utilization.gpu)",
                "memory_busy_percent": "the same call's .memory: device memory read or written (nvidia-smi utilization.memory)",
                "vram_used_bytes | vram_total_bytes": "nvmlMemory_v2: used leaves out the driver's reserve, as nvidia-smi's Used row does",
                "vram_reserved_bytes": "the driver and firmware's reserve (nvidia-smi's Reserved row); absent on a driver without nvmlMemory_v2, whose used then includes it",
                "temperature_core_celsius": "the die sensor (NVML_TEMPERATURE_GPU)",
                "power_draw_watts | power_cap_watts": "nvmlDeviceGetPowerUsage (a 1 s average on Ampere and newer) / the enforced power limit",
                "core_clock_mhz | memory_clock_mhz": "current graphics / memory clock, one read (clock_source \"instant read\")",
                "fan_percent | fan_rpm": "the fan's target speed, % / its RPM where the driver reports it",
                "processes": "[{pid, busy_percent, vram_bytes}] from NVML's compute + graphics process lists (a pid in both counts its memory once); busy = the driver's newest per-process sample since the previous read (0 on the first). vram_bytes is 0 where NVML withholds it (another user's process)",
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
            "processes": "[{pid, ppid, name (kernel comm, 15 chars), display_name (what to show: a generic thread name like MainThread resolved to its executable, python3 to its script), user, state, state_word, is_kernel_thread, cpu_percent (can exceed 100 when multithreaded), memory_rss_bytes (statm, matches ps), memory_virtual_bytes, threads, nice, started_ts, cpu_time_seconds, disk_read_bps?, disk_write_bps? (None = unreadable, not zero), gpu_busy_percent, gpu_vram_bytes, net_rx_bps?, net_tx_bps?, command_line, exe_basename?}]",
            "sensors": {
                "chips": "[{name, kind: cpu|gpu|drive|board|battery|other, device? (block dev a drive chip measures, e.g. sda), device_model?, temps: [{label, celsius, plausible (false for an unconnected input: -62 °C or exactly 0 °C with no limits), max_celsius?, crit_celsius? (firmware sentinels dropped)}], fans: [{label, rpm, max_rpm?, duty_percent? (the pwm drive; duty > 0 with rpm 0 means a stalled or unplugged fan)}], voltages?: [{label, volts}], power?: [{label, watts, cap_watts?}]}] — every hwmon chip (indices scanned, gaps honored)",
                "battery": "{name, percent, status, power_draw_watts?, seconds_remaining?} when present",
            },
            "connections": "[{pid (-1 = unresolved), process_name?, protocol tcp|tcp6|udp|udp6, local_address, remote_address, state, rx_bps?, tx_bps? (TCP only)}] — the full socket table",
        },
        "notes": {
            "rates": "every *_bps/_percent rate spans `interval_seconds`, reported per snapshot; each collector tracks its own window so mixed-section clients never skew each other",
            "first_sample": "a fresh sampler's first snapshot has zero rates (nothing to delta against)",
            "bar_integration": "poll `sysmon tap network --interval 2`; on click, `sysmon probe network --json | jq .result.network.top_processes` and `sysmon probe connections`",
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
    let line = |label: &str, value: String| out!("  {label:<14} {value}");

    if let Some(system) = snapshot.get("system").filter(|s| !s.is_null()) {
        out!("system");
        line("hostname", system["hostname"].as_str().unwrap_or("?").to_string());
        line("kernel", system["kernel"].as_str().unwrap_or("?").to_string());
        line(
            "uptime",
            units::format_duration_seconds(value_f64(&system["uptime_seconds"])),
        );
    }
    if let Some(cpu) = snapshot.get("cpu").filter(|s| !s.is_null()) {
        out!("cpu");
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
        if let Some(per_core) = cpu["per_core_top"].as_array() {
            // Busiest CPUs first, each naming who is on it.
            let mut rows: Vec<(usize, f64, String)> = per_core
                .iter()
                .enumerate()
                .filter_map(|(index, occupants)| {
                    let occupants = occupants.as_array()?;
                    let busy = value_f64(&cpu["per_core_percent"][index]);
                    let names: Vec<String> = occupants
                        .iter()
                        .map(|o| format!("{} {:.0}%", o["name"].as_str().unwrap_or("?"), value_f64(&o["cpu_percent"])))
                        .collect();
                    (!names.is_empty()).then(|| (index, busy, names.join(" · ")))
                })
                .collect();
            rows.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (index, busy, names) in rows.iter().take(8) {
                line(&format!("cpu{index} {busy:.0}%"), names.clone());
            }
        }
    }
    if let Some(memory) = snapshot.get("memory").filter(|s| !s.is_null()) {
        out!("memory");
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
        out!("gpu");
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
    if let Some(gpu) = snapshot.get("nvidia").filter(|s| !s.is_null()) {
        out!("nvidia");
        if gpu["available"].as_bool() == Some(true) {
            line("device", gpu["device_name"].as_str().unwrap_or("?").to_string());
            let mut busy = units::format_percent(value_f64(&gpu["busy_percent"]) as f32);
            if let Some(core) = gpu["temperature_core_celsius"].as_f64() {
                busy += &format!(" · {}", units::format_temperature(core as f32));
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
            line("device", "no NVIDIA GPU on this machine".to_string());
        }
    }
    if let Some(network) = snapshot.get("network").filter(|s| !s.is_null()) {
        out!("network");
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
        out!("disks");
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
        out!("processes ({} — top 5 by cpu)", processes.len());
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
        out!("connections ({})", connections.len());
    }
    out!("(window {:.2}s · via {via})", value_f64(&snapshot["interval_seconds"]));
}
