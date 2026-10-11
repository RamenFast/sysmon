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
use sysmon_core::history::{HistoryRecord, Limits, OpenError, Recorder, RecordingStatus};
use sysmon_core::snapshot::Wants;

use crate::control::{Backend, BindError, ControlServer, VerbError, socket_path};
use crate::envelope::{self, EXIT_OK, EXIT_RUNTIME, EXIT_UNAVAILABLE};
use crate::history_desk::{HistoryDesk, HistoryOp};

struct ServeBackend {
    sampler: Mutex<Sampler>,
    started_ts: f64,
    quit: Arc<AtomicBool>,
    /// `--record`: the recorder thread's last account (None without
    /// the flag: plain serve samples only on demand).
    history: Option<Arc<Mutex<RecordingStatus>>>,
    /// Clear / pack requests for the recorder thread.
    desk: Arc<HistoryDesk>,
    /// Set by a window's takeover (`quit` with `takeover_pid`): the pid
    /// to stand by for.
    takeover: Arc<Mutex<Option<i64>>>,
}

/// A window longer than the slowest subscriber cadence (60 s) is no
/// client's cadence: it's leftover from a client that went away.
const STALE_WINDOW_SECONDS: f64 = 61.0;

impl Backend for ServeBackend {
    fn mode(&self) -> &'static str {
        "serve"
    }

    fn status(&self) -> Value {
        let mut status = json!({
            "socket": socket_path(),
            "since_ts": self.started_ts,
            "sampling": if self.history.is_some() {
                "on demand, plus the history recorder's own cadence (--record)"
            } else {
                "on demand (client cadence drives it)"
            },
        });
        if let Some(history) = &self.history {
            status["history"] = json!(*history.lock().unwrap_or_else(|p| p.into_inner()));
        }
        status
    }

    fn snapshot(&self, wants: Wants) -> Result<Value, VerbError> {
        let mut sampler = self.sampler.lock().map_err(|_| {
            VerbError::runtime(
                "sampler poisoned by an earlier panic",
                "restart the daemon: sysmon ctl quit && sysmon serve",
            )
        })?;
        // On-demand sampling, shared by every client: the sections this
        // client wants may never have been sampled (every rate a false
        // 0), or were last sampled by other clients at other times (one
        // stale, one fresh: an 8 s average under a 0.3 s label). Unless
        // they share one recent window, open a fresh common window, the
        // same short one a direct probe takes.
        let window_ok = sampler
            .common_window(wants)
            .is_some_and(|window| (0.2..=STALE_WINDOW_SECONDS).contains(&window));
        if !window_ok {
            let _prime = sampler.sample(wants);
            let window = if wants.processes { 1000 } else { 250 };
            std::thread::sleep(Duration::from_millis(window));
        }
        let snapshot = sampler.sample(wants);
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

    fn request_quit_from(&self, request: &Value) {
        if let Some(pid) = request["takeover_pid"].as_i64() {
            *self.takeover.lock().unwrap_or_else(|p| p.into_inner()) = Some(pid);
        }
        self.request_quit();
    }

    fn history_op(&self, arguments: &Value) -> Result<Value, VerbError> {
        let op = HistoryOp::from_request(arguments)?;
        if self.history.is_none() {
            return Err(VerbError::unavailable(
                "this serve is not recording history (no --record)",
                "with no recorder holding the lock, `sysmon history --clear --yes` acts on the files itself",
            ));
        }
        self.desk.ask(op, Duration::from_secs(600))
    }
}

static QUIT_FLAG: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signal: libc::c_int) {
    QUIT_FLAG.store(true, Ordering::SeqCst);
}

/// `serve --record [--interval N]`: what the recorder thread needs.
#[derive(Clone, Copy)]
struct RecordOptions {
    interval_seconds: f64,
    limits: Limits,
}

fn parse_arguments(arguments: &[String]) -> Result<Option<RecordOptions>, String> {
    let settings = crate::gui::settings::Settings::load();
    let mut record = false;
    let mut interval = settings.update_interval_seconds;
    let mut cursor = arguments.iter();
    while let Some(argument) = cursor.next() {
        match argument.as_str() {
            "--record" => record = true,
            "--json" => {}
            "--interval" | "-i" => {
                interval = cursor
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite() && (0.5..=60.0).contains(v))
                    .ok_or("--interval needs seconds between 0.5 and 60")?;
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(record.then(|| RecordOptions { interval_seconds: interval, limits: settings.history_limits() }))
}

/// The `--record` thread: its own sampler at its own cadence (the
/// on-demand sampler's windows stay the clients'), one record a sample.
fn spawn_recorder(
    options: RecordOptions,
    recorder: Recorder,
    status: Arc<Mutex<RecordingStatus>>,
    quit: Arc<AtomicBool>,
    desk: Arc<HistoryDesk>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("sysmon-history".to_string())
        .spawn(move || {
            let mut recorder = recorder;
            let dir = recorder.dir().to_path_buf();
            // The recorder wants what the window's history holds;
            // processes only to name the GPU's clients (gpu covers the
            // per-process GPU scan itself).
            let mut wants = Wants::all();
            wants.per_process_net = false;
            wants.processes = false;
            let mut sampler = Sampler::with_options(sysmon_core::collect::SamplerOptions { enable_nethogs: false });
            let _prime = sampler.sample(wants);
            while !quit.load(Ordering::SeqCst) && !QUIT_FLAG.load(Ordering::SeqCst) {
                let started = std::time::Instant::now();
                // Sleep in short steps so a quit lands within 200 ms.
                while started.elapsed().as_secs_f64() < options.interval_seconds {
                    if quit.load(Ordering::SeqCst) || QUIT_FLAG.load(Ordering::SeqCst) {
                        return;
                    }
                    // A clear or pack asked over the socket lands
                    // between samples, never mid-append.
                    if desk.serve(Some(&mut recorder), &dir) {
                        *status.lock().unwrap_or_else(|p| p.into_inner()) = recorder.status();
                    }
                    std::thread::sleep(Duration::from_millis(200).min(
                        Duration::from_secs_f64((options.interval_seconds - started.elapsed().as_secs_f64()).max(0.0)),
                    ));
                }
                let snapshot = sampler.sample(wants);
                let next = match recorder.record(&HistoryRecord::from_snapshot(&snapshot)) {
                    Ok(()) => recorder.status(),
                    Err(error) => {
                        let mut failed = recorder.status();
                        failed.problem = Some(format!("writing history failed: {error}"));
                        failed
                    }
                };
                *status.lock().unwrap_or_else(|p| p.into_inner()) = next;
            }
        })
        .expect("history thread")
}

/// Whether a pid still names a running process (a zombie is not).
fn alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state letter follows the last ')' (comm may hold parens).
        Ok(stat) => stat.rsplit_once(')').and_then(|(_, rest)| rest.trim_start().chars().next()).is_some_and(|state| state != 'Z' && state != 'X'),
        Err(_) => false,
    }
}

/// `--record` only: a window holds the history (it took over, or was
/// open first). Wait for it to exit, then come back. Returns false on
/// SIGTERM/SIGINT. Costs nothing that matters: one /proc read every 2 s.
fn stand_by(owner_pid: i64) -> bool {
    eprintln!(
        "sysmon serve: the window (pid {owner_pid}) is recording the history; standing by until it closes"
    );
    loop {
        if QUIT_FLAG.load(Ordering::SeqCst) {
            return false;
        }
        if !alive(owner_pid) {
            // Its socket and writer lock go with it; a moment for the
            // teardown.
            std::thread::sleep(Duration::from_millis(500));
            return !QUIT_FLAG.load(Ordering::SeqCst);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The pid in a lock holder's words ("pid 4242 gui").
fn holder_pid(holder: &str) -> Option<i64> {
    holder.strip_prefix("pid ")?.split_whitespace().next()?.parse().ok()
}

/// How one serving stretch ended.
enum Ended {
    /// `ctl quit`, SIGTERM or SIGINT: exit.
    Quit(i32),
    /// A window took over (or already held the history): stand by for
    /// this pid, then serve again.
    TakenOver(i64),
}

pub fn run(arguments: &[String]) -> i32 {
    let record = match parse_arguments(arguments) {
        Ok(record) => record,
        Err(message) => {
            return envelope::fail(
                message,
                "usage: sysmon serve [--record [--interval N]]",
                crate::envelope::EXIT_BAD_ARGS,
            );
        }
    };
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    // Plain serve answers until asked to quit. `serve --record` (the
    // headless recorder, e.g. a systemd user service) hands the history
    // to a window while one is open and picks it back up when it closes
    // (3.5; before, a window's takeover ended it for good).
    let mut first = true;
    loop {
        match serve_once(record.as_ref(), first) {
            Ended::Quit(code) => return code,
            Ended::TakenOver(pid) => {
                if !stand_by(pid) {
                    return EXIT_OK;
                }
                first = false;
            }
        }
    }
}

fn serve_once(record: Option<&RecordOptions>, first: bool) -> Ended {
    let quit = Arc::new(AtomicBool::new(false));
    let takeover = Arc::new(Mutex::new(None));
    let history_status = record.map(|_| Arc::new(Mutex::new(RecordingStatus::default())));
    let desk = Arc::new(HistoryDesk::default());
    let backend = Arc::new(ServeBackend {
        sampler: Mutex::new(Sampler::new()),
        started_ts: envelope::now_ts(),
        quit: quit.clone(),
        takeover: takeover.clone(),
        history: history_status.clone(),
        desk: desk.clone(),
    });

    let mut server = match ControlServer::bind(backend) {
        Ok(server) => server,
        Err(BindError::AlreadyRunning(status)) => {
            let owner = &status["result"];
            let mode = owner["mode"].as_str().unwrap_or("?");
            let pid = owner["pid"].as_i64().unwrap_or(0);
            if record.is_some() && mode == "gui" && pid > 0 {
                return Ended::TakenOver(pid);
            }
            return Ended::Quit(envelope::fail(
                format!("an instance already owns the socket (mode {mode}, pid {pid})"),
                "query it instead (`sysmon probe`, `sysmon tap`), or stop it: `sysmon ctl quit`",
                EXIT_UNAVAILABLE,
            ));
        }
        Err(BindError::Io(io_error)) => {
            return Ended::Quit(envelope::fail(
                format!("could not bind {}: {io_error}", socket_path().display()),
                "check $XDG_RUNTIME_DIR exists and is yours",
                EXIT_RUNTIME,
            ));
        }
    };

    // The writer lock is taken after the socket: a second serve never
    // gets this far, and a window that takes the socket over asks this
    // one to step aside, which drops the lock for it.
    let mut recorder_thread = None;
    if let (Some(options), Some(status)) = (record, &history_status) {
        let dir = sysmon_core::history::default_dir();
        match Recorder::open(&dir, options.limits, "serve") {
            Ok(recorder) => {
                *status.lock().unwrap_or_else(|p| p.into_inner()) = recorder.status();
                recorder_thread = Some(spawn_recorder(*options, recorder, status.clone(), quit.clone(), desk.clone()));
            }
            Err(error) => {
                server.shutdown();
                // A window that records without owning the socket (its
                // bind failed): wait for it like any window.
                if let OpenError::Busy(holder) = &error
                    && holder.ends_with(" gui")
                    && let Some(pid) = holder_pid(holder)
                {
                    return Ended::TakenOver(pid);
                }
                let (fix, exit) = match error {
                    OpenError::Busy(_) => ("stop the other recorder first, or let it record", EXIT_UNAVAILABLE),
                    OpenError::Io(_) => ("check the directory is writable (XDG_STATE_HOME)", EXIT_RUNTIME),
                };
                return Ended::Quit(envelope::fail(format!("cannot record history in {}: {error}", dir.display()), fix, exit));
            }
        }
    }

    let recording_dir = recorder_thread.as_ref().map(|_| sysmon_core::history::default_dir());
    if !first {
        eprintln!("sysmon serve: the window closed; recording the history again");
    }
    if envelope::json_wanted(false) {
        let mut result = json!({
            "serving": socket_path(),
            "pid": std::process::id(),
        });
        if let Some(dir) = &recording_dir {
            result["recording"] = json!(dir);
        }
        if !first {
            result["resumed"] = json!(true);
        }
        envelope::emit(&envelope::ok(result));
    } else if let Some(dir) = &recording_dir {
        out!(
            "sysmon serve — answering on {} and recording usage history into {} (ctrl-c to stop)",
            socket_path().display(),
            dir.display()
        );
    } else {
        out!(
            "sysmon serve — answering on {} (ctrl-c to stop; costs nothing while idle)",
            socket_path().display()
        );
    }

    while !quit.load(Ordering::SeqCst) && !QUIT_FLAG.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let taken_over = if QUIT_FLAG.load(Ordering::SeqCst) || record.is_none() {
        None
    } else {
        *takeover.lock().unwrap_or_else(|p| p.into_inner())
    };
    quit.store(true, Ordering::SeqCst);
    if taken_over.is_some() {
        // A window is waiting for the socket (it retries for ~2 s):
        // free it first. The lock follows within a second, and the
        // window retries the lock every 5 s.
        server.shutdown();
        if let Some(thread) = recorder_thread {
            let _ = thread.join();
        }
    } else {
        if let Some(thread) = recorder_thread {
            let _ = thread.join();
        }
        server.shutdown();
    }
    match taken_over {
        Some(pid) => Ended::TakenOver(pid),
        None => Ended::Quit(EXIT_OK),
    }
}
