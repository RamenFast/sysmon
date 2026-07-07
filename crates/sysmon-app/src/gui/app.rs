// SPDX-License-Identifier: GPL-3.0-or-later
//! The application shell: sampler thread → shared state → main
//! window + pop-out/detail viewports. State the viewports read is
//! Arc'd (deferred viewports render on their own repaints, so a
//! minimized main window never freezes a pop-out — v1's law), and
//! the same shared state serves the control socket in wave 7.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use egui::{Align, Layout, RichText, ViewportBuilder, ViewportCommand, ViewportId};

use sysmon_core::collect::Sampler;
use sysmon_core::snapshot::{SystemSnapshot, Wants};
use sysmon_core::units::Units;

use super::actions::{self, ConfirmKind, PendingConfirm};
use super::cards::{self, AppAction, CardContext, Glyph, glyph_button};
use super::details;
use super::graphs::History;
use super::icons::IconCache;
use super::processes::{ProcessTableState, processes_page};
use super::settings::{SECTION_KEYS, Settings};
use super::theme::{self, Palette};

#[derive(Clone, Copy, PartialEq)]
pub enum Page {
    Overview,
    Processes,
}

#[derive(Default)]
pub struct Histories {
    pub gpu: History,
    pub memory: History,
    pub cpu: History,
    pub net_down: History,
    pub net_up: History,
}

/// Everything the render paths (main window, pop-outs, socket
/// backend) share with the sampler.
pub struct SharedUi {
    pub latest: RwLock<Arc<SystemSnapshot>>,
    pub histories: RwLock<Histories>,
    pub paused: AtomicBool,
    pub interval_seconds: Mutex<f64>,
    pub connections_wanted: AtomicBool,
    /// Pop-out sections + detail windows request repaints by id.
    pub open_viewports: Mutex<Vec<ViewportId>>,
}

impl SharedUi {
    fn new(interval: f64) -> Self {
        SharedUi {
            latest: RwLock::new(Arc::new(SystemSnapshot::default())),
            histories: RwLock::new(Histories::default()),
            paused: AtomicBool::new(false),
            interval_seconds: Mutex::new(interval),
            connections_wanted: AtomicBool::new(false),
            open_viewports: Mutex::new(Vec::new()),
        }
    }
}

/// The sampler thread: samples at the user cadence, publishes into
/// SharedUi, pokes every live viewport.
fn spawn_sampler(ctx: egui::Context, shared: Arc<SharedUi>) {
    std::thread::Builder::new()
        .name("sysmon-sampler".to_string())
        .spawn(move || {
            let mut sampler = Sampler::new();
            loop {
                let started = Instant::now();
                if !shared.paused.load(Ordering::Relaxed) {
                    let mut wants = Wants::all();
                    wants.system = true;
                    wants.connections = shared.connections_wanted.load(Ordering::Relaxed);
                    let snapshot = Arc::new(sampler.sample(wants));

                    {
                        let mut histories = shared.histories.write().unwrap();
                        if let Some(gpu) = &snapshot.gpu
                            && gpu.available
                        {
                            histories.gpu.push(gpu.busy_percent as f64);
                        }
                        if let Some(memory) = &snapshot.memory {
                            histories.memory.push(memory.used_percent as f64);
                        }
                        if let Some(cpu) = &snapshot.cpu {
                            histories.cpu.push(cpu.overall_percent as f64);
                        }
                        if let Some(network) = &snapshot.network {
                            histories.net_down.push(network.download_bps);
                            histories.net_up.push(network.upload_bps);
                        }
                    }
                    *shared.latest.write().unwrap() = snapshot;

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
    page: Page,
    table_state: ProcessTableState,
    details_open: Vec<i32>,
    pending_confirm: Option<PendingConfirm>,
    toast_tx: std::sync::mpsc::Sender<String>,
    toast_rx: std::sync::mpsc::Receiver<String>,
    toasts: Vec<actions::Toast>,
    applied_palette_id: String,
    system_palette: &'static Palette,
    system_palette_checked: Instant,
    /// Pin state per pop-out viewport (pinned-on-top by default).
    popout_pins: HashMap<&'static str, bool>,
}

impl SysMonApp {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings) -> Self {
        let shared = Arc::new(SharedUi::new(settings.update_interval_seconds));
        spawn_sampler(cc.egui_ctx.clone(), shared.clone());
        let (toast_tx, toast_rx) = std::sync::mpsc::channel();

        SysMonApp {
            shared,
            icon_cache: IconCache::new(),
            page: Page::Overview,
            table_state: ProcessTableState::default(),
            details_open: Vec::new(),
            pending_confirm: None,
            toast_tx,
            toast_rx,
            toasts: Vec::new(),
            applied_palette_id: String::new(),
            system_palette: theme::palette_for_system(),
            system_palette_checked: Instant::now(),
            popout_pins: HashMap::new(),
            settings,
        }
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
                    let pause_glyph = if paused { Glyph::Play } else { Glyph::Pause };
                    let pause_tooltip = if paused { "Resume updates" } else { "Pause updates" };
                    if glyph_button(ui, palette, pause_glyph, paused, pause_tooltip).clicked() {
                        self.shared.paused.store(!paused, Ordering::Relaxed);
                    }
                    if self.settings.show_pin_button
                        && glyph_button(
                            ui,
                            palette,
                            Glyph::Pin,
                            self.settings.always_on_top,
                            "Keep window on top",
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

                    // Page switcher, centered-ish.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let menu_response = glyph_button(
                            ui,
                            palette,
                            Glyph::Menu,
                            false,
                            "Display options",
                        );
                        self.settings_menu(ui, &menu_response);

                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add_space((ui.available_width() / 2.0 - 90.0).max(0.0));
                            for (label, page) in
                                [("Overview", Page::Overview), ("Processes", Page::Processes)]
                            {
                                let selected = self.page == page;
                                let text = RichText::new(label).size(12.5).color(if selected {
                                    palette.ink
                                } else {
                                    palette.muted
                                });
                                let response = ui.selectable_label(selected, text);
                                if response.clicked() {
                                    self.page = page;
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
                ui.set_min_width(210.0);
                let palette = theme::palette_by_id(&self.applied_palette_id)
                    .unwrap_or(&theme::PALETTES[0]);
                let heading = |ui: &mut egui::Ui, text: &str| {
                    ui.label(RichText::new(text).color(palette.muted).size(10.5));
                };

                heading(ui, "Appearance");
                let before = self.settings.theme_mode.clone();
                ui.radio_value(
                    &mut self.settings.theme_mode,
                    "system".to_string(),
                    "Follow system theme",
                );
                for candidate in theme::PALETTES.iter() {
                    ui.radio_value(
                        &mut self.settings.theme_mode,
                        candidate.id.to_string(),
                        candidate.label,
                    );
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
                        if ui
                            .radio(
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
                heading(ui, "Units");
                ui.horizontal(|ui| {
                    if ui
                        .radio(!self.settings.use_binary_units, "Decimal (GB)")
                        .on_hover_text("what drive stickers and ISPs quote")
                        .clicked()
                    {
                        self.settings.use_binary_units = false;
                        settings_changed = true;
                    }
                    if ui
                        .radio(self.settings.use_binary_units, "Binary (GiB)")
                        .on_hover_text("what htop and GNOME System Monitor show")
                        .clicked()
                    {
                        self.settings.use_binary_units = true;
                        settings_changed = true;
                    }
                });

                ui.separator();
                heading(ui, "Update interval");
                ui.horizontal(|ui| {
                    for interval in [1.0f64, 2.0, 3.0, 5.0] {
                        let selected =
                            (self.settings.update_interval_seconds - interval).abs() < 0.01;
                        if ui.radio(selected, format!("{interval:.0}s")).clicked() {
                            self.settings.update_interval_seconds = interval;
                            *self.shared.interval_seconds.lock().unwrap() = interval;
                            settings_changed = true;
                        }
                    }
                });

                ui.separator();
                heading(ui, "Window");
                if ui
                    .checkbox(&mut self.settings.always_on_top, "Always on top")
                    .changed()
                {
                    ui.ctx().send_viewport_cmd(ViewportCommand::WindowLevel(
                        if self.settings.always_on_top {
                            egui::WindowLevel::AlwaysOnTop
                        } else {
                            egui::WindowLevel::Normal
                        },
                    ));
                    settings_changed = true;
                }
                if ui
                    .checkbox(&mut self.settings.show_pin_button, "Show pin button")
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(&mut self.settings.compact_mode, "Compact mode")
                    .on_hover_text("shrink graphs and hide detail rows — for a screen corner")
                    .changed()
                {
                    settings_changed = true;
                }

                ui.separator();
                heading(ui, "Overview sections");
                for key in SECTION_KEYS {
                    let mut visible = self.settings.section_visible(key);
                    let label = match key {
                        "gpu" => "GPU",
                        "memory" => "Memory",
                        "cpu" => "CPU",
                        "network" => "Network",
                        "disks" => "Disks",
                        "sensors" => "Sensors",
                        _ => key,
                    };
                    if ui.checkbox(&mut visible, label).changed() {
                        self.settings.visible_sections.insert(key.to_string(), visible);
                        if !visible {
                            self.settings.popped_out_sections.retain(|s| s != key);
                        }
                        settings_changed = true;
                    }
                }
            });
        if settings_changed {
            self.settings.save();
        }
    }

    fn overview(&mut self, ui: &mut egui::Ui, actions_out: &mut Vec<AppAction>) {
        let palette = self.palette();
        let snapshot = self.shared.latest.read().unwrap().clone();
        let histories = self.shared.histories.read().unwrap();
        let units = if self.settings.use_binary_units {
            Units::Binary
        } else {
            Units::Decimal
        };

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                let mut cx = CardContext {
                    palette,
                    graph_palette_id: &self.settings.graph_palette,
                    units,
                    compact: self.settings.compact_mode,
                    snapshot: &snapshot,
                    icon_cache: &mut self.icon_cache,
                    actions: actions_out,
                    popped_out: &self.settings.popped_out_sections,
                };
                let popped = |cx: &CardContext, key: &str| {
                    cx.popped_out.iter().any(|s| s == key)
                };
                if self.settings.section_visible("gpu") && !popped(&cx, "gpu") {
                    cards::gpu_card(ui, &mut cx, &histories.gpu);
                    ui.add_space(6.0);
                }
                if self.settings.section_visible("memory") && !popped(&cx, "memory") {
                    cards::memory_card(ui, &mut cx, &histories.memory);
                    ui.add_space(6.0);
                }
                if self.settings.section_visible("cpu") && !popped(&cx, "cpu") {
                    cards::cpu_card(ui, &mut cx, &histories.cpu);
                    ui.add_space(6.0);
                }
                if self.settings.section_visible("network") && !popped(&cx, "network") {
                    cards::network_card(ui, &mut cx, &histories.net_down, &histories.net_up);
                    ui.add_space(6.0);
                }
                if self.settings.section_visible("disks") && !popped(&cx, "disks") {
                    cards::disks_card(ui, &mut cx);
                    ui.add_space(6.0);
                }
                if self.settings.section_visible("sensors") && !popped(&cx, "sensors") {
                    cards::sensors_card(ui, &mut cx);
                    ui.add_space(6.0);
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
        if popped.is_empty() {
            return;
        }
        let snapshot = self.shared.latest.read().unwrap().clone();
        let units = if self.settings.use_binary_units {
            Units::Binary
        } else {
            Units::Decimal
        };

        for section in popped {
            let viewport_id = ViewportId::from_hash_of(("popout", section));
            let title = match section {
                "gpu" => "GPU — SysMon",
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
            let histories = self.shared.histories.read().unwrap();
            let builder = ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([380.0, 320.0])
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
                        ui.horizontal(|ui| {
                            let pin_response = glyph_button(
                                ui,
                                palette,
                                Glyph::Pin,
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
                            ui.label(
                                RichText::new("drag onto the main window to dock")
                                    .color(palette.muted)
                                    .size(10.0),
                            );
                        });
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let mut cx = CardContext {
                                    palette,
                                    graph_palette_id: &self.settings.graph_palette,
                                    units,
                                    compact: false,
                                    snapshot: &snapshot,
                                    icon_cache: &mut self.icon_cache,
                                    actions: actions_out,
                                    popped_out: &self.settings.popped_out_sections,
                                };
                                match section {
                                    "gpu" => cards::gpu_card(ui, &mut cx, &histories.gpu),
                                    "memory" => {
                                        cards::memory_card(ui, &mut cx, &histories.memory)
                                    }
                                    "cpu" => cards::cpu_card(ui, &mut cx, &histories.cpu),
                                    "network" => cards::network_card(
                                        ui,
                                        &mut cx,
                                        &histories.net_down,
                                        &histories.net_up,
                                    ),
                                    "disks" => cards::disks_card(ui, &mut cx),
                                    "sensors" => cards::sensors_card(ui, &mut cx),
                                    _ => {}
                                }
                            });
                    });
                if ctx.input(|input| input.viewport().close_requested()) {
                    close_requested = true;
                }
            });

            if close_requested {
                self.settings.popped_out_sections.retain(|s| s != section);
                self.settings.save();
            }
        }
        // Keep the sampler poking these viewports.
        let mut open = self.shared.open_viewports.lock().unwrap();
        open.clear();
        for section in &self.settings.popped_out_sections {
            open.push(ViewportId::from_hash_of(("popout", section.as_str())));
        }
        for pid in &self.details_open {
            open.push(ViewportId::from_hash_of(("details", *pid)));
        }
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
            }
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

    fn keyboard(&mut self, ctx: &egui::Context) {
        ctx.input_mut(|input| {
            use egui::{Key, KeyboardShortcut, Modifiers};
            if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::F)) {
                self.page = Page::Processes;
                self.table_state.focus_filter = true;
            }
            if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Num1)) {
                self.page = Page::Overview;
            }
            if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Num2)) {
                self.page = Page::Processes;
            }
            if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Q)) {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        });
    }
}

impl eframe::App for SysMonApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
        self.keyboard(ctx);

        // Remember the window size for next launch.
        if let Some(rect) = ctx.input(|input| input.viewport().inner_rect) {
            self.settings.window_width = rect.width() as i32;
            self.settings.window_height = rect.height() as i32;
        }

        let mut frame_actions: Vec<AppAction> = Vec::new();

        self.top_bar(ctx);

        let palette = self.palette();
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
                    let units = if self.settings.use_binary_units {
                        Units::Binary
                    } else {
                        Units::Decimal
                    };
                    processes_page(
                        ui,
                        palette,
                        units,
                        &snapshot,
                        &mut self.table_state,
                        &mut self.icon_cache,
                        &mut frame_actions,
                    );
                }
            });

        self.popout_viewports(ctx, &mut frame_actions);

        // Detail windows (and whether the sampler should pay for the
        // connection table).
        let snapshot = self.shared.latest.read().unwrap().clone();
        let units = if self.settings.use_binary_units {
            Units::Binary
        } else {
            Units::Decimal
        };
        let mut closed: Vec<i32> = Vec::new();
        for pid in self.details_open.clone() {
            let keep = details::details_window(
                ctx,
                self.palette(),
                units,
                &snapshot,
                pid,
                &mut frame_actions,
            );
            if !keep {
                closed.push(pid);
            }
        }
        self.details_open.retain(|pid| !closed.contains(pid));
        self.shared
            .connections_wanted
            .store(!self.details_open.is_empty(), Ordering::Relaxed);

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
