// SPDX-License-Identifier: GPL-3.0-or-later
//! The writer's inbox. Clearing or packing the history must happen in
//! the process that holds the writer lock (the window, or `serve
//! --record`), on the thread that owns its Recorder, so a clear can
//! never race a line being appended. `sysmon history --clear` and
//! `--pack` try the lock themselves first; when a recorder holds it,
//! they send the `history` verb over the control socket, the backend
//! drops the request here, and the recorder's thread answers it
//! between two samples. The window's own Clear… button posts here too.

use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use serde_json::{Value, json};

use sysmon_core::history::{OpenError, PackScope, Recorder, manage};

use crate::control::VerbError;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HistoryOp {
    Clear,
    Pack,
}

impl HistoryOp {
    pub fn parse(name: &str) -> Option<HistoryOp> {
        match name {
            "clear" => Some(HistoryOp::Clear),
            "pack" => Some(HistoryOp::Pack),
            _ => None,
        }
    }

    /// The socket verb's arguments, checked: `op`, and `yes: true` for
    /// a clear (the socket is as deliberate as the CLI's `--yes`).
    pub fn from_request(arguments: &Value) -> Result<HistoryOp, VerbError> {
        let op = arguments["op"].as_str().and_then(HistoryOp::parse).ok_or_else(|| {
            VerbError::bad_args("history needs `op`: clear or pack", "send {\"verb\":\"history\",\"op\":\"pack\"}")
        })?;
        if op == HistoryOp::Clear && arguments["yes"] != json!(true) {
            return Err(VerbError::bad_args(
                "clearing deletes the whole usage history and needs `yes`: true",
                "send {\"verb\":\"history\",\"op\":\"clear\",\"yes\":true}, or run `sysmon history --clear --yes`",
            ));
        }
        Ok(op)
    }
}

pub type HistoryReply = Result<Value, VerbError>;
type Reply = Sender<HistoryReply>;

pub struct HistoryDesk {
    tx: Mutex<Sender<(HistoryOp, Reply)>>,
    rx: Mutex<Receiver<(HistoryOp, Reply)>>,
}

impl Default for HistoryDesk {
    fn default() -> Self {
        let (tx, rx) = channel();
        HistoryDesk { tx: Mutex::new(tx), rx: Mutex::new(rx) }
    }
}

impl HistoryDesk {
    /// Queue an op; the answer arrives on the returned receiver.
    pub fn submit(&self, op: HistoryOp) -> Receiver<HistoryReply> {
        let (reply_tx, reply_rx) = channel();
        if let Err(failed) = self.tx.lock().unwrap_or_else(|p| p.into_inner()).send((op, reply_tx)) {
            let (_, reply_tx) = failed.0;
            let _ = reply_tx.send(Err(VerbError::runtime("this instance's history recorder is gone", "restart it, then try again")));
        }
        reply_rx
    }

    /// From a socket connection's thread: queue the op and wait.
    pub fn ask(&self, op: HistoryOp, wait: Duration) -> HistoryReply {
        self.submit(op).recv_timeout(wait).unwrap_or_else(|_| {
            Err(VerbError::runtime(
                "the history recorder did not answer in time",
                "it may be busy packing; try again in a minute",
            ))
        })
    }

    /// From the recorder's thread, between samples: answer every
    /// waiting request. With no recorder (the window's recording is
    /// off) a clear acts on the files under their own lock. Returns
    /// whether it answered anything.
    pub fn serve(&self, recorder: Option<&mut Recorder>, dir: &Path) -> bool {
        let pending: Vec<(HistoryOp, Reply)> = {
            let rx = self.rx.lock().unwrap_or_else(|p| p.into_inner());
            std::iter::from_fn(|| rx.try_recv().ok()).collect()
        };
        if pending.is_empty() {
            return false;
        }
        let mut recorder = recorder;
        for (op, reply) in pending {
            let answer = match (recorder.as_deref_mut(), op) {
                (Some(recorder), HistoryOp::Clear) => recorder
                    .clear()
                    .map(|report| json!({"op": "clear", "cleared": report, "by": "recorder"}))
                    .map_err(|error| VerbError::runtime(format!("clearing the history failed: {error}"), "check the directory's permissions")),
                (Some(recorder), HistoryOp::Pack) => {
                    let report = recorder.maintain_blocking(PackScope::AllSealed);
                    Ok(json!({"op": "pack", "packed": report, "store": recorder.status(), "by": "recorder"}))
                }
                (None, HistoryOp::Clear) => match manage::clear_unlocked(dir) {
                    Ok(report) => Ok(json!({"op": "clear", "cleared": report, "by": "files"})),
                    Err(OpenError::Busy(holder)) => Err(VerbError::unavailable(
                        format!("{holder} is recording the history"),
                        "clear it there (`sysmon history --clear --yes` reaches it), or stop it first",
                    )),
                    Err(OpenError::Io(error)) => {
                        Err(VerbError::runtime(format!("clearing the history failed: {error}"), "check the directory's permissions"))
                    }
                },
                (None, HistoryOp::Pack) => Err(VerbError::unavailable(
                    "this instance is not recording history",
                    "with no recorder holding the lock, `sysmon history --pack` acts on the files itself",
                )),
            };
            let _ = reply.send(answer);
        }
        true
    }
}
