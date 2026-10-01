// SPDX-License-Identifier: GPL-3.0-or-later
//! The GUI's control-socket backend. Reads (`status`, `snapshot`,
//! `subscribe`) answer straight from the shared sampler state and
//! never wake the window — phosphor's law. Verbs that must touch the
//! UI (`raise`, `page`, `theme`, `popout`, `shot`, …) go through a
//! command queue with a one-shot reply channel; the update loop
//! drains it on the main thread, the one place UI state may change.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

use sysmon_core::snapshot::{SystemSnapshot, Wants};

use crate::control::{Backend, VerbError, socket_path};

use super::app::SharedUi;

/// A UI-touching verb waiting for the main thread.
pub struct GuiCommand {
    pub verb: String,
    pub value: Option<String>,
    pub path: Option<String>,
    pub reply: mpsc::Sender<Result<Value, VerbError>>,
}

pub struct GuiBackend {
    pub shared: Arc<SharedUi>,
    pub commands: Mutex<mpsc::Sender<GuiCommand>>,
    /// Set once the eframe context exists; verbs before that get a
    /// gentle "starting up".
    pub ctx: OnceLock<egui::Context>,
}

impl GuiBackend {
    pub fn new(shared: Arc<SharedUi>, commands: mpsc::Sender<GuiCommand>) -> Self {
        GuiBackend {
            shared,
            commands: Mutex::new(commands),
            ctx: OnceLock::new(),
        }
    }

    fn filtered_snapshot(&self, wants: Wants) -> Value {
        let snapshot: Arc<SystemSnapshot> = self.shared.latest.read().unwrap().clone();
        let mut value = snapshot.to_json_value().unwrap_or_else(|_| json!({}));
        if let Some(object) = value.as_object_mut() {
            let keep = |name: &str, wanted: bool, object: &mut serde_json::Map<String, Value>| {
                if !wanted {
                    object.remove(name);
                }
            };
            keep("system", wants.system, object);
            keep("cpu", wants.cpu, object);
            keep("memory", wants.memory, object);
            keep("gpu", wants.gpu, object);
            keep("network", wants.network, object);
            keep("disks", wants.disks, object);
            keep("processes", wants.processes, object);
            keep("sensors", wants.sensors, object);
            keep("connections", wants.connections, object);
        }
        value
    }
}

impl Backend for GuiBackend {
    fn mode(&self) -> &'static str {
        "gui"
    }

    fn status(&self) -> Value {
        let mut payload = json!({
            "socket": socket_path(),
            "paused": self.shared.paused.load(Ordering::Relaxed),
            "interval_seconds": *self.shared.interval_seconds.lock().unwrap(),
        });
        // Disclosed like process_source/_hint: the hint key exists
        // only in the degraded mode, and names the fix.
        if let Some(renderer) = self.shared.renderer.get() {
            payload["renderer"] = json!(renderer.description);
            if renderer.degraded {
                payload["renderer_hint"] = json!(
                    "CPU rasterizer — no GPU acceleration; install Vulkan \
                     drivers (e.g. mesa-vulkan-drivers) and relaunch"
                );
            }
        }
        payload
    }

    fn snapshot(&self, wants: Wants) -> Result<Value, VerbError> {
        // The connection table is only sampled while someone wants
        // it; flip the flag and give the sampler one tick to comply.
        if wants.connections {
            self.shared.connections_wanted.store(true, Ordering::Relaxed);
            let deadline = std::time::Instant::now()
                + Duration::from_secs_f64(
                    (*self.shared.interval_seconds.lock().unwrap() * 2.0).clamp(1.0, 10.0),
                );
            loop {
                if self.shared.latest.read().unwrap().connections.is_some() {
                    break;
                }
                if std::time::Instant::now() > deadline {
                    break; // reply with what exists; absence is visible
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(self.filtered_snapshot(wants))
    }

    fn set_paused(&self, paused: bool) -> Result<Value, VerbError> {
        self.shared.paused.store(paused, Ordering::Relaxed);
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
        Ok(json!({"paused": paused}))
    }

    fn set_interval(&self, seconds: f64) -> Result<Value, VerbError> {
        *self.shared.interval_seconds.lock().unwrap() = seconds;
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
        Ok(json!({"interval_seconds": seconds}))
    }

    fn gui_verb(&self, verb: &str, arguments: &Value) -> Result<Value, VerbError> {
        let Some(ctx) = self.ctx.get() else {
            return Err(VerbError::unavailable(
                "the GUI is still starting up",
                "retry in a moment",
            ));
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        let command = GuiCommand {
            verb: verb.to_string(),
            value: arguments["value"].as_str().map(str::to_string),
            path: arguments["path"].as_str().map(str::to_string),
            reply: reply_tx,
        };
        self.commands
            .lock()
            .unwrap()
            .send(command)
            .map_err(|_| {
                VerbError::unavailable(
                    "the GUI's command queue is gone (shutting down?)",
                    "relaunch sysmon",
                )
            })?;
        ctx.request_repaint();

        // `shot` waits for a render + encode round-trip.
        let timeout = if verb == "shot" {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(5)
        };
        match reply_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => Err(VerbError::runtime(
                format!("the GUI did not answer `{verb}` in time"),
                "is the window responding? try `sysmon ctl status`",
            )),
        }
    }

    fn request_quit(&self) {
        // The socket is bound before the window exists, so a `quit`
        // can arrive while the GUI is still starting. Remember it: the
        // app checks the flag on its first frame. (It was dropped
        // before, and the window lived on after a `quit` that had
        // replied "quitting": true.)
        self.shared.quit_requested.store(true, Ordering::SeqCst);
        if let Some(ctx) = self.ctx.get() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx.request_repaint();
        }
    }
}
