// SPDX-License-Identifier: GPL-3.0-or-later
//! Process control: end / kill / renice, straight syscalls first,
//! pkexec fallback when permissions say no (root-owned processes,
//! raising priority) — v1's exact ladder. Destructive actions go
//! through a confirm modal; results that fail land as a gentle
//! toast, never a silent shrug.

use egui::RichText;

use super::theme::Palette;

/// A destructive action awaiting the user's confirmation.
#[derive(Clone, Debug)]
pub struct PendingConfirm {
    pub pid: i32,
    pub name: String,
    pub kind: ConfirmKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConfirmKind {
    Terminate,
    Kill,
}

/// Transient error/info line shown at the bottom of the window.
#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub shown_at: std::time::Instant,
}

pub fn signal_process(pid: i32, signal: i32) -> Result<(), std::io::Error> {
    let result = unsafe { libc::kill(pid, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub fn set_priority(pid: i32, nice: i32) -> Result<(), std::io::Error> {
    // setpriority returns -1 both for errors and legally; clear errno
    // first, the libc way.
    unsafe { *libc::__errno_location() = 0 };
    let result = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, nice) };
    let errno = unsafe { *libc::__errno_location() };
    if result == -1 && errno != 0 {
        Err(std::io::Error::from_raw_os_error(errno))
    } else {
        Ok(())
    }
}

/// Run a pkexec fallback off the UI thread; report failure through
/// the toast channel. Dismissing the auth dialog (126/127) is not an
/// error worth nagging about.
pub fn pkexec_fallback(
    command: Vec<String>,
    failure_message: String,
    toast_tx: std::sync::mpsc::Sender<String>,
) {
    std::thread::spawn(move || {
        let status = std::process::Command::new("pkexec")
            .args(&command)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(status) => {
                let code = status.code().unwrap_or(-1);
                if !status.success() && code != 126 && code != 127 {
                    let _ = toast_tx.send(failure_message);
                }
            }
            Err(_) => {
                let _ = toast_tx.send(format!("{failure_message} (pkexec missing?)"));
            }
        }
    });
}

/// Terminate/kill with the permission ladder.
pub fn end_process(
    pid: i32,
    name: &str,
    force: bool,
    toast_tx: &std::sync::mpsc::Sender<String>,
) {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    match signal_process(pid, signal) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::EPERM) => {
            pkexec_fallback(
                vec![
                    "kill".to_string(),
                    format!("-{}", if force { "KILL" } else { "TERM" }),
                    pid.to_string(),
                ],
                format!("Could not end {name} (PID {pid})."),
                toast_tx.clone(),
            );
        }
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {} // already gone
        Err(error) => {
            let _ = toast_tx.send(format!("Could not end {name} (PID {pid}): {error}"));
        }
    }
}

pub fn renice_process(
    pid: i32,
    name: &str,
    nice: i32,
    toast_tx: &std::sync::mpsc::Sender<String>,
) {
    match set_priority(pid, nice) {
        Ok(()) => {}
        Err(error)
            if error.raw_os_error() == Some(libc::EPERM)
                || error.raw_os_error() == Some(libc::EACCES) =>
        {
            pkexec_fallback(
                vec![
                    "renice".to_string(),
                    "-n".to_string(),
                    nice.to_string(),
                    "-p".to_string(),
                    pid.to_string(),
                ],
                format!("Could not change the priority of {name} (PID {pid})."),
                toast_tx.clone(),
            );
        }
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {}
        Err(error) => {
            let _ = toast_tx.send(format!(
                "Could not change the priority of {name} (PID {pid}): {error}"
            ));
        }
    }
}

/// The confirm modal. Returns Some(confirmed) when the user decided.
pub fn confirm_modal(
    ctx: &egui::Context,
    palette: &Palette,
    pending: &PendingConfirm,
) -> Option<bool> {
    let mut decision = None;
    let title = match pending.kind {
        ConfirmKind::Terminate => format!("End “{}”?", pending.name),
        ConfirmKind::Kill => format!("Kill “{}”?", pending.name),
    };
    let detail = match pending.kind {
        ConfirmKind::Terminate => format!(
            "PID {} will be asked to exit (SIGTERM). Unsaved work may be lost.",
            pending.pid
        ),
        ConfirmKind::Kill => format!(
            "PID {} will be terminated immediately (SIGKILL). Unsaved work will be lost.",
            pending.pid
        ),
    };
    let modal = egui::Modal::new(egui::Id::new("confirm_process_action")).show(ctx, |ui| {
        ui.set_max_width(300.0);
        ui.label(RichText::new(&title).strong().size(14.0));
        ui.add_space(4.0);
        ui.label(RichText::new(&detail).color(palette.ink_2).size(12.0));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                decision = Some(false);
            }
            let confirm_label = match pending.kind {
                ConfirmKind::Terminate => "End process",
                ConfirmKind::Kill => "Kill",
            };
            let confirm_button = egui::Button::new(
                RichText::new(confirm_label).color(palette.on_accent),
            )
            .fill(palette.accent);
            if ui.add(confirm_button).clicked() {
                decision = Some(true);
            }
        });
    });
    if modal.should_close() && decision.is_none() {
        decision = Some(false);
    }
    decision
}
