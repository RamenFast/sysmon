// SPDX-License-Identifier: GPL-3.0-or-later
//! `sysmon serve` — the headless sampling daemon. Binds the control
//! socket and answers snapshot/subscribe requests **on demand**:
//! no clients means literally zero sampling work, which is what a
//! desktop-bar backend should cost while the bar isn't looking.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use sysmon_core::collect::Sampler;
use sysmon_core::snapshot::Wants;

use crate::control::{Backend, BindError, ControlServer, VerbError, socket_path};
use crate::envelope::{self, EXIT_OK, EXIT_RUNTIME, EXIT_UNAVAILABLE};

struct ServeBackend {
    sampler: Mutex<Sampler>,
    started_ts: f64,
    quit: Arc<AtomicBool>,
}

impl Backend for ServeBackend {
    fn mode(&self) -> &'static str {
        "serve"
    }

    fn status(&self) -> Value {
        json!({
            "socket": socket_path(),
            "since_ts": self.started_ts,
            "sampling": "on demand (client cadence drives it)",
        })
    }

    fn snapshot(&self, wants: Wants) -> Result<Value, VerbError> {
        let mut sampler = self.sampler.lock().map_err(|_| {
            VerbError::runtime(
                "sampler poisoned by an earlier panic",
                "restart the daemon: sysmon ctl quit && sysmon serve",
            )
        })?;
        // On-demand sampling means the first ask (or the first after
        // a long quiet, for a section nobody asked for yet) has no
        // previous sample to diff, so every rate would be a false 0.
        // Take the same short window a direct probe takes instead.
        let mut snapshot = sampler.sample(wants);
        if snapshot.interval_seconds < 0.2 {
            let window = if wants.processes { 1000 } else { 250 };
            std::thread::sleep(Duration::from_millis(window));
            snapshot = sampler.sample(wants);
        }
        snapshot.to_json_value().map_err(|serialize_error| {
            VerbError::runtime(
                format!("snapshot serialization failed: {serialize_error}"),
                "this is a sysmon bug — please report it",
            )
        })
    }

    fn set_paused(&self, _paused: bool) -> Result<Value, VerbError> {
        Err(VerbError::unavailable(
            "serve samples on demand — there is nothing to pause",
            "drive cadence from the client side (`sysmon tap … --interval N`), or pause the GUI",
        ))
    }

    fn set_interval(&self, _seconds: f64) -> Result<Value, VerbError> {
        Err(VerbError::unavailable(
            "serve has no fixed interval — each subscriber picks its own",
            "pass --interval to `sysmon tap`, or send {\"verb\":\"subscribe\",\"interval\":N}",
        ))
    }

    fn request_quit(&self) {
        self.quit.store(true, Ordering::SeqCst);
    }
}

static QUIT_FLAG: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signal: libc::c_int) {
    QUIT_FLAG.store(true, Ordering::SeqCst);
}

pub fn run(_arguments: &[String]) -> i32 {
    let quit = Arc::new(AtomicBool::new(false));
    let backend = Arc::new(ServeBackend {
        sampler: Mutex::new(Sampler::new()),
        started_ts: envelope::now_ts(),
        quit: quit.clone(),
    });

    let mut server = match ControlServer::bind(backend) {
        Ok(server) => server,
        Err(BindError::AlreadyRunning(status)) => {
            let owner = &status["result"];
            return envelope::fail(
                format!(
                    "an instance already owns the socket (mode {}, pid {})",
                    owner["mode"].as_str().unwrap_or("?"),
                    owner["pid"].as_u64().unwrap_or(0),
                ),
                "query it instead (`sysmon probe`, `sysmon tap`), or stop it: `sysmon ctl quit`",
                EXIT_UNAVAILABLE,
            );
        }
        Err(BindError::Io(io_error)) => {
            return envelope::fail(
                format!("could not bind {}: {io_error}", socket_path().display()),
                "check $XDG_RUNTIME_DIR exists and is yours",
                EXIT_RUNTIME,
            );
        }
    };

    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }

    if envelope::json_wanted(false) {
        envelope::emit(&envelope::ok(json!({
            "serving": socket_path(),
            "pid": std::process::id(),
        })));
    } else {
        out!(
            "sysmon serve — answering on {} (ctrl-c to stop; costs nothing while idle)",
            socket_path().display()
        );
    }

    while !quit.load(Ordering::SeqCst) && !QUIT_FLAG.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
    }
    server.shutdown();
    EXIT_OK
}
