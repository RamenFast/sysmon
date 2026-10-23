// SPDX-License-Identifier: GPL-3.0-or-later
//! Threads & CPUs: one process, thread by thread, and which logical
//! CPUs its time landed on (Ben, 3.8: "I see our custom st is using
//! 125.2% cpu, but I want to be able to view what cores that is on
//! specifically").
//!
//! Two views of one reading, tied by hover: the CPU strip (one bar per
//! logical CPU: this process's share, over the faint whole-core busy
//! from the CPU card, in the CPU card's own per-thread colours so CPU 7
//! is the same colour everywhere) and the thread list (each busy
//! thread wears its CPU's colour). Point at a CPU and its threads stay
//! lit; point at a thread and its CPU does.
//!
//! Shared by the Inspector and the details window. Each holds its own
//! `ThreadsState`, so each owns its sampler window (collector law).

use std::time::{Duration, Instant};

use egui::{Align2, FontId, Id, Rect, RichText, Sense, Ui, ViewportId, pos2, vec2};

use sysmon_core::collect::threads::{ProcessThreads, ThreadSampler, parse_cpu_list};
use sysmon_core::snapshot::SystemSnapshot;
use sysmon_core::units::format_duration_seconds;

use super::theme::{Palette, thread_colors};
use super::widgets::paint_row_hover;

/// Below this a thread is asleep for this view's purposes (one 10 ms
/// tick over a 2 s window is 0.5%).
const BUSY_PERCENT: f32 = 0.5;
/// A window is at least this long: thread CPU comes in 10 ms ticks.
const MIN_WINDOW: Duration = Duration::from_millis(1000);
const STRIP_HEIGHT: f32 = 34.0;
const ROW_HEIGHT: f32 = 17.0;
const ALL_ROWS_CAP: usize = 256;

/// One view's reading of one process's threads.
#[derive(Default)]
pub struct ThreadsState {
    sampler: ThreadSampler,
    pid: Option<i32>,
    latest: Option<ProcessThreads>,
    read_at: Option<Instant>,
    read_for_ts: f64,
    reads: u32,
    /// Every thread, not only the busy ones.
    pub show_all: bool,
}

impl ThreadsState {
    /// Re-read when the pid changes, once more a second after the first
    /// read (a rate needs two), and then once per snapshot, at most
    /// once a second.
    pub fn refresh(&mut self, ctx: &egui::Context, pid: i32, snapshot_ts: f64) {
        if self.pid != Some(pid) {
            *self = ThreadsState { pid: Some(pid), show_all: self.show_all, ..Default::default() };
        }
        let old_enough = self.read_at.is_none_or(|at| at.elapsed() >= MIN_WINDOW);
        let due = self.read_at.is_none() || (old_enough && (self.reads < 2 || snapshot_ts != self.read_for_ts));
        if due {
            self.latest = self.sampler.sample(pid);
            self.read_at = Some(Instant::now());
            self.read_for_ts = snapshot_ts;
            self.reads += 1;
        }
        if self.reads < 2 {
            // The window draws only when the sampler ticks (5 s by
            // default); come back for the second read sooner. Immediate
            // viewports draw inside the root's frame.
            ctx.request_repaint_after_for(MIN_WINDOW, ViewportId::ROOT);
        }
    }
}

/// The hovered CPU, shared by the strip and the rows of one process.
fn focus_id(pid: i32) -> Id {
    Id::new(("threads_view_focus_cpu", pid))
}

pub fn threads_view(ui: &mut Ui, palette: &Palette, graph_palette_id: &str, snapshot: &SystemSnapshot, state: &mut ThreadsState) {
    let Some(threads) = &state.latest else {
        ui.label(RichText::new("its threads can't be read (it may have just exited)").color(palette.muted).italics().size(10.5));
        return;
    };
    let pid = threads.pid;
    let focus: Option<u32> = ui.ctx().data(|data| data.get_temp(focus_id(pid)));
    let mut new_focus: Option<u32> = None;
    let colors = thread_colors(graph_palette_id, threads.cpu_count);
    let color_of = |cpu: Option<u32>| cpu.and_then(|cpu| colors.get(cpu as usize).copied()).unwrap_or(palette.muted);

    // ── the summary: what 125% means here
    let busy: Vec<_> = threads.threads.iter().filter(|thread| thread.cpu_percent >= BUSY_PERCENT).collect();
    let measuring = threads.interval_seconds <= 0.0;
    let summary = if measuring {
        format!("{} threads · measuring…", threads.threads.len())
    } else {
        format!(
            "{} of {} threads busy · {:.2} of {} CPUs",
            busy.len(),
            threads.threads.len(),
            threads.cpu_percent / 100.0,
            threads.cpu_count
        )
    };
    ui.label(RichText::new(summary).monospace().size(10.5).color(palette.ink_2)).on_hover_text(
        "100% is one whole CPU. Each thread's time is placed on the CPU it last ran on \
         (the reading ps and htop give), over the last ~second or sample.",
    );
    let allowed: Option<Vec<u32>> = threads.allowed_cpus.as_deref().map(parse_cpu_list);
    if let Some(list) = &threads.allowed_cpus {
        ui.label(RichText::new(format!("pinned to CPUs {list}")).size(10.5).color(palette.accent))
            .on_hover_text("its CPU affinity (Cpus_allowed_list): it can't run anywhere else");
    }

    // ── the CPU strip
    let core_busy = snapshot.cpu.as_ref().map(|cpu| cpu.per_core_percent.as_slice()).unwrap_or(&[]);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, STRIP_HEIGHT), Sense::hover());
    let count = threads.cpu_count.max(1);
    let gap = if width / count as f32 > 8.0 { 2.0 } else { 1.0 };
    let bar_width = ((width - gap * (count as f32 - 1.0)) / count as f32).max(1.0);
    let hovered_cpu = response.hover_pos().and_then(|pointer| {
        let index = ((pointer.x - rect.left()) / (bar_width + gap)) as usize;
        (index < count).then_some(index as u32)
    });
    if hovered_cpu.is_some() {
        new_focus = hovered_cpu;
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        for index in 0..count {
            let x = rect.left() + index as f32 * (bar_width + gap);
            let track = Rect::from_min_size(pos2(x, rect.top()), vec2(bar_width, STRIP_HEIGHT));
            let barred = allowed.as_ref().is_some_and(|allowed| !allowed.contains(&(index as u32)));
            painter.rect_filled(track, 0.0, palette.ink.gamma_multiply(if barred { 0.02 } else { 0.07 }));
            let column = |percent: f32| {
                Rect::from_min_max(pos2(x, track.bottom() - STRIP_HEIGHT * percent.clamp(0.0, 100.0) / 100.0), pos2(x + bar_width, track.bottom()))
            };
            // The whole core's busy, faint: how much of it is this one.
            if let Some(core) = core_busy.get(index).filter(|core| **core >= 1.0) {
                painter.rect_filled(column(*core), 0.0, palette.ink.gamma_multiply(0.13));
            }
            let share = threads.per_cpu_percent.get(index).copied().unwrap_or(0.0);
            if share >= BUSY_PERCENT {
                let dimmed = focus.is_some_and(|focus| focus as usize != index);
                let fill = column(share.max(4.0));
                painter.rect_filled(fill, 0.0, color_of(Some(index as u32)).gamma_multiply(if dimmed { 0.3 } else { 0.95 }));
            }
            if barred {
                painter.hline(track.x_range(), track.bottom() - 0.5, egui::Stroke::new(1.0, palette.muted.gamma_multiply(0.5)));
            }
        }
        if let Some(focus) = focus.filter(|focus| (*focus as usize) < count) {
            let x = rect.left() + focus as f32 * (bar_width + gap);
            painter.rect_stroke(
                Rect::from_min_size(pos2(x, rect.top()), vec2(bar_width, STRIP_HEIGHT)).expand(0.5),
                0.0,
                egui::Stroke::new(1.0, palette.ink.gamma_multiply(0.6)),
                egui::StrokeKind::Inside,
            );
        }
    }
    if let Some(cpu) = hovered_cpu {
        let share = threads.per_cpu_percent.get(cpu as usize).copied().unwrap_or(0.0);
        let on_it: Vec<_> = threads.threads.iter().filter(|t| t.cpu == Some(cpu) && t.cpu_percent >= BUSY_PERCENT).collect();
        response.on_hover_ui_at_pointer(|ui| {
            ui.monospace(format!("CPU {cpu}"));
            ui.monospace(format!("this process  {share:>5.1}%"));
            if let Some(core) = core_busy.get(cpu as usize) {
                ui.monospace(format!("whole core    {core:>5.1}%"));
            }
            if on_it.is_empty() {
                ui.label(RichText::new("none of its threads ran here this window").italics().size(10.5));
            }
            for thread in on_it.iter().take(8) {
                ui.monospace(format!("  {:<15} {:>5.1}%", thread.name, thread.cpu_percent));
            }
            if on_it.len() > 8 {
                ui.monospace(format!("  … {} more", on_it.len() - 8));
            }
        });
    }
    // CPU numbers under the strip: the ends and the middle.
    let (axis, _) = ui.allocate_exact_size(vec2(width, 11.0), Sense::hover());
    if ui.is_rect_visible(axis) && count > 1 {
        let painter = ui.painter();
        let font = FontId::monospace(9.0);
        let center_of = |index: usize| rect.left() + index as f32 * (bar_width + gap) + bar_width / 2.0;
        painter.text(pos2(rect.left(), axis.center().y), Align2::LEFT_CENTER, "0", font.clone(), palette.muted);
        painter.text(pos2(center_of(count / 2), axis.center().y), Align2::CENTER_CENTER, (count / 2).to_string(), font.clone(), palette.muted);
        painter.text(pos2(rect.right(), axis.center().y), Align2::RIGHT_CENTER, (count - 1).to_string(), font, palette.muted);
    }
    ui.add_space(2.0);

    // ── the threads
    let rows: Vec<_> = if state.show_all {
        threads.threads.iter().take(ALL_ROWS_CAP).collect()
    } else {
        busy.clone()
    };
    if rows.is_empty() && !measuring {
        ui.label(RichText::new(format!("all {} threads are asleep this window", threads.threads.len())).color(palette.muted).italics().size(10.5));
    }
    for thread in &rows {
        let (_, row) = ui.allocate_space(vec2(ui.available_width(), ROW_HEIGHT));
        let response = ui.interact(row, ui.id().with(("thread_row", thread.tid)), Sense::hover());
        if response.hovered() {
            new_focus = thread.cpu;
        }
        if ui.is_rect_visible(row) {
            let painter = ui.painter();
            if response.hovered() {
                paint_row_hover(painter, row, palette, true);
            }
            let quiet = focus.is_some() && thread.cpu != focus;
            let ink = if quiet { palette.muted } else { palette.ink };
            let chip = Rect::from_center_size(pos2(row.left() + 7.0, row.center().y), vec2(7.0, 7.0));
            let chip_color = color_of(thread.cpu);
            painter.rect_filled(chip, 0.0, if thread.cpu_percent >= BUSY_PERCENT { chip_color } else { chip_color.gamma_multiply(0.35) });
            let name = painter.text(pos2(row.left() + 16.0, row.center().y), Align2::LEFT_CENTER, &thread.name, FontId::proportional(11.0), ink);
            painter.text(
                pos2(name.right() + 6.0, row.center().y),
                Align2::LEFT_CENTER,
                thread.tid.to_string(),
                FontId::monospace(9.5),
                palette.muted,
            );
            let cpu = thread.cpu.map(|cpu| format!("CPU {cpu:>2}")).unwrap_or_else(|| "CPU  —".into());
            let percent = painter.text(
                pos2(row.right() - 2.0, row.center().y),
                Align2::RIGHT_CENTER,
                format!("{:>5.1}%", thread.cpu_percent),
                FontId::monospace(10.5),
                ink,
            );
            painter.text(pos2(percent.left() - 10.0, row.center().y), Align2::RIGHT_CENTER, cpu, FontId::monospace(10.0), palette.ink_2);
        }
        response.on_hover_text(format!(
            "{} · TID {} · {}\non CPU {} · ran {} in all",
            thread.name,
            thread.tid,
            thread.state_word,
            thread.cpu.map(|cpu| cpu.to_string()).unwrap_or("—".into()),
            format_duration_seconds(thread.cpu_time_seconds)
        ));
    }
    let quiet_count = threads.threads.len() - busy.len();
    let toggle = if state.show_all {
        Some("only the busy threads".to_string())
    } else if quiet_count > 0 {
        Some(format!("+ {quiet_count} sleeping thread{}", if quiet_count == 1 { "" } else { "s" }))
    } else {
        None
    };
    if let Some(toggle) = toggle {
        let link = ui.add(egui::Label::new(RichText::new(toggle).size(10.5).color(palette.muted)).sense(Sense::click()).selectable(false));
        if link.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            state.show_all = !state.show_all;
        }
    }
    if state.show_all && threads.threads.len() > ALL_ROWS_CAP {
        ui.label(RichText::new(format!("… {} more (sysmon probe threads --pid {pid} --json has all)", threads.threads.len() - ALL_ROWS_CAP)).color(palette.muted).size(10.0));
    }

    if new_focus != focus {
        ui.ctx().data_mut(|data| match new_focus {
            Some(cpu) => data.insert_temp(focus_id(pid), cpu),
            None => data.remove::<u32>(focus_id(pid)),
        });
        // The strip above was already painted this frame.
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_state_reads_once_then_again_after_a_second() {
        let ctx = egui::Context::default();
        let mut state = ThreadsState::default();
        let pid = std::process::id() as i32;
        state.refresh(&ctx, pid, 1.0);
        assert_eq!(state.reads, 1);
        state.refresh(&ctx, pid, 1.0);
        assert_eq!(state.reads, 1, "not twice inside the window");
        std::thread::sleep(MIN_WINDOW + Duration::from_millis(20));
        state.refresh(&ctx, pid, 1.0);
        assert_eq!(state.reads, 2, "the second read comes without a new snapshot");
        assert!(state.latest.as_ref().is_some_and(|t| t.interval_seconds > 0.0));
        // Another pid starts over but keeps the show-all choice.
        state.show_all = true;
        state.refresh(&ctx, 1, 2.0);
        assert_eq!(state.reads, 1);
        assert!(state.show_all);
    }
}
