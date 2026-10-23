// SPDX-License-Identifier: GPL-3.0-or-later
//! The application shell: sampler thread → shared state → main
//! window + pop-out/detail viewports. State the viewports read is
//! Arc'd (deferred viewports render on their own repaints, so a
//! minimized main window never freezes a pop-out — v1's law), and
//! the same shared state serves the control socket in wave 7.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use egui::{Align, Layout, RichText, ViewportBuilder, ViewportCommand, ViewportId};

use sysmon_core::collect::Sampler;
use sysmon_core::snapshot::{SystemSnapshot, Wants};
use sysmon_core::units::TemperatureScale;

use super::actions::{self, ConfirmKind, PendingConfirm};
use super::backend::GuiCommand;
use crate::control::VerbError;
use super::cards::{self, AppAction, CardContext};
use super::details;
use super::graphs::History;
use super::icons::IconCache;
use super::overview::{self, CpuGraphMode};
use super::processes::{ProcessTableState, SortColumn, matches_filter, processes_page};
use super::settings::{CAP_CHOICES_MB, HOT_CHOICES_HOURS, KEEP_CHOICES_DAYS, SECTION_KEYS, Settings};
use super::specs;
use super::theme::{self, Palette};
use super::widgets::{ButtonGlyph, glyph_button, menu_check_row, menu_chip, menu_option_row};

/// The two pages. 3.6 folded the Performance page into the Overview
/// (Ben: the two were redundant); its ids are gone from the wire.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Page {
    Overview,
    Processes,
}

impl Page {
    pub fn id(self) -> &'static str {
        match self {
            Page::Overview => "overview",
            Page::Processes => "processes",
        }
    }

    pub fn from_id(id: &str) -> Option<Page> {
        match id {
            "overview" => Some(Page::Overview),
            "processes" => Some(Page::Processes),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct Histories {
    pub gpu: History,
    pub memory: History,
    pub cpu: History,
    pub net_down: History,
    pub net_up: History,
    /// One per thread, /proc/stat order.
    pub per_core: Vec<History>,
    pub cpu_kernel: History,
    /// (used + cache) as a % of total: the upper trace of the stacked
    /// memory graph; the band down to `memory` is the cache.
    pub memory_stacked: History,
    pub vram: History,
    pub disk_read: History,
    pub disk_write: History,
    pub cpu_temperature: History,
    pub gpu_temperature: History,
    /// The NVIDIA card (3.7): busy, VRAM %, die temperature.
    pub nvidia: History,
    pub nvidia_vram: History,
    pub nvidia_temperature: History,
    pub busy_clock: History,
    /// Highest Committed_AS seen since launch (SysMon's own mark).
    pub commit_peak_bytes: u64,
}

/// Whether this machine's latest sample has an NVIDIA card to show.
fn nvidia_present(snapshot: &SystemSnapshot) -> bool {
    snapshot.nvidia.as_ref().is_some_and(|gpu| gpu.available)
}

/// The Overview's slots: one card each, except that the NVIDIA card
/// rides in the AMD card's slot, stacked under it. So a machine without
/// one lays out exactly as before, and every other card keeps its
/// column when one appears.
pub(crate) fn overview_slots(sections: &[&'static str]) -> Vec<Vec<&'static str>> {
    let mut slots: Vec<Vec<&'static str>> = Vec::new();
    for &section in sections {
        match slots.last_mut() {
            Some(slot) if section == "nvidia" && slot[0] == "gpu" => slot.push(section),
            _ => slots.push(vec![section]),
        }
    }
    slots
}

impl Histories {
    /// Push one sample of every graphed value. The sampler calls this
    /// under the histories write lock; `ui_overview` calls it
    /// directly so the test walks the real push path (reviewer R16).
    pub fn record(&mut self, snapshot: &SystemSnapshot) {
        if let Some(gpu) = &snapshot.gpu
            && gpu.available
        {
            self.gpu.push(gpu.busy_percent as f64);
            if gpu.vram_total_bytes > 0 {
                self.vram.push(gpu.vram_used_bytes as f64 / gpu.vram_total_bytes as f64 * 100.0);
            }
            if let Some(t) = gpu.temperature_edge_celsius {
                self.gpu_temperature.push(t as f64);
            }
        }
        if let Some(gpu) = &snapshot.nvidia
            && gpu.available
        {
            self.nvidia.push(gpu.busy_percent as f64);
            if gpu.vram_total_bytes > 0 {
                self.nvidia_vram.push(gpu.vram_used_bytes as f64 / gpu.vram_total_bytes as f64 * 100.0);
            }
            if let Some(t) = gpu.temperature_core_celsius {
                self.nvidia_temperature.push(t as f64);
            }
        }
        if let Some(memory) = &snapshot.memory {
            self.memory.push(memory.used_percent as f64);
            if memory.total_bytes > 0 {
                let cache_percent = memory.cached_bytes as f64 / memory.total_bytes as f64 * 100.0;
                self.memory_stacked.push((memory.used_percent as f64 + cache_percent).min(100.0));
            }
            self.commit_peak_bytes = self.commit_peak_bytes.max(memory.committed_bytes);
        }
        if let Some(cpu) = &snapshot.cpu {
            self.cpu.push(cpu.overall_percent as f64);
            self.cpu_kernel.push(cpu.kernel_percent as f64);
            if self.per_core.len() != cpu.per_core_percent.len() {
                self.per_core = vec![History::default(); cpu.per_core_percent.len()];
            }
            for (history, percent) in self.per_core.iter_mut().zip(&cpu.per_core_percent) {
                history.push(*percent as f64);
            }
            if let Some(t) = cpu.temperature_celsius {
                self.cpu_temperature.push(t as f64);
            }
            if let Some(clock) = cpu.frequency_busy_mhz.or(cpu.frequency_mhz) {
                self.busy_clock.push(clock);
            }
        }
        if let Some(network) = &snapshot.network {
            self.net_down.push(network.download_bps);
            self.net_up.push(network.upload_bps);
        }
        if let Some(disks) = &snapshot.disks {
            self.disk_read.push(disks.iter().map(|d| d.read_bps).sum());
            self.disk_write.push(disks.iter().map(|d| d.write_bps).sum());
        }
    }
}

/// Everything the render paths (main window, pop-outs, socket
/// backend) share with the sampler.
pub struct SharedUi {
    pub latest: RwLock<Arc<SystemSnapshot>>,
    pub histories: RwLock<Histories>,
    pub paused: AtomicBool,
    pub interval_seconds: Mutex<f64>,
    pub connections_wanted: AtomicBool,
    /// Who-is-on-each-CPU (`cpu.per_core_top`) is paid for only while a
    /// CPU card is on screen to name them.
    pub occupants_wanted: AtomicBool,
    /// Pop-out sections + detail windows request repaints by id.
    pub open_viewports: Mutex<Vec<ViewportId>>,
    /// The main window's outer rect (screen coords) — the drag-dock
    /// target the pop-outs test against.
    pub main_window_rect: Mutex<Option<egui::Rect>>,
    /// The adapter the window renders on, set once at startup.
    /// `degraded` = a CPU rasterizer (llvmpipe): the app still runs,
    /// but slower and hungrier than it should be — `status` and the
    /// startup toast say so out loud (never silently slow).
    pub renderer: OnceLock<RendererInfo>,
    /// Set by the control socket's `quit`; honored on the next frame
    /// (including a quit that arrived before the first one).
    pub quit_requested: AtomicBool,
    /// Usage history on disk: what the sampler should do (the window
    /// sets it from settings) and what it last reported.
    pub history: Mutex<HistoryControl>,
    /// Clear / pack requests (the menu's Clear…, `sysmon history
    /// --clear`) for the sampler thread, which owns the recorder.
    pub history_desk: crate::history_desk::HistoryDesk,
}

/// The main thread's wishes for the recorder, and the recorder's own
/// account back.
pub struct HistoryControl {
    pub enabled: bool,
    pub dir: std::path::PathBuf,
    pub limits: sysmon_core::history::Limits,
    pub status: sysmon_core::history::RecordingStatus,
}

pub struct RendererInfo {
    pub description: String,
    pub degraded: bool,
}

impl SharedUi {
    pub fn new(interval: f64) -> Self {
        SharedUi {
            latest: RwLock::new(Arc::new(SystemSnapshot::default())),
            histories: RwLock::new(Histories::default()),
            paused: AtomicBool::new(false),
            interval_seconds: Mutex::new(interval),
            connections_wanted: AtomicBool::new(false),
            occupants_wanted: AtomicBool::new(false),
            open_viewports: Mutex::new(Vec::new()),
            main_window_rect: Mutex::new(None),
            renderer: OnceLock::new(),
            quit_requested: AtomicBool::new(false),
            history: Mutex::new(HistoryControl {
                enabled: false,
                dir: sysmon_core::history::default_dir(),
                limits: Settings::default().history_limits(),
                status: sysmon_core::history::RecordingStatus::default(),
            }),
            history_desk: crate::history_desk::HistoryDesk::default(),
        }
    }

    /// Point the recorder at the settings' choices (applied by the
    /// sampler on its next sample).
    pub fn apply_history_settings(&self, settings: &Settings) {
        let mut history = self.history.lock().unwrap();
        history.enabled = settings.history_enabled;
        history.limits = settings.history_limits();
    }
}

/// The sampler's side of the history: opens the recorder when wanted
/// (retrying a busy lock every 5 s: a `sysmon serve --record` that is
/// still handing over), writes each sample, and reports.
struct HistoryWriter {
    recorder: Option<sysmon_core::history::Recorder>,
    last_attempt: Option<Instant>,
    retry_after: Duration,
}

impl HistoryWriter {
    fn write(&mut self, shared: &SharedUi, snapshot: &SystemSnapshot) {
        use sysmon_core::history::{HistoryRecord, OpenError, Recorder, RecordingStatus};
        let (enabled, dir, limits) = {
            let history = shared.history.lock().unwrap();
            (history.enabled, history.dir.clone(), history.limits)
        };
        if !enabled {
            // Off: drop the writer (and its lock), and say so with the
            // place and limits it would use. A clear still works.
            self.recorder = None;
            shared.history_desk.serve(None, &dir);
            shared.history.lock().unwrap().status = RecordingStatus {
                dir,
                hot_seconds: limits.hot_seconds,
                keep_seconds: limits.keep_seconds,
                max_bytes: limits.max_bytes,
                ..Default::default()
            };
            return;
        }
        if self.recorder.is_none() && self.last_attempt.is_none_or(|at| at.elapsed() > self.retry_after) {
            self.last_attempt = Some(Instant::now());
            match Recorder::open(&dir, limits, "gui") {
                Ok(recorder) => self.recorder = Some(recorder),
                Err(error) => {
                    self.retry_after =
                        Duration::from_secs(if matches!(error, OpenError::Busy(_)) { 5 } else { 30 });
                    let problem = match &error {
                        OpenError::Busy(_) => format!("{error}; this window will take over when it stops"),
                        OpenError::Io(_) => format!("cannot write {}: {error}", dir.display()),
                    };
                    shared.history_desk.serve(None, &dir);
                    shared.history.lock().unwrap().status = RecordingStatus {
                        dir,
                        hot_seconds: limits.hot_seconds,
                        keep_seconds: limits.keep_seconds,
                        max_bytes: limits.max_bytes,
                        problem: Some(problem),
                        ..Default::default()
                    };
                    return;
                }
            }
        }
        let Some(recorder) = &mut self.recorder else { return };
        recorder.set_limits(limits);
        shared.history_desk.serve(Some(recorder), &dir);
        let mut status = match recorder.record(&HistoryRecord::from_snapshot(snapshot)) {
            Ok(()) => recorder.status(),
            Err(error) => {
                // A full disk or a removed directory: say so, and try a
                // fresh open later.
                let mut status = recorder.status();
                status.recording = false;
                status.problem = Some(format!("writing history failed: {error}"));
                self.recorder = None;
                status
            }
        };
        status.dir = dir;
        shared.history.lock().unwrap().status = status;
    }
}

/// The sampler thread: samples at the user cadence, publishes into
/// SharedUi, pokes every live viewport.
fn spawn_sampler(ctx: egui::Context, shared: Arc<SharedUi>) {
    std::thread::Builder::new()
        .name("sysmon-sampler".to_string())
        .spawn(move || {
            let mut sampler = Sampler::new();
            let mut history = HistoryWriter { recorder: None, last_attempt: None, retry_after: Duration::ZERO };
            loop {
                let started = Instant::now();
                if !shared.paused.load(Ordering::Relaxed) {
                    let mut wants = Wants::all();
                    wants.system = true;
                    wants.connections = shared.connections_wanted.load(Ordering::Relaxed);
                    wants.cpu_occupants = shared.occupants_wanted.load(Ordering::Relaxed);
                    let snapshot = Arc::new(sampler.sample(wants));

                    // Histories and `latest` are published under ONE
                    // guard: a frame that reads histories first and then
                    // latest (overview()) sees the same sample in both
                    // (reviewer R4, FM V1). Readers take histories first.
                    {
                        let mut histories = shared.histories.write().unwrap();
                        histories.record(&snapshot);
                        *shared.latest.write().unwrap() = snapshot.clone();
                    }
                    // Then the disk: a slow write never holds the lock
                    // the frames read under.
                    history.write(&shared, &snapshot);

                    ctx.request_repaint();
                    for viewport in shared.open_viewports.lock().unwrap().iter() {
                        ctx.request_repaint_of(*viewport);
                    }
                }
                let interval = *shared.interval_seconds.lock().unwrap();
                let elapsed = started.elapsed();
                let remaining = Duration::from_secs_f64(interval.max(0.5))
                    .saturating_sub(elapsed)
                    .max(Duration::from_millis(100));
                std::thread::sleep(remaining);
            }
        })
        .expect("sampler thread");
}

pub struct SysMonApp {
    pub settings: Settings,
    shared: Arc<SharedUi>,
    icon_cache: IconCache,
    pub page: Page,
    pub table_state: ProcessTableState,
    details_open: Vec<i32>,
    /// Each details window's own thread reading.
    details_threads: HashMap<i32, super::threads_view::ThreadsState>,
    /// The one combined-details window (multi-select → Details);
    /// opening a new selection replaces it.
    combined_details_open: Option<Vec<i32>>,
    pending_confirm: Option<PendingConfirm>,
    toast_tx: std::sync::mpsc::Sender<String>,
    toast_rx: std::sync::mpsc::Receiver<String>,
    toasts: Vec<actions::Toast>,
    /// The copy buttons' off-thread spec reader.
    specs_copier: specs::SpecsCopier,
    applied_palette_id: String,
    system_palette: &'static Palette,
    system_palette_checked: Instant,
    /// Pin state per pop-out viewport (pinned-on-top by default).
    popout_pins: HashMap<&'static str, bool>,
    command_rx: std::sync::mpsc::Receiver<GuiCommand>,
    /// One in-flight `shot` at a time: (reply, requested path).
    pending_screenshot: Option<PendingScreenshot>,
    /// Drag state per popped-out section (drag-anywhere → dock).
    popout_drags: HashMap<&'static str, PopoutDrag>,
    /// Fit-to-content state per popped-out section.
    popout_fits: HashMap<&'static str, PopoutFit>,
    /// One-shot: scroll the Overview to this card (the Processes
    /// summary strip's way back).
    overview_scroll_to: Option<&'static str>,
    /// ☰ → Usage history → Export…: the path step, then the write.
    export_dialog: Option<ExportDialog>,
    /// ☰ → Usage history → Clear…: the confirm step, with what goes.
    clear_dialog: Option<sysmon_core::history::ClearReport>,
    /// A confirmed clear, waiting for the recorder's answer.
    clear_pending: Option<std::sync::mpsc::Receiver<crate::history_desk::HistoryReply>>,
    /// The oldest record on disk when this window is not the one
    /// recording: (read at, ts).
    history_oldest_cache: (Option<Instant>, Option<f64>),
    /// Keeps the socket alive exactly as long as the app; Drop
    /// unlinks it.
    _control_server: Option<crate::control::ControlServer>,
}

/// The `shot` verb's deferred reply: where to answer, where to write.
type PendingScreenshot = (
    std::sync::mpsc::Sender<Result<serde_json::Value, crate::control::VerbError>>,
    Option<String>,
);

/// A pop-out sizes itself to its card (no scrollbar, no spare room)
/// until the user resizes it by hand; from then on it keeps their size.
#[derive(Clone, Copy, Default)]
struct PopoutFit {
    /// First frame of this pop-out.
    opened: Option<Instant>,
    /// The inner size last asked of the compositor, when, and how many
    /// times in a row.
    requested: Option<egui::Vec2>,
    sent_at: Option<Instant>,
    sends: u8,
    /// The window reached `requested` at least once.
    settled: bool,
    /// The size moved away from a settled request: the user's now.
    user_sized: bool,
}

impl PopoutFit {
    /// The size to ask for this frame, if any. Width grows at once but
    /// shrinks only past a margin, so a value gaining and losing a digit
    /// between samples doesn't make the window breathe.
    fn next_request(&mut self, current: Option<egui::Vec2>, desired: egui::Vec2) -> Option<egui::Vec2> {
        if let (Some(requested), Some(current)) = (self.requested, current) {
            let reached = (current - requested).abs().max_elem() <= 2.0;
            if reached {
                self.settled = true;
            } else if self.settled {
                self.user_sized = true;
            }
        }
        if self.user_sized {
            return None;
        }
        let target = match self.requested {
            None => desired,
            Some(requested) => egui::vec2(
                if desired.x > requested.x + 1.0 || desired.x < requested.x - 24.0 { desired.x } else { requested.x },
                if (desired.y - requested.y).abs() > 2.0 { desired.y } else { requested.y },
            ),
        };
        if self.requested == Some(target) {
            // A request that lands while the compositor is still
            // configuring the last one can be dropped (seen on Sway:
            // a second size within a frame or two never arrived). Ask
            // again a few times, then accept what the desktop gives
            // (a tiling layout owns the size).
            let stale = self.sent_at.is_some_and(|at| at.elapsed() > Duration::from_millis(400));
            if self.settled || !stale || self.sends >= 4 {
                return None;
            }
            self.sends += 1;
        } else {
            self.sends = 1;
        }
        self.requested = Some(target);
        self.sent_at = Some(Instant::now());
        self.settled = false;
        Some(target)
    }
}

#[derive(Clone, Copy)]
struct PopoutDrag {
    opened_at: Instant,
    last_outer_min: Option<egui::Pos2>,
    dragging: bool,
}

impl SysMonApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        shared: Arc<SharedUi>,
        command_rx: std::sync::mpsc::Receiver<GuiCommand>,
        control_server: Option<crate::control::ControlServer>,
    ) -> Self {
        shared.apply_history_settings(&settings);
        spawn_sampler(cc.egui_ctx.clone(), shared.clone());
        let (toast_tx, toast_rx) = std::sync::mpsc::channel();

        // Name the adapter the window landed on. A CPU rasterizer
        // (llvmpipe — no usable Vulkan driver) still works, but slow
        // and CPU-hungry; a monitor must never be mysteriously slow,
        // so the degraded mode is disclosed: stderr with a fix, a
        // toast in the window, and `status` on the wire.
        if let Some(render_state) = &cc.wgpu_render_state {
            let info = render_state.adapter.get_info();
            let degraded = info.device_type == wgpu::DeviceType::Cpu;
            let description = format!("{} · {:?}", info.name, info.backend);
            if degraded {
                eprintln!(
                    "sysmon: rendering on a CPU rasterizer ({description}) — \
                     no GPU acceleration, the window costs more CPU than it should"
                );
                eprintln!(
                    "fix: install Vulkan drivers for this GPU (e.g. mesa-vulkan-drivers), \
                     then relaunch"
                );
                let _ = toast_tx.send(
                    "No GPU acceleration — rendering on the CPU. \
                     Vulkan drivers would fix this."
                        .to_string(),
                );
            }
            let _ = shared.renderer.set(RendererInfo {
                description,
                degraded,
            });
        }

        // The table's sort choice survives restarts.
        let mut table_state = ProcessTableState::default();
        if let Some(column) = SortColumn::from_id(&settings.sort_column) {
            table_state.sort_column = column;
            table_state.sort_descending = settings.sort_descending;
        }
        table_state.group_by_app = settings.group_by_app;

        SysMonApp {
            shared,
            icon_cache: IconCache::new(),
            page: Page::from_id(&settings.start_page).unwrap_or(Page::Overview),
            table_state,
            details_open: Vec::new(),
            details_threads: HashMap::new(),
            combined_details_open: None,
            pending_confirm: None,
            toast_tx,
            toast_rx,
            specs_copier: specs::SpecsCopier::default(),
            toasts: Vec::new(),
            applied_palette_id: String::new(),
            system_palette: theme::palette_for_system(),
            system_palette_checked: Instant::now(),
            popout_pins: HashMap::new(),
            command_rx,
            pending_screenshot: None,
            popout_drags: HashMap::new(),
            popout_fits: HashMap::new(),
            overview_scroll_to: None,
            export_dialog: None,
            clear_dialog: None,
            clear_pending: None,
            history_oldest_cache: (None, None),
            _control_server: control_server,
            settings,
        }
    }

    /// The state shared with the sampler and the socket (tests read the
    /// history recorder's status through it).
    pub fn shared(&self) -> &Arc<SharedUi> {
        &self.shared
    }

    /// The open combined-details selection, if any (tests + future
    /// introspection verbs read this).
    pub fn combined_details(&self) -> Option<&[i32]> {
        self.combined_details_open.as_deref()
    }

    pub fn palette(&mut self) -> &'static Palette {
        if self.settings.theme_mode == "system" {
            // gsettings is a subprocess — refresh at a gentle cadence.
            if self.system_palette_checked.elapsed() > Duration::from_secs(10) {
                self.system_palette = theme::palette_for_system();
                self.system_palette_checked = Instant::now();
            }
            self.system_palette
        } else {
            theme::palette_by_id(&self.settings.theme_mode).unwrap_or(&theme::PALETTES[0])
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        if self.applied_palette_id != palette.id {
            theme::apply(ctx, palette);
            self.applied_palette_id = palette.id.to_string();
        }
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        egui::TopBottomPanel::top("controls")
            .frame(
                egui::Frame::new()
                    .fill(palette.surface)
                    .inner_margin(egui::Margin::symmetric(8, 5)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let paused = self.shared.paused.load(Ordering::Relaxed);
                    let pause_glyph = if paused { ButtonGlyph::Play } else { ButtonGlyph::Pause };
                    let pause_tooltip = if paused { "Resume updates" } else { "Pause updates" };
                    if glyph_button(ui, palette, pause_glyph, paused, pause_tooltip).clicked() {
                        self.shared.paused.store(!paused, Ordering::Relaxed);
                    }
                    if self.settings.show_pin_button
                        && glyph_button(
                            ui,
                            palette,
                            ButtonGlyph::Pin,
                            self.settings.always_on_top,
                            "Keep window on top (P)",
                        )
                        .clicked()
                    {
                        self.settings.always_on_top = !self.settings.always_on_top;
                        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(
                            if self.settings.always_on_top {
                                egui::WindowLevel::AlwaysOnTop
                            } else {
                                egui::WindowLevel::Normal
                            },
                        ));
                        self.settings.save();
                    }
                    let copying = self.specs_copier.is_busy();
                    if glyph_button(ui, palette, ButtonGlyph::Copy, copying, "Copy system specs").clicked() {
                        self.specs_copier.request(
                            ctx,
                            specs::CopyJob { title: "System specs".to_string(), specs: specs::SpecsPart::Whole, history: None },
                        );
                    }

                    // Page switcher, centered-ish.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let menu_response = glyph_button(
                            ui,
                            palette,
                            ButtonGlyph::Menu,
                            false,
                            "Display options",
                        );
                        self.settings_menu(ui, &menu_response);

                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add_space((ui.available_width() / 2.0 - 80.0).max(0.0));
                            for (label, page) in [
                                ("Overview", Page::Overview),
                                ("Processes", Page::Processes),
                            ] {
                                let selected = self.page == page;
                                let text = RichText::new(label).size(12.5).color(if selected {
                                    palette.ink
                                } else {
                                    palette.muted
                                });
                                let response = ui.selectable_label(selected, text);
                                if response.clicked() {
                                    self.page = page;
                                    self.settings.start_page = page.id().to_string();
                                    self.settings.save();
                                }
                            }
                        });
                    });
                });
            });
    }

    fn settings_menu(&mut self, _ui: &mut egui::Ui, button_response: &egui::Response) {
        let mut settings_changed = false;
        egui::Popup::menu(button_response)
            .align(egui::RectAlign::BOTTOM_END)
            .show(|ui| {
                // The menu outgrew the window (3.5's history rows; at
                // Ben's 842 px the last rows were already cut in 3.4, and
                // a popup is clamped to the window): it scrolls instead.
                let max_height = (ui.ctx().content_rect().height() - button_response.rect.bottom() - 16.0).max(200.0);
                // No drag-to-scroll: a press on a chip must be a click, never
                // the start of a scroll (wheel and scrollbar scroll it).
                // min_scrolled_height: the popup's first (sizing) frame
                // offers only egui's default 400 px area, and the area
                // keeps whatever size that frame reported, so without it
                // the menu stayed a 400 px scroller with rows clipped.
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .min_scrolled_height(max_height)
                    .auto_shrink([true, true])
                    .scroll_source(egui::scroll_area::ScrollSource { drag: false, ..egui::scroll_area::ScrollSource::ALL })
                    .show(ui, |ui| {
                ui.set_min_width(210.0);
                let palette = theme::palette_by_id(&self.applied_palette_id)
                    .unwrap_or(&theme::PALETTES[0]);
                let heading = |ui: &mut egui::Ui, text: &str| {
                    ui.label(RichText::new(text).color(palette.muted).size(10.5));
                };

                // Every option row/chip below is a menu_* widget —
                // full-target, hover-lit (Ben: bare radios showed no
                // hover effect at all).
                heading(ui, "Appearance");
                let before = self.settings.theme_mode.clone();
                if menu_option_row(
                    ui,
                    palette,
                    self.settings.theme_mode == "system",
                    "Follow system theme",
                )
                .clicked()
                {
                    self.settings.theme_mode = "system".to_string();
                }
                for candidate in theme::PALETTES.iter() {
                    if menu_option_row(
                        ui,
                        palette,
                        self.settings.theme_mode == candidate.id,
                        candidate.label,
                    )
                    .clicked()
                    {
                        self.settings.theme_mode = candidate.id.to_string();
                    }
                }
                if before != self.settings.theme_mode {
                    settings_changed = true;
                    // Companion graph palette (v1 behavior).
                    if let Some(companion) =
                        theme::companion_graph_palette(&self.settings.theme_mode)
                    {
                        self.settings.graph_palette = companion.to_string();
                    }
                }

                ui.separator();
                heading(ui, "Graph colours");
                ui.horizontal_wrapped(|ui| {
                    for graph_palette in theme::GRAPH_PALETTES.iter() {
                        if menu_chip(
                            ui,
                            palette,
                            self.settings.graph_palette == graph_palette.id,
                            graph_palette.label,
                        )
                        .clicked()
                        {
                            self.settings.graph_palette = graph_palette.id.to_string();
                            settings_changed = true;
                        }
                    }
                });

                ui.separator();
                heading(ui, "Sizes");
                ui.horizontal(|ui| {
                    if menu_chip(ui, palette, !self.settings.use_binary_units, "GB (decimal)")
                        .on_hover_text("1 GB = 1,000,000,000 bytes — what drive boxes and ISPs quote")
                        .clicked()
                    {
                        self.settings.use_binary_units = false;
                        settings_changed = true;
                    }
                    if menu_chip(ui, palette, self.settings.use_binary_units, "GiB (binary)")
                        .on_hover_text("1 GiB = 1,073,741,824 bytes — what htop and the kernel count in")
                        .clicked()
                    {
                        self.settings.use_binary_units = true;
                        settings_changed = true;
                    }
                });

                ui.separator();
                heading(ui, "Temperature");
                ui.horizontal(|ui| {
                    let current = self.settings.display().temperature;
                    for (scale, label, hover) in [
                        (TemperatureScale::Celsius, "°C", "Celsius"),
                        (TemperatureScale::Fahrenheit, "°F", "Fahrenheit"),
                        (TemperatureScale::Both, "°F + °C", "both at once — 131°F · 55°C"),
                    ] {
                        if menu_chip(ui, palette, current == scale, label).on_hover_text(hover).clicked() {
                            self.settings.set_temperature_scale(scale);
                            settings_changed = true;
                        }
                    }
                });

                ui.separator();
                heading(ui, "Update interval");
                ui.horizontal(|ui| {
                    for interval in [1.0f64, 2.0, 3.0, 5.0] {
                        let selected =
                            (self.settings.update_interval_seconds - interval).abs() < 0.01;
                        if menu_chip(ui, palette, selected, &format!("{interval:.0}s")).clicked()
                        {
                            self.settings.update_interval_seconds = interval;
                            *self.shared.interval_seconds.lock().unwrap() = interval;
                            settings_changed = true;
                        }
                    }
                });

                ui.separator();
                heading(ui, "Window");
                if menu_check_row(ui, palette, self.settings.always_on_top, "Always on top")
                    .clicked()
                {
                    self.settings.always_on_top = !self.settings.always_on_top;
                    ui.ctx().send_viewport_cmd(ViewportCommand::WindowLevel(
                        if self.settings.always_on_top {
                            egui::WindowLevel::AlwaysOnTop
                        } else {
                            egui::WindowLevel::Normal
                        },
                    ));
                    settings_changed = true;
                }
                if menu_check_row(ui, palette, self.settings.show_pin_button, "Show pin button")
                    .clicked()
                {
                    self.settings.show_pin_button = !self.settings.show_pin_button;
                    settings_changed = true;
                }
                if menu_check_row(ui, palette, self.settings.compact_mode, "Compact mode")
                    .on_hover_text("shrink graphs and hide detail rows — for a screen corner")
                    .clicked()
                {
                    self.settings.compact_mode = !self.settings.compact_mode;
                    settings_changed = true;
                }

                ui.separator();
                heading(ui, "Usage history");
                if menu_check_row(ui, palette, self.settings.history_enabled, "Record usage history")
                    .on_hover_text(
                        "CPU, memory, the GPUs and their programs, network, disks and sensors, \
                         one line per sample, kept on disk for the copy buttons and `sysmon history`",
                    )
                    .clicked()
                {
                    self.settings.history_enabled = !self.settings.history_enabled;
                    settings_changed = true;
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("hourly for").color(palette.muted).size(10.5));
                    for hours in HOT_CHOICES_HOURS {
                        let label = sysmon_core::history::text::range_label(hours * 3_600.0);
                        if menu_chip(ui, palette, (self.settings.history_hot_hours - hours).abs() < 0.01, &label)
                            .on_hover_text(
                                "how long hours stay hourly files; after that each day is packed, \
                                 losslessly, into one smaller archive file",
                            )
                            .clicked()
                        {
                            self.settings.history_hot_hours = hours;
                            settings_changed = true;
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("keep").color(palette.muted).size(10.5));
                    for days in KEEP_CHOICES_DAYS {
                        let label = match days as u32 {
                            0 => "forever".to_string(),
                            365 => "1 y".to_string(),
                            other => format!("{other} d"),
                        };
                        let hover = if days == 0.0 {
                            "nothing is ever removed by age (the default)"
                        } else {
                            "older days are removed for good"
                        };
                        if menu_chip(ui, palette, (self.settings.history_keep_days - days).abs() < 0.01, &label)
                            .on_hover_text(hover)
                            .clicked()
                        {
                            self.settings.history_keep_days = days;
                            settings_changed = true;
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("size cap").color(palette.muted).size(10.5));
                    for megabytes in CAP_CHOICES_MB {
                        let label = if megabytes == 0 { "none".to_string() } else { format!("{} GB", megabytes / 1_000) };
                        let hover = if megabytes == 0 {
                            "no size limit (the default); past 1 GB the line below warns"
                        } else {
                            "past this size the oldest days are removed for good"
                        };
                        if menu_chip(ui, palette, self.settings.history_cap_mb == megabytes, &label)
                            .on_hover_text(hover)
                            .clicked()
                        {
                            self.settings.history_cap_mb = megabytes;
                            settings_changed = true;
                        }
                    }
                });
                let status = self.shared.history.lock().unwrap().status.clone();
                for (line, alert) in history_status_lines(&status) {
                    ui.label(RichText::new(line).color(if alert { palette.accent } else { palette.muted }).size(10.5))
                        .on_hover_text(status.dir.display().to_string());
                }
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("Export…").size(11.5))
                        .on_hover_text("save the whole history as one .tar.zst file")
                        .clicked()
                    {
                        self.export_dialog = Some(ExportDialog::new(&status.dir));
                    }
                    if ui
                        .button(RichText::new("Clear…").size(11.5))
                        .on_hover_text("delete the whole history (asks first)")
                        .clicked()
                    {
                        self.clear_dialog = Some(sysmon_core::history::manage::clear_preview(&status.dir));
                    }
                });

                ui.separator();
                heading(ui, "Overview sections");
                let nvidia_here = nvidia_present(&self.shared.latest.read().unwrap());
                for key in SECTION_KEYS {
                    // No NVIDIA card, no row for one.
                    if key == "nvidia" && !nvidia_here {
                        continue;
                    }
                    let visible = self.settings.section_visible(key);
                    let label = match key {
                        "gpu" => "GPU",
                        "nvidia" => "NVIDIA",
                        "memory" => "Memory",
                        "cpu" => "CPU",
                        "network" => "Network",
                        "disks" => "Disks",
                        "sensors" => "Sensors",
                        _ => key,
                    };
                    if menu_check_row(ui, palette, visible, label).clicked() {
                        self.settings.visible_sections.insert(key.to_string(), !visible);
                        if visible {
                            self.settings.popped_out_sections.retain(|s| s != key);
                        }
                        settings_changed = true;
                    }
                }
                });
            });
        if settings_changed {
            self.shared.apply_history_settings(&self.settings);
            self.settings.save();
        }
    }

    /// The Overview: every visible card that isn't popped out, in one
    /// column, or two once the window is wide (3.2's Performance page
    /// did that; 3.6 folded it in here).
    fn overview(&mut self, ui: &mut egui::Ui, actions_out: &mut Vec<AppAction>) {
        let palette = self.palette();
        // Histories first, then latest: the sampler writes both under
        // the histories guard, so this order sees one sample (V1).
        let histories = self.shared.histories.read().unwrap();
        let snapshot = self.shared.latest.read().unwrap().clone();
        let scroll_to = self.overview_scroll_to.take();
        let nvidia_here = nvidia_present(&snapshot);
        let sections: Vec<&'static str> = SECTION_KEYS
            .iter()
            .copied()
            .filter(|section| *section != "nvidia" || nvidia_here)
            .filter(|section| {
                self.settings.section_visible(section) && !self.settings.popped_out_sections.iter().any(|s| s == section)
            })
            .collect();
        let slots = overview_slots(&sections);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                let mut cx = CardContext::new(
                    palette,
                    &self.settings,
                    &snapshot,
                    &histories,
                    &mut self.icon_cache,
                    actions_out,
                    &mut self.table_state.selected_pids,
                    self.settings.compact_mode,
                );
                let draw = |ui: &mut egui::Ui, cx: &mut CardContext, section: &'static str| {
                    let top = ui.cursor().min;
                    cards::draw_card(ui, cx, section);
                    if scroll_to == Some(section) {
                        ui.scroll_to_rect(egui::Rect::from_min_max(top, ui.cursor().min), Some(Align::TOP));
                    }
                    ui.add_space(6.0);
                };
                if ui.available_width() >= overview::TWO_COLUMN_WIDTH && slots.len() > 1 {
                    // Slots alternate between the columns in their
                    // usual order: the left reads gpu (with the NVIDIA
                    // card under it), cpu, disks; the right memory,
                    // network, sensors.
                    let gap = 8.0;
                    let column = (ui.available_width() - gap) / 2.0;
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for side in 0..2 {
                            ui.allocate_ui_with_layout(egui::vec2(column, 0.0), Layout::top_down(Align::Min), |ui| {
                                ui.set_width(column);
                                for slot in slots.iter().skip(side).step_by(2) {
                                    for section in slot {
                                        draw(ui, &mut cx, section);
                                    }
                                }
                            });
                        }
                    });
                } else {
                    for section in &sections {
                        draw(ui, &mut cx, section);
                    }
                }
            });
    }

    /// Pop-out viewports: each popped section is an immediate child
    /// window, pinned on top by default (v1 behavior).
    fn popout_viewports(&mut self, ctx: &egui::Context, actions_out: &mut Vec<AppAction>) {
        let popped: Vec<&'static str> = SECTION_KEYS
            .iter()
            .copied()
            .filter(|key| self.settings.popped_out_sections.iter().any(|s| s == key))
            .collect();
        // A section docked by any path (button, drag, ctl) fits afresh
        // when it pops out again.
        self.popout_fits.retain(|key, _| popped.contains(key));
        if popped.is_empty() {
            return;
        }
        let snapshot = self.shared.latest.read().unwrap().clone();

        for section in popped {
            let viewport_id = ViewportId::from_hash_of(("popout", section));
            let title = match section {
                "gpu" => "GPU — SysMon",
                "nvidia" => "NVIDIA — SysMon",
                "memory" => "Memory — SysMon",
                "cpu" => "CPU — SysMon",
                "network" => "Network — SysMon",
                "disks" => "Disks — SysMon",
                "sensors" => "Sensors — SysMon",
                _ => "SysMon",
            };
            let pinned = *self.popout_pins.entry(section).or_insert(true);
            let mut close_requested = false;

            let palette = self.palette();
            // Through a local handle: the closure below needs `&mut self`
            // (the copy range slider) while the read guard is held.
            let shared = self.shared.clone();
            let histories = shared.histories.read().unwrap();
            let builder = ViewportBuilder::default()
                .with_title(title)
                // One app id for every SysMon window, so the desktop
                // treats a pop-out as SysMon's (Ben's Sway floats
                // `app_id=sysmon`; without an id a Wayland pop-out had
                // none and opened tiled).
                .with_app_id(super::APP_ID)
                .with_inner_size([440.0, 400.0])
                .with_min_inner_size([300.0, 160.0])
                .with_always_on_top();
            ctx.show_viewport_immediate(viewport_id, builder, |ctx, _class| {
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(palette.plane)
                            .inner_margin(egui::Margin::same(8)),
                    )
                    .show(ctx, |ui| {
                        // v1's gesture: the whole body is a drag
                        // handle. A background interact registered
                        // FIRST loses to every widget drawn after it,
                        // so buttons/scroll still work; dragging the
                        // leftovers moves the window.
                        let body = ui.interact(
                            ui.max_rect(),
                            ui.id().with("popout_body_drag"),
                            egui::Sense::drag(),
                        );
                        if body.drag_started() {
                            ctx.send_viewport_cmd_to(
                                viewport_id,
                                ViewportCommand::StartDrag,
                            );
                        }
                        ui.horizontal(|ui| {
                            let pin_response = glyph_button(
                                ui,
                                palette,
                                ButtonGlyph::Pin,
                                pinned,
                                "Keep above other windows",
                            );
                            if pin_response.clicked() {
                                let now_pinned = !pinned;
                                self.popout_pins.insert(section, now_pinned);
                                ctx.send_viewport_cmd_to(
                                    viewport_id,
                                    ViewportCommand::WindowLevel(if now_pinned {
                                        egui::WindowLevel::AlwaysOnTop
                                    } else {
                                        egui::WindowLevel::Normal
                                    }),
                                );
                            }
                            let copying = self.specs_copier.is_busy();
                            let span = self.history_span();
                            let tooltip = format!("Copy {} specs and the last {} of usage", specs::section_title(section), sysmon_core::history::text::range_label(self.settings.copy_range(span)));
                            if glyph_button(ui, palette, ButtonGlyph::Copy, copying, &tooltip).clicked() {
                                self.copy_section(ctx, section);
                            }
                            self.copy_range_slider(ui, palette);
                            if ui.available_width() > 190.0 {
                                ui.label(
                                    RichText::new("drag onto the main window to dock")
                                        .color(palette.muted)
                                        .size(10.0),
                                );
                            }
                        });
                        let scroll_top = ui.cursor().top();
                        let scroll = egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let mut cx = CardContext::new(
                                    palette,
                                    &self.settings,
                                    &snapshot,
                                    &histories,
                                    &mut self.icon_cache,
                                    actions_out,
                                    &mut self.table_state.selected_pids,
                                    false,
                                );
                                let chrome = cards::draw_card(ui, &mut cx, section);
                                chrome + cx.want_width.get()
                            });
                        // The window the card asks for: its content
                        // width plus the panel margins, and the whole
                        // card's height under the button row, capped
                        // by the monitor (a tall Sensors card scrolls).
                        let margin = 8.0;
                        let monitor = ctx
                            .input(|input| input.viewport().monitor_size)
                            .unwrap_or(egui::vec2(1920.0, 1080.0));
                        let desired = egui::vec2(
                            (scroll.inner + 2.0 * margin).ceil().clamp(300.0, monitor.x * 0.9),
                            (scroll_top + scroll.content_size.y + margin).ceil().clamp(160.0, monitor.y * 0.85),
                        );
                        // The viewport's own size: Wayland reports no
                        // inner_rect, so egui's content rect is the one
                        // reading every backend has.
                        let current = Some(ctx.content_rect().size());
                        let fit = self.popout_fits.entry(section).or_default();
                        // Fit to a full card: before the first complete
                        // sample a card is a stub (and the top-3 list
                        // arrives a sample late), so fitting earlier
                        // shrinks the window and grows it straight back.
                        let opened = *fit.opened.get_or_insert_with(Instant::now);
                        let ready = snapshot.processes.is_some() || opened.elapsed() > Duration::from_secs(3);
                        let request = if ready { fit.next_request(current, desired) } else { None };
                        if std::env::var_os("SYSMON_DEBUG_FIT").is_some() {
                            eprintln!("fit[{section}] current={current:?} desired={desired:?} request={request:?} settled={} user={}", fit.settled, fit.user_sized);
                        }
                        if let Some(size) = request {
                            ctx.send_viewport_cmd_to(viewport_id, ViewportCommand::InnerSize(size));
                        }
                        if !fit.settled && !fit.user_sized {
                            // Come back to measure at the new size (or
                            // ask again) without waiting for a sample. An
                            // immediate viewport draws inside the root's
                            // frame, so the root is the one to wake.
                            ctx.request_repaint_after_for(Duration::from_millis(450), ViewportId::ROOT);
                        }
                    });
                if ctx.input(|input| input.viewport().close_requested()) {
                    close_requested = true;
                }

                // Drag-anywhere-to-dock (v1's gesture): after a grace
                // period, movement while button 1 is down arms the
                // drag; releasing it over the main window docks the
                // card. X11 supplies the button state; elsewhere the
                // gesture quietly doesn't exist.
                let drag = self.popout_drags.entry(section).or_insert(PopoutDrag {
                    opened_at: Instant::now(),
                    last_outer_min: None,
                    dragging: false,
                });
                let outer = ctx.input(|input| input.viewport().outer_rect);
                if std::env::var_os("SYSMON_DEBUG_DRAG").is_some() {
                    eprintln!(
                        "drag[{section}] outer={outer:?} button={:?} dragging={} main={:?}",
                        x11_button1_down(),
                        drag.dragging,
                        *self.shared.main_window_rect.lock().unwrap(),
                    );
                }
                if let Some(outer) = outer {
                    let armed = drag.opened_at.elapsed() > Duration::from_millis(1200);
                    let moved = drag
                        .last_outer_min
                        .map(|last| (last - outer.min).length() > 1.0)
                        .unwrap_or(false);
                    drag.last_outer_min = Some(outer.min);
                    let button_down = x11_button1_down();
                    if armed && moved && button_down == Some(true) {
                        drag.dragging = true;
                    }
                    if drag.dragging {
                        // Keep polling until the button releases.
                        ctx.request_repaint_after(Duration::from_millis(120));
                        if button_down == Some(false) {
                            drag.dragging = false;
                            let main_rect = *self.shared.main_window_rect.lock().unwrap();
                            if let Some(main_rect) = main_rect
                                && main_rect.contains(outer.center())
                            {
                                close_requested = true; // dock it home
                            }
                        }
                    }
                }
            });

            if close_requested {
                self.settings.popped_out_sections.retain(|s| s != section);
                self.popout_drags.remove(section);
                self.popout_fits.remove(section);
                self.settings.save();
            }
        }
        // (Viewport poke registration moved to update() — it must run
        // even when no section is popped out.)
    }

    fn apply_actions(&mut self, ctx: &egui::Context, actions_in: Vec<AppAction>) {
        for action in actions_in {
            match action {
                AppAction::TogglePopOut(section) => {
                    if self.settings.popped_out_sections.iter().any(|s| s == section) {
                        self.settings.popped_out_sections.retain(|s| s != section);
                    } else {
                        self.settings.popped_out_sections.push(section.to_string());
                        self.popout_pins.insert(section, true);
                    }
                    self.settings.save();
                }
                AppAction::OpenDetails(pid) => {
                    if !self.details_open.contains(&pid) {
                        self.details_open.push(pid);
                    }
                }
                AppAction::OpenCombinedDetails(mut pids) => {
                    pids.dedup();
                    pids.truncate(cards::MAX_SELECTED);
                    if pids.len() >= 2 {
                        self.combined_details_open = Some(pids);
                    }
                }
                AppAction::RevealInProcesses(pid) => {
                    self.page = Page::Processes;
                    self.table_state.selected_pids = vec![pid];
                    self.table_state.reveal_pid = Some(pid);
                    self.table_state.inspector.inspect(pid);
                    self.clear_filter_hiding(pid);
                }
                AppAction::Inspect(pid) => {
                    // Inspecting from the Overview opens the Processes
                    // page with the row revealed; from the table it
                    // just retargets the Inspector.
                    if self.page != Page::Processes {
                        self.page = Page::Processes;
                        self.table_state.selected_pids = vec![pid];
                        self.table_state.reveal_pid = Some(pid);
                        self.clear_filter_hiding(pid);
                    }
                    self.table_state.inspector.inspect(pid);
                    if !self.settings.show_inspector {
                        // A narrow or inspector-less layout: the
                        // details window is the deep view.
                        if !self.details_open.contains(&pid) {
                            self.details_open.push(pid);
                        }
                    }
                }
                AppAction::ShowProcessesBy(column) => {
                    self.page = Page::Processes;
                    self.table_state.sort_column = column;
                    self.table_state.sort_descending = column.defaults_descending();
                }
                AppAction::ShowOverview(section) => {
                    self.page = Page::Overview;
                    self.overview_scroll_to = Some(section);
                }
                AppAction::SetGroupByApp(on) => {
                    self.table_state.group_by_app = on;
                    self.settings.group_by_app = on;
                    self.settings.save();
                }
                AppAction::SetInspector(on) => {
                    self.settings.show_inspector = on;
                    self.settings.save();
                }
                AppAction::ToggleSensorGroup(key) => {
                    self.settings.toggle_sensor_group(&key);
                    self.settings.save();
                }
                AppAction::SetCpuGraphMode(mode) => {
                    self.settings.cpu_graph_mode = mode.to_string();
                    self.settings.save();
                }
                AppAction::Notify(text) => {
                    let _ = self.toast_tx.send(text);
                }
                AppAction::ConfirmTerminate(pid, name) => {
                    self.pending_confirm = Some(PendingConfirm {
                        pid,
                        name,
                        kind: ConfirmKind::Terminate,
                    });
                }
                AppAction::ConfirmKill(pid, name) => {
                    self.pending_confirm = Some(PendingConfirm {
                        pid,
                        name,
                        kind: ConfirmKind::Kill,
                    });
                }
                AppAction::SetPriority(pid, name, nice) => {
                    actions::renice_process(pid, &name, nice, &self.toast_tx);
                }
                AppAction::CopyPid(pid) => {
                    ctx.copy_text(pid.to_string());
                }
                AppAction::CopySection(section) => {
                    self.copy_section(ctx, section);
                }
            }
        }
    }

    /// A section's copy: its specs (the Sensors readings stand in for
    /// specs there), then its usage over the copy range.
    fn copy_section(&mut self, ctx: &egui::Context, section: &'static str) {
        let span = self.history_span();
        let specs_part = match specs::spec_section_for_popout(section) {
            Some(spec_section) => specs::SpecsPart::Section(spec_section),
            None => specs::SpecsPart::Text(specs::sensor_readings_text(&self.shared.latest.read().unwrap())),
        };
        let display = self.settings.display();
        let job = specs::CopyJob {
            title: specs::section_title(section).to_string(),
            specs: specs_part,
            history: Some(specs::HistoryPart {
                section,
                range_seconds: self.settings.copy_range(span),
                dir: self.shared.history.lock().unwrap().dir.clone(),
                style: sysmon_core::history::text::TextStyle { units: display.units, temperature: display.temperature },
            }),
        };
        self.specs_copier.request(ctx, job);
    }

    /// Seconds from the oldest record on disk to now: the recorder's
    /// own account while this window records, else a cheap read of the
    /// directory, refreshed every 30 s (another SysMon may be recording).
    fn history_span(&mut self) -> Option<f64> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let (oldest, dir) = {
            let history = self.shared.history.lock().unwrap();
            (history.status.oldest_ts, history.dir.clone())
        };
        let oldest = match oldest {
            Some(oldest) => Some(oldest),
            None => {
                if self.history_oldest_cache.0.is_none_or(|at| at.elapsed() > Duration::from_secs(30)) {
                    self.history_oldest_cache = (Some(Instant::now()), sysmon_core::history::Recording::oldest_ts(&dir));
                }
                self.history_oldest_cache.1
            }
        };
        oldest.map(|oldest| (now - oldest).max(0.0))
    }

    /// The pop-outs' "how much history to copy" slider: stops from
    /// 5 min up to the oldest record on disk (both tiers).
    fn copy_range_slider(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let span = self.history_span();
        let stops = self.settings.copy_range_stops(span);
        let current = self.settings.copy_range(span);
        let mut index = stops.iter().position(|stop| *stop >= current - 0.5).unwrap_or(stops.len() - 1);
        let label = sysmon_core::history::text::range_label(stops[index]);
        let response = ui
            .scope(|ui| {
                ui.spacing_mut().slider_width = 84.0;
                ui.add(
                    egui::Slider::new(&mut index, 0..=stops.len() - 1)
                        .show_value(false)
                        .handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.5 }),
                )
            })
            .inner;
        response
            .widget_info(|| egui::WidgetInfo::slider(true, index as f64, format!("Copy range: last {label}")));
        let response = response.on_hover_text(
            "How much usage history the copy button includes, \
             up to the oldest record on disk.",
        );
        ui.label(RichText::new(format!("last {label}")).color(palette.muted).monospace().size(10.5));
        if response.changed() {
            let index = index.min(stops.len() - 1);
            // The last stop is "everything on disk": stored as the
            // longest range, so it keeps meaning all of it as the
            // history grows.
            self.settings.copy_range_seconds =
                if index == stops.len() - 1 { super::settings::MAX_COPY_RANGE_SECONDS } else { stops[index] };
            self.settings.save();
        }
    }

    /// A live filter that hides the target would make a jump land on
    /// nothing — clear it, disclosed.
    fn clear_filter_hiding(&mut self, pid: i32) {
        let filter = self.table_state.filter.to_lowercase();
        let hidden = self.shared.latest.read().unwrap().processes.as_ref().is_some_and(|records| {
            records.iter().find(|r| r.pid == pid).is_some_and(|r| !matches_filter(r, &filter))
        });
        if hidden {
            self.table_state.filter.clear();
            let _ = self.toast_tx.send("Filter cleared to reveal the process".to_string());
        }
    }

    fn confirm_and_toasts(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        if let Some(pending) = self.pending_confirm.clone()
            && let Some(confirmed) = actions::confirm_modal(ctx, palette, &pending)
        {
            if confirmed {
                actions::end_process(
                    pending.pid,
                    &pending.name,
                    pending.kind == ConfirmKind::Kill,
                    &self.toast_tx,
                );
            }
            self.pending_confirm = None;
        }

        self.history_dialogs(ctx, palette);

        // A finished spec read lands on the clipboard from a frame of
        // the main viewport (the clipboard belongs to the window).
        while let Some(copied) = self.specs_copier.poll() {
            ctx.copy_text(copied.text);
            let _ = self.toast_tx.send(copied.toast);
        }
        while let Ok(text) = self.toast_rx.try_recv() {
            self.toasts.push(actions::Toast {
                text,
                shown_at: Instant::now(),
            });
        }
        self.toasts
            .retain(|toast| toast.shown_at.elapsed() < Duration::from_secs(6));
        if !self.toasts.is_empty() {
            egui::TopBottomPanel::bottom("toasts")
                .frame(
                    egui::Frame::new()
                        .fill(palette.surface_2)
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ctx, |ui| {
                    for toast in &self.toasts {
                        ui.label(
                            RichText::new(&toast.text).color(palette.accent).size(11.5),
                        );
                    }
                });
        }
    }

    /// ☰ → Usage history's two steps: Export… (a path, then the write
    /// off the UI thread) and Clear… (what goes, then the recorder's
    /// own thread deletes it).
    fn history_dialogs(&mut self, ctx: &egui::Context, palette: &'static Palette) {
        let size = |bytes: u64| sysmon_core::units::format_size(bytes, sysmon_core::units::Units::Decimal);
        let dir = self.shared.history.lock().unwrap().dir.clone();

        if let Some(dialog) = &mut self.export_dialog {
            if let Some(running) = &dialog.running
                && let Ok(result) = running.try_recv()
            {
                dialog.running = None;
                match result {
                    Ok(report) => {
                        let _ = self.toast_tx.send(format!(
                            "History exported: {} files, {} → {}",
                            report.files,
                            size(report.bytes),
                            report.path.display()
                        ));
                        self.export_dialog = None;
                        return;
                    }
                    Err(error) => dialog.error = Some(error),
                }
            }
            let mut close = false;
            let mut start = false;
            let modal = egui::Modal::new(egui::Id::new("export_history")).show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.label(RichText::new("Export the usage history").strong().size(14.0));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("One .tar.zst file with every hour and archived day, as they are on disk.")
                        .color(palette.ink_2)
                        .size(12.0),
                );
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut dialog.path)
                        .desired_width(360.0)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("~/sysmon-history.tar.zst"),
                )
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Export path"));
                if let Some(error) = &dialog.error {
                    ui.label(RichText::new(error).color(palette.accent).size(11.5));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let busy = dialog.running.is_some();
                    let label = if busy { "Exporting…" } else { "Export" };
                    let button = egui::Button::new(RichText::new(label).color(palette.on_accent)).fill(palette.accent);
                    if ui.add_enabled(!busy, button).clicked() {
                        start = true;
                    }
                });
            });
            if modal.should_close() && dialog.running.is_none() {
                close = true;
            }
            if start {
                let raw = dialog.path.trim().to_string();
                let expanded = match raw.strip_prefix("~/") {
                    Some(rest) => std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(rest),
                    None => std::path::PathBuf::from(&raw),
                };
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
                let target = if expanded.is_dir() { expanded.join(sysmon_core::history::manage::default_export_name(now)) } else { expanded };
                if raw.is_empty() {
                    dialog.error = Some("type where the file should go".to_string());
                } else if target.starts_with(&dir) {
                    dialog.error = Some("not inside the history folder itself; pick another place".to_string());
                } else if target.exists() {
                    dialog.error = Some(format!("{} already exists; pick another name", target.display()));
                } else {
                    dialog.error = None;
                    let (tx, rx) = std::sync::mpsc::channel();
                    let source = dir.clone();
                    let ctx = ctx.clone();
                    let spawned = std::thread::Builder::new().name("sysmon-export".to_string()).spawn(move || {
                        let result = sysmon_core::history::manage::export(&source, &target, false).map_err(|error| format!("export failed: {error}"));
                        let _ = tx.send(result);
                        ctx.request_repaint();
                    });
                    match spawned {
                        Ok(_) => dialog.running = Some(rx),
                        Err(error) => dialog.error = Some(format!("could not start the export: {error}")),
                    }
                }
            }
            if close {
                self.export_dialog = None;
            }
        }

        if let Some(pending) = &self.clear_pending
            && let Ok(answer) = pending.try_recv()
        {
            self.clear_pending = None;
            let toast = match answer {
                Ok(result) => format!(
                    "History cleared: {} files, {} removed",
                    result["cleared"]["removed_files"].as_u64().unwrap_or(0),
                    size(result["cleared"]["removed_bytes"].as_u64().unwrap_or(0))
                ),
                Err(error) => format!("Could not clear the history: {}", error.message),
            };
            let _ = self.toast_tx.send(toast);
            self.history_oldest_cache = (None, None);
        }

        if let Some(preview) = self.clear_dialog.clone() {
            let mut decision = None;
            let modal = egui::Modal::new(egui::Id::new("clear_history")).show(ctx, |ui| {
                ui.set_max_width(340.0);
                ui.label(RichText::new("Delete the whole usage history?").strong().size(14.0));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "{} files, {}, from {}. This cannot be undone; Export… first keeps a copy. \
                         Recording carries on from now.",
                        preview.removed_files,
                        size(preview.removed_bytes),
                        preview.dir.display()
                    ))
                    .color(palette.ink_2)
                    .size(12.0),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                    let button = egui::Button::new(RichText::new("Delete history").color(palette.on_accent)).fill(palette.accent);
                    if ui.add(button).clicked() {
                        decision = Some(true);
                    }
                });
            });
            if modal.should_close() && decision.is_none() {
                decision = Some(false);
            }
            if let Some(confirmed) = decision {
                self.clear_dialog = None;
                if confirmed {
                    if self.shared.paused.load(Ordering::Relaxed) {
                        let _ = self.toast_tx.send("Resume sampling first: the recorder clears between samples".to_string());
                    } else {
                        self.clear_pending = Some(self.shared.history_desk.submit(crate::history_desk::HistoryOp::Clear));
                    }
                }
            }
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        // `P` is a bare letter — never steal it from a focused text
        // field (the filter box). Same guard for Escape-clears.
        let typing = ctx.wants_keyboard_input();
        let mut toggle_pin = false;
        let mut clear_selection = false;
        ctx.input_mut(|input| {
            use egui::{Key, KeyboardShortcut, Modifiers};
            if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::F)) {
                self.page = Page::Processes;
                self.table_state.focus_filter = true;
            }
            for (key, page) in [(Key::Num1, Page::Overview), (Key::Num2, Page::Processes)] {
                if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, key)) {
                    self.page = page;
                    // One meaning everywhere (R20): the window reopens on
                    // the page it was last on.
                    self.settings.start_page = page.id().to_string();
                    self.settings.save();
                }
            }
            if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Q)) {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            // Phosphor's pin shortcut, ported with its button.
            if !typing && input.consume_shortcut(&KeyboardShortcut::new(Modifiers::NONE, Key::P))
            {
                toggle_pin = true;
            }
            if !typing
                && !self.table_state.selected_pids.is_empty()
                && self.pending_confirm.is_none()
                && input.key_pressed(Key::Escape)
            {
                clear_selection = true;
            }
        });
        if toggle_pin {
            self.settings.always_on_top = !self.settings.always_on_top;
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if self.settings.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }));
            self.settings.save();
        }
        // Escape closes an open menu first; only a bare Escape drops
        // the multi-selection.
        if clear_selection && !egui::Popup::is_any_open(ctx) {
            self.table_state.selected_pids.clear();
        }
    }
}

impl SysMonApp {
    /// Drain ctl verbs queued by the socket backend — the one place
    /// UI state changes off a socket request.
    fn process_commands(&mut self, ctx: &egui::Context) {
        while let Ok(command) = self.command_rx.try_recv() {
            let GuiCommand {
                verb,
                value,
                path,
                reply,
            } = command;
            let value_or = |value: &Option<String>| value.clone().unwrap_or_default();
            let result: Result<serde_json::Value, crate::control::VerbError> = match verb.as_str()
            {
                "raise" => {
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    Ok(serde_json::json!({"raised": true}))
                }
                "page" => match Page::from_id(&value_or(&value)) {
                    Some(page) => {
                        self.page = page;
                        self.settings.start_page = page.id().to_string();
                        self.settings.save();
                        Ok(serde_json::json!({"page": page.id()}))
                    }
                    None if value_or(&value) == "performance" => Err(VerbError::bad_args(
                        "the Performance page was folded into the Overview in 3.6",
                        "`sysmon ctl page overview` (pages: overview processes)",
                    )),
                    None => Err(VerbError::bad_args(
                        format!("unknown page `{}`", value_or(&value)),
                        "pages: overview processes",
                    )),
                },
                // Open a process in the Inspector (Processes page) or its
                // own details window: the agent's way to the same
                // Threads & CPUs view a click opens.
                "inspect" | "details" => {
                    let wanted = value_or(&value);
                    let live = wanted.parse::<i32>().ok().and_then(|pid| {
                        let snapshot = self.shared.latest.read().unwrap();
                        snapshot
                            .processes
                            .as_ref()
                            .and_then(|records| records.iter().find(|r| r.pid == pid))
                            .map(|record| (pid, record.name.clone()))
                    });
                    match live {
                        Some((pid, name)) if verb == "inspect" => {
                            self.page = Page::Processes;
                            self.settings.show_inspector = true;
                            self.settings.save();
                            self.table_state.selected_pids = vec![pid];
                            self.table_state.reveal_pid = Some(pid);
                            self.table_state.inspector.inspect(pid);
                            self.clear_filter_hiding(pid);
                            Ok(serde_json::json!({"inspecting": pid, "name": name}))
                        }
                        Some((pid, name)) => {
                            if !self.details_open.contains(&pid) {
                                self.details_open.push(pid);
                            }
                            Ok(serde_json::json!({"details": pid, "name": name}))
                        }
                        None => Err(VerbError::bad_args(
                            format!("no process `{wanted}` in the latest sample"),
                            "give a live pid: `sysmon probe processes` lists them (or `pgrep <name>`)",
                        )),
                    }
                }
                "cpugraph" => match CpuGraphMode::from_id(&value_or(&value)) {
                    Some(mode) => {
                        self.settings.cpu_graph_mode = mode.id().to_string();
                        self.settings.save();
                        Ok(serde_json::json!({"cpugraph": mode.id()}))
                    }
                    None => Err(VerbError::bad_args(
                        format!("unknown CPU graph view `{}`", value_or(&value)),
                        "cpugraph: total threads grid (3.6 renamed them: combined is total; \
                         per_thread is threads, a line each, or grid, a graph each)",
                    )),
                },
                "theme" => {
                    let wanted = value_or(&value);
                    if wanted == "system" || theme::palette_by_id(&wanted).is_some() {
                        self.settings.theme_mode = wanted.clone();
                        if let Some(companion) = theme::companion_graph_palette(&wanted) {
                            self.settings.graph_palette = companion.to_string();
                        }
                        self.settings.save();
                        Ok(serde_json::json!({"theme": wanted}))
                    } else {
                        Err(VerbError::bad_args(
                            format!("unknown theme `{wanted}`"),
                            "themes: system blossom_dark blossom amoled light dark funky paper \
                             basalt amber chromacore",
                        ))
                    }
                }
                "palette" => {
                    let wanted = value_or(&value);
                    if theme::GRAPH_PALETTES.iter().any(|p| p.id == wanted) {
                        self.settings.graph_palette = wanted.clone();
                        self.settings.save();
                        Ok(serde_json::json!({"palette": wanted}))
                    } else {
                        Err(VerbError::bad_args(
                            format!("unknown graph palette `{wanted}`"),
                            "palettes: mint aqua sunset forest mono blossom funky amber terminal",
                        ))
                    }
                }
                "popout" | "popin" => {
                    let wanted = value_or(&value);
                    match SECTION_KEYS.iter().find(|key| **key == wanted) {
                        Some(section) => {
                            let currently = self
                                .settings
                                .popped_out_sections
                                .iter()
                                .any(|s| s == section);
                            if verb == "popout" && !currently {
                                self.settings.popped_out_sections.push(section.to_string());
                                self.popout_pins.insert(section, true);
                            } else if verb == "popin" && currently {
                                self.settings.popped_out_sections.retain(|s| s != section);
                            }
                            self.settings.save();
                            Ok(serde_json::json!({
                                "section": section,
                                "popped_out": verb == "popout",
                            }))
                        }
                        None => Err(VerbError::bad_args(
                            format!("unknown section `{wanted}`"),
                            "sections: gpu nvidia memory cpu network disks sensors",
                        )),
                    }
                }
                "compact" => {
                    let on = matches!(value_or(&value).as_str(), "on" | "true" | "1");
                    self.settings.compact_mode = on;
                    self.settings.save();
                    Ok(serde_json::json!({"compact": on}))
                }
                "units" => match value_or(&value).as_str() {
                    wanted @ ("binary" | "gib" | "decimal" | "gb") => {
                        let binary = matches!(wanted, "binary" | "gib");
                        self.settings.use_binary_units = binary;
                        self.settings.save();
                        Ok(serde_json::json!({
                            "units": if binary { "binary" } else { "decimal" },
                        }))
                    }
                    other => Err(VerbError::bad_args(
                        format!("unknown units `{other}`"),
                        "units: decimal binary",
                    )),
                },
                "temperature" => match TemperatureScale::from_id(&value_or(&value)) {
                    Some(scale) => {
                        self.settings.set_temperature_scale(scale);
                        self.settings.save();
                        Ok(serde_json::json!({"temperature": scale.id()}))
                    }
                    None => Err(VerbError::bad_args(
                        format!("unknown temperature scale `{}`", value_or(&value)),
                        "temperature: celsius fahrenheit both",
                    )),
                },
                "shot" => {
                    if self.pending_screenshot.is_some() {
                        Err(VerbError::unavailable(
                            "a screenshot is already in flight",
                            "wait for it, then retry",
                        ))
                    } else {
                        self.pending_screenshot = Some((reply, path));
                        ctx.send_viewport_cmd(ViewportCommand::Screenshot(
                            egui::UserData::default(),
                        ));
                        continue; // deferred reply after the render
                    }
                }
                other => Err(VerbError::bad_args(
                    format!("the GUI does not know `{other}`"),
                    "see `sysmon schema`",
                )),
            };
            let _ = reply.send(result);
        }
    }

    /// When the requested screenshot frame arrives, encode + reply.
    fn collect_screenshot(&mut self, ctx: &egui::Context) {
        if self.pending_screenshot.is_none() {
            return;
        }
        let image = ctx.input(|input| {
            input.raw.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = image else { return };
        let (reply, requested_path) = self.pending_screenshot.take().expect("checked above");

        let path = requested_path.unwrap_or_else(|| {
            let directory = crate::control::socket_directory().join("shots");
            let _ = std::fs::create_dir_all(&directory);
            directory
                .join(format!("shot-{}.png", std::process::id()))
                .to_string_lossy()
                .to_string()
        });
        let result = save_color_image_png(&image, &path)
            .map(|()| serde_json::json!({"path": path}))
            .map_err(|error| {
                VerbError::runtime(
                    format!("could not write the screenshot: {error}"),
                    "pass a writable path: `sysmon ctl shot /tmp/shot.png`",
                )
            });
        let _ = reply.send(result);
    }
}

/// The menu's account of the history, a line each: what it is doing,
/// what is on disk per tier, how fast it grows, and any warning.
/// (text, is it an alert).
pub fn history_status_lines(status: &sysmon_core::history::RecordingStatus) -> Vec<(String, bool)> {
    use sysmon_core::history::text::range_label;
    let size = |bytes: f64| sysmon_core::units::format_size(bytes.max(0.0).round() as u64, sysmon_core::units::Units::Decimal);
    let mut lines = Vec::new();
    if let Some(problem) = &status.problem {
        lines.push((problem.clone(), true));
    }
    if !status.recording {
        if status.problem.is_none() {
            lines.push(("not recording".to_string(), false));
        }
        return lines;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let back = status
        .oldest_ts
        .map(|oldest| format!(" · {} back", range_label(((now - oldest).max(0.0) / 60.0).round() * 60.0)))
        .unwrap_or_default();
    let packing = if status.packing { " · packing…" } else { "" };
    lines.push((format!("recording · {} on disk{back}{packing}", size(status.bytes as f64)), false));
    lines.push((
        if status.archive_days > 0 {
            format!(
                "hourly {} · archive {} in {} day{}",
                size(status.hot_bytes as f64),
                size(status.archive_bytes as f64),
                status.archive_days,
                if status.archive_days == 1 { "" } else { "s" }
            )
        } else {
            format!("hourly {} · nothing older than {} yet", size(status.hot_bytes as f64), range_label(status.hot_seconds))
        },
        false,
    ));
    if let Some(growth) = &status.growth {
        let estimate = if growth.basis == "archive" { "" } else { " (est.)" };
        lines.push((
            format!(
                "grows ~{} a month · ~{} a year{estimate}",
                size(growth.per_month_bytes),
                size(growth.per_year_bytes)
            ),
            false,
        ));
    }
    if let Some(warning) = &status.warning {
        lines.push((warning.clone(), true));
    }
    lines
}

/// The menu's Export… step: where to write, and the write in flight.
pub struct ExportDialog {
    pub path: String,
    pub running: Option<std::sync::mpsc::Receiver<Result<sysmon_core::history::ExportReport, String>>>,
    pub error: Option<String>,
}

impl ExportDialog {
    pub fn new(history_dir: &std::path::Path) -> ExportDialog {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| history_dir.to_path_buf());
        let path = home.join(sysmon_core::history::manage::default_export_name(now));
        ExportDialog { path: path.display().to_string(), running: None, error: None }
    }
}

/// True while X11 reports button 1 held anywhere on screen — the
/// signal that a pop-out is mid-drag. None off X11 (Wayland): the
/// dock-on-drop gesture degrades to the ⧉ toggle.
fn x11_button1_down() -> Option<bool> {
    use std::sync::OnceLock;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, KeyButMask};
    static CONNECTION: OnceLock<Option<(x11rb::rust_connection::RustConnection, u32)>> =
        OnceLock::new();
    let connection = CONNECTION
        .get_or_init(|| {
            x11rb::connect(None).ok().map(|(connection, screen)| {
                let root = connection.setup().roots[screen].root;
                (connection, root)
            })
        })
        .as_ref()?;
    let reply = connection
        .0
        .query_pointer(connection.1)
        .ok()?
        .reply()
        .ok()?;
    Some(reply.mask.contains(KeyButMask::BUTTON1))
}

fn save_color_image_png(image: &egui::ColorImage, path: &str) -> Result<(), String> {
    let [width, height] = image.size;
    let mut buffer = image::RgbaImage::new(width as u32, height as u32);
    for (index, pixel) in image.pixels.iter().enumerate() {
        let x = (index % width) as u32;
        let y = (index / width) as u32;
        buffer.put_pixel(x, y, image::Rgba(pixel.to_srgba_unmultiplied()));
    }
    buffer.save(path).map_err(|error| error.to_string())
}

impl eframe::App for SysMonApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.shared.quit_requested.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if std::env::var_os("SYSMON_DEBUG_FPS").is_some() {
            use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
            static FRAMES: AtomicU64 = AtomicU64::new(0);
            static WINDOW_START: Mutex<Option<Instant>> = Mutex::new(None);
            let count = FRAMES.fetch_add(1, AtomicOrdering::Relaxed) + 1;
            let mut start = WINDOW_START.lock().unwrap();
            let begin = start.get_or_insert_with(Instant::now);
            if begin.elapsed() > Duration::from_secs(10) {
                eprintln!("frames in 10s: {count}");
                FRAMES.store(0, AtomicOrdering::Relaxed);
                *start = Some(Instant::now());
            }
        }
        self.apply_theme(ctx);
        self.keyboard(ctx);
        self.process_commands(ctx);
        self.collect_screenshot(ctx);

        // Remember the window size for next launch, and the outer
        // rect for the pop-outs' drag-dock test.
        if let Some(rect) = ctx.input(|input| input.viewport().inner_rect) {
            self.settings.window_width = rect.width() as i32;
            self.settings.window_height = rect.height() as i32;
        }
        *self.shared.main_window_rect.lock().unwrap() =
            ctx.input(|input| input.viewport().outer_rect);

        let mut frame_actions: Vec<AppAction> = Vec::new();

        self.top_bar(ctx);

        let palette = self.palette();
        if self.page == Page::Overview {
            let text = {
                let snapshot = self.shared.latest.read().unwrap();
                // The central panel's width: the window's content rect
                // minus its 8 px side margins (R17). (The viewport's
                // inner_rect is a backend report, absent in some
                // backends and in the test harness.)
                let wide = ctx.content_rect().width() - 16.0 >= overview::WIDE_STATUS_WIDTH;
                overview::status_text(&snapshot, self.settings.display(), wide)
            };
            overview::status_bar(ctx, palette, &text);
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(palette.plane)
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| match self.page {
                Page::Overview => self.overview(ui, &mut frame_actions),
                Page::Processes => {
                    let snapshot = self.shared.latest.read().unwrap().clone();
                    processes_page(
                        ui,
                        palette,
                        &self.settings.graph_palette,
                        self.settings.display(),
                        &snapshot,
                        &mut self.table_state,
                        &mut self.icon_cache,
                        &mut frame_actions,
                        self.settings.show_inspector,
                    );
                    // Persist a changed sort choice (survives restarts).
                    let sort_id = self.table_state.sort_column.id();
                    if self.settings.sort_column != sort_id
                        || self.settings.sort_descending != self.table_state.sort_descending
                    {
                        self.settings.sort_column = sort_id.to_string();
                        self.settings.sort_descending = self.table_state.sort_descending;
                        self.settings.save();
                    }
                }
            });

        self.popout_viewports(ctx, &mut frame_actions);

        // Detail windows (and whether the sampler should pay for the
        // connection table).
        let snapshot = self.shared.latest.read().unwrap().clone();
        let units = self.settings.display().units;
        let mut closed: Vec<i32> = Vec::new();
        for pid in self.details_open.clone() {
            let palette = self.palette();
            let keep = details::details_window(
                ctx,
                palette,
                &self.settings.graph_palette,
                units,
                &snapshot,
                pid,
                self.details_threads.entry(pid).or_default(),
                &mut frame_actions,
            );
            if !keep {
                closed.push(pid);
            }
        }
        self.details_open.retain(|pid| !closed.contains(pid));
        let open = &self.details_open;
        self.details_threads.retain(|pid, _| open.contains(pid));
        // The socket table is only paid for while something shows it:
        // a details window, or the Inspector on the Processes page.
        let inspecting = self.page == Page::Processes
            && self.settings.show_inspector
            && self.table_state.inspector.pid.is_some();
        self.shared
            .connections_wanted
            .store(!self.details_open.is_empty() || inspecting, Ordering::Relaxed);
        let cpu_popped = self.settings.popped_out_sections.iter().any(|s| s == "cpu");
        let cpu_card_shown = self.settings.section_visible("cpu") && (cpu_popped || self.page == Page::Overview);
        self.shared.occupants_wanted.store(cpu_card_shown, Ordering::Relaxed);

        // The combined-details window (multi-select → Details).
        if let Some(pids) = self.combined_details_open.clone() {
            let keep = details::combined_details_window(
                ctx,
                self.palette(),
                &self.settings.graph_palette,
                units,
                &snapshot,
                &pids,
                &mut self.icon_cache,
                &mut frame_actions,
            );
            if !keep {
                self.combined_details_open = None;
            }
        }

        // Register every live child viewport for sampler pokes (kept
        // HERE, not in popout_viewports — that returns early with no
        // pop-outs, which would strand details/combined windows).
        {
            let mut open = self.shared.open_viewports.lock().unwrap();
            open.clear();
            for section in &self.settings.popped_out_sections {
                open.push(ViewportId::from_hash_of(("popout", section.as_str())));
            }
            for pid in &self.details_open {
                open.push(ViewportId::from_hash_of(("details", *pid)));
            }
            if let Some(pids) = &self.combined_details_open {
                let mut sorted = pids.clone();
                sorted.sort_unstable();
                open.push(ViewportId::from_hash_of(("combined", sorted)));
            }
        }

        self.apply_actions(ctx, frame_actions);
        self.confirm_and_toasts(ctx);

        // Belt-and-braces repaint: the sampler pokes us on data, this
        // covers pause/interval edge cases.
        let interval = *self.shared.interval_seconds.lock().unwrap();
        ctx.request_repaint_after(Duration::from_secs_f64(interval.clamp(0.5, 5.0)));
    }

    fn on_exit(&mut self) {
        self.settings.save();
    }
}

#[cfg(test)]
mod tests {
    use super::overview_slots;

    #[test]
    fn the_nvidia_card_rides_in_the_amd_cards_slot() {
        let all = ["gpu", "nvidia", "memory", "cpu", "network", "disks", "sensors"];
        assert_eq!(
            overview_slots(&all),
            vec![vec!["gpu", "nvidia"], vec!["memory"], vec!["cpu"], vec!["network"], vec!["disks"], vec!["sensors"]],
            "six slots: the columns alternate exactly as they did before the card existed"
        );
        let without = ["gpu", "memory", "cpu", "network", "disks", "sensors"];
        assert_eq!(overview_slots(&without).len(), 6, "no NVIDIA card: one slot per card, as in 3.6");
        // The AMD card hidden or popped out: the NVIDIA card takes the
        // slot alone and nothing is lost.
        assert_eq!(overview_slots(&["nvidia", "memory"]), vec![vec!["nvidia"], vec!["memory"]]);
    }

    #[test]
    fn a_popout_fits_its_card_until_the_user_resizes_it() {
        use super::PopoutFit;
        use egui::vec2;
        let mut fit = PopoutFit::default();
        // First frame: ask for the card's size.
        assert_eq!(fit.next_request(Some(vec2(440.0, 400.0)), vec2(451.0, 392.0)), Some(vec2(451.0, 392.0)));
        // Reached: settled, nothing more to ask.
        assert_eq!(fit.next_request(Some(vec2(451.0, 392.0)), vec2(451.0, 392.0)), None);
        assert!(fit.settled);
        // A digit less in a value doesn't shrink the width; a new row grows the height.
        assert_eq!(fit.next_request(Some(vec2(451.0, 392.0)), vec2(445.0, 414.0)), Some(vec2(451.0, 414.0)));
        assert_eq!(fit.next_request(Some(vec2(451.0, 414.0)), vec2(445.0, 414.0)), None);
        // The user drags it bigger: their size from now on.
        assert_eq!(fit.next_request(Some(vec2(600.0, 600.0)), vec2(451.0, 300.0)), None);
        assert!(fit.user_sized);
    }
}
