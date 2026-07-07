// SPDX-License-Identifier: GPL-3.0-or-later
//! The control socket — a Unix-domain NDJSON server at
//! `$XDG_RUNTIME_DIR/sysmon/ctl.sock` (phosphor's pattern). One JSON
//! request line in, one envelope line out; `subscribe` upgrades the
//! connection to a raw snapshot stream. The same server fronts both
//! backends: `serve` (headless, samples on demand — zero idle cost)
//! and the GUI (answers from its live bundle and applies verbs).
//!
//! Single-owner: whoever binds the socket is *the* instance. A
//! second binder gets `AlreadyRunning` with the owner's status so it
//! can forward (GUI raise) or bow out with a useful message.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use sysmon_core::snapshot::Wants;

use crate::envelope;

/// (message, fix) — every backend error teaches the caller the way out.
pub type VerbError = (String, String);

pub trait Backend: Send + Sync {
    fn mode(&self) -> &'static str;
    /// Extra fields merged into the `status` reply.
    fn status(&self) -> Value;
    fn snapshot(&self, wants: Wants) -> Result<Value, VerbError>;
    fn set_paused(&self, paused: bool) -> Result<Value, VerbError>;
    fn set_interval(&self, seconds: f64) -> Result<Value, VerbError>;
    /// raise / page / theme / palette / popout / popin / shot — GUI only.
    fn gui_verb(&self, verb: &str, arguments: &Value) -> Result<Value, VerbError> {
        let _ = arguments;
        Err((
            format!("`{verb}` drives the GUI, and this is `sysmon serve`"),
            "launch the GUI first: `sysmon` (or `sysmon --background` for headless)".to_string(),
        ))
    }
    fn request_quit(&self);
}

pub fn socket_directory() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("sysmon");
    }
    let uid = unsafe { libc::geteuid() };
    PathBuf::from(format!("/tmp/sysmon-{uid}"))
}

pub fn socket_path() -> PathBuf {
    socket_directory().join("ctl.sock")
}

pub enum BindError {
    /// Someone already serves this socket; here's their status.
    AlreadyRunning(Value),
    Io(std::io::Error),
}

pub struct ControlServer {
    path: PathBuf,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<std::thread::JoinHandle<()>>,
}

impl ControlServer {
    pub fn bind(backend: Arc<dyn Backend>) -> Result<ControlServer, BindError> {
        let directory = socket_directory();
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory);
        let path = socket_path();

        // Is a live instance holding it?
        if path.exists() {
            match UnixStream::connect(&path) {
                Ok(stream) => {
                    let status = request_on(stream, &json!({"verb": "status"}))
                        .unwrap_or_else(|| json!({"status": "ok", "result": {"unreachable": true}}));
                    return Err(BindError::AlreadyRunning(status));
                }
                Err(_) => {
                    // Stale socket from a dead instance.
                    let _ = std::fs::remove_file(&path);
                }
            }
        }

        let listener = UnixListener::bind(&path).map_err(BindError::Io)?;
        let shutdown = Arc::new(AtomicBool::new(false));

        let accept_shutdown = shutdown.clone();
        let accept_thread = std::thread::Builder::new()
            .name("sysmon-ctl-accept".to_string())
            .spawn(move || {
                for stream in listener.incoming() {
                    if accept_shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let backend = backend.clone();
                    let connection_shutdown = accept_shutdown.clone();
                    let _ = std::thread::Builder::new()
                        .name("sysmon-ctl-conn".to_string())
                        .spawn(move || handle_connection(stream, backend, connection_shutdown));
                }
            })
            .map_err(BindError::Io)?;

        Ok(ControlServer {
            path,
            shutdown,
            accept_thread: Some(accept_thread),
        })
    }

    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Unblock accept() with a throwaway connection.
        let _ = UnixStream::connect(&self.path);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn handle_connection(stream: UnixStream, backend: Arc<dyn Backend>, shutdown: Arc<AtomicBool>) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return, // client hung up
            Ok(_) => {}
        }
        let request: Value = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(parse_error) => {
                let reply = envelope::error(
                    format!("request is not JSON: {parse_error}"),
                    "send one JSON object per line, e.g. {\"verb\":\"status\"}",
                );
                if writeln!(writer, "{reply}").is_err() {
                    return;
                }
                continue;
            }
        };
        let verb = request["verb"].as_str().unwrap_or("");

        if verb == "subscribe" {
            run_subscription(&mut writer, &backend, &request, &shutdown);
            return;
        }

        let reply = dispatch(verb, &request, &backend);
        let done = verb == "quit";
        if writeln!(writer, "{reply}").is_err() {
            return;
        }
        if done {
            backend.request_quit();
            return;
        }
    }
}

fn dispatch(verb: &str, request: &Value, backend: &Arc<dyn Backend>) -> Value {
    let result = match verb {
        "status" => {
            let mut status = json!({
                "running": true,
                "pid": std::process::id(),
                "version": sysmon_core::VERSION,
                "mode": backend.mode(),
            });
            if let (Some(base), Some(extra)) = (status.as_object_mut(), backend.status().as_object())
            {
                for (key, value) in extra {
                    base.insert(key.clone(), value.clone());
                }
            }
            Ok(status)
        }
        "snapshot" => wants_from_request(request).and_then(|wants| backend.snapshot(wants)),
        "pause" => backend.set_paused(true),
        "resume" => backend.set_paused(false),
        "interval" => match request["seconds"].as_f64() {
            Some(seconds) if (0.2..=60.0).contains(&seconds) => backend.set_interval(seconds),
            _ => Err((
                "interval needs `seconds` between 0.2 and 60".to_string(),
                "send {\"verb\":\"interval\",\"seconds\":2}".to_string(),
            )),
        },
        "quit" => Ok(json!({"quitting": true})),
        "raise" | "page" | "theme" | "palette" | "popout" | "popin" | "shot" | "compact"
        | "sections" | "units" => backend.gui_verb(verb, request),
        other => Err((
            format!("unknown verb `{other}`"),
            "verbs: status snapshot subscribe pause resume interval quit raise page theme \
             palette popout popin shot compact sections units"
                .to_string(),
        )),
    };
    match result {
        Ok(value) => envelope::ok(value),
        Err((message, fix)) => envelope::error(message, fix),
    }
}

/// `subscribe` turns the connection into a raw NDJSON snapshot
/// stream at the client's cadence — the client drives sampling, so
/// an idle daemon costs nothing.
fn run_subscription(
    writer: &mut UnixStream,
    backend: &Arc<dyn Backend>,
    request: &Value,
    shutdown: &Arc<AtomicBool>,
) {
    let wants = match wants_from_request(request) {
        Ok(wants) => wants,
        Err((message, fix)) => {
            let _ = writeln!(writer, "{}", envelope::error(message, fix));
            return;
        }
    };
    let interval = request["interval"]
        .as_f64()
        .unwrap_or(1.0)
        .clamp(0.2, 60.0);

    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        match backend.snapshot(wants) {
            Ok(snapshot) => {
                if writeln!(writer, "{snapshot}").is_err() {
                    return; // client gone
                }
            }
            Err((message, fix)) => {
                let _ = writeln!(writer, "{}", envelope::error(message, fix));
                return;
            }
        }
        std::thread::sleep(Duration::from_secs_f64(interval));
    }
}

fn wants_from_request(request: &Value) -> Result<Wants, VerbError> {
    let mut wants = Wants::none();
    let mut any = false;
    if let Some(sections) = request["sections"].as_array() {
        for section in sections {
            let name = section.as_str().unwrap_or("");
            match Wants::from_section_name(name) {
                Some(section_wants) => {
                    wants = wants.union(section_wants);
                    any = true;
                }
                None => {
                    return Err((
                        format!("unknown section `{name}`"),
                        "sections: all system cpu memory gpu network disks processes sensors \
                         connections"
                            .to_string(),
                    ));
                }
            }
        }
    }
    if !any {
        wants = Wants::all();
    }
    Ok(wants)
}

/// One request/reply on an already-connected stream.
pub fn request_on(stream: UnixStream, request: &Value) -> Option<Value> {
    let mut writer = stream.try_clone().ok()?;
    writer
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    writeln!(writer, "{request}").ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

/// Connect + one request/reply against the live instance, if any.
pub fn request(request_value: &Value) -> Option<Value> {
    let stream = UnixStream::connect(socket_path()).ok()?;
    request_on(stream, request_value)
}
