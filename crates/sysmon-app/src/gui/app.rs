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
use sysmon_core::units::Units;

use super::actions::{self, ConfirmKind, PendingConfirm};
use super::backend::GuiCommand;
use super::cards::{
    self, AppAction, CardContext, Glyph, glyph_button, menu_check_row, menu_chip, menu_option_row,
};
use super::details;
use super::graphs::History;
use super::icons::IconCache;
use super::processes::{ProcessTableState, SortColumn, matches_filter, processes_page};
use super::settings::{SECTION_KEYS, Settings};
use super::theme::{self, Palette};

#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// The main window's outer rect (screen coords) — the drag-dock
    /// target the pop-outs test against.
    pub main_window_rect: Mutex<Option<egui::Rect>>,
    /// The adapter the window renders on, set once at startup.
    /// `degraded` = a CPU rasterizer (llvmpipe): the app still runs,
    /// but slower and hungrier than it should be — `status` and the
    /// startup toast say so out loud (never silently slow).
    pub renderer: OnceLock<RendererInfo>,
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
            open_viewports: Mutex::new(Vec::new()),
            main_window_rect: Mutex::new(None),
            renderer: OnceLock::new(),
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
    pub page: Page,
    pub table_state: ProcessTableState,
    details_open: Vec<i32>,
    /// The one combined-details window (multi-select → Details);
    /// opening a new selection replaces it.
    combined_details_open: Option<Vec<i32>>,
    pending_confirm: Option<PendingConfirm>,
    toast_tx: std::sync::mpsc::Sender<String>,
    toast_rx: std::sync::mpsc::Receiver<String>,
    toasts: Vec<actions::Toast>,
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
    /// Keeps the socket alive exactly as long as the app; Drop
    /// unlinks it.
    _control_server: Option<crate::control::ControlServer>,
}

/// The `shot` verb's deferred reply: where to answer, where to write.
type PendingScreenshot = (
    std::sync::mpsc::Sender<Result<serde_json::Value, crate::control::VerbError>>,
    Option<String>,
);

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

        SysMonApp {
            shared,
            icon_cache: IconCache::new(),
            page: Page::Overview,
            table_state,
            details_open: Vec::new(),
            combined_details_open: None,
            pending_confirm: None,
            toast_tx,
            toast_rx,
            toasts: Vec::new(),
            applied_palette_id: String::new(),
            system_palette: theme::palette_for_system(),
            system_palette_checked: Instant::now(),
            popout_pins: HashMap::new(),
            command_rx,
            pending_screenshot: None,
            popout_drags: HashMap::new(),
            _control_server: control_server,
            settings,
        }
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
                heading(ui, "Units");
                ui.horizontal(|ui| {
                    if menu_chip(ui, palette, !self.settings.use_binary_units, "Decimal (GB)")
                        .on_hover_text("what drive stickers and ISPs quote")
                        .clicked()
                    {
                        self.settings.use_binary_units = false;
                        settings_changed = true;
                    }
                    if menu_chip(ui, palette, self.settings.use_binary_units, "Binary (GiB)")
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
                heading(ui, "Overview sections");
                for key in SECTION_KEYS {
                    let visible = self.settings.section_visible(key);
                    let label = match key {
                        "gpu" => "GPU",
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
                    selected: &mut self.table_state.selected_pids,
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
                                    selected: &mut self.table_state.selected_pids,
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
                    // A live filter that hides the target would make
                    // the jump land on nothing — clear it, disclosed.
                    let filter = self.table_state.filter.to_lowercase();
                    let hidden = {
                        let snapshot = self.shared.latest.read().unwrap();
                        snapshot.processes.as_ref().is_some_and(|records| {
                            records
                                .iter()
                                .find(|r| r.pid == pid)
                                .is_some_and(|r| !matches_filter(r, &filter))
                        })
                    };
                    if hidden {
                        self.table_state.filter.clear();
                        let _ = self.toast_tx.send("Filter cleared to reveal the process".to_string());
                    }
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
            if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Num1)) {
                self.page = Page::Overview;
            }
            if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Num2)) {
                self.page = Page::Processes;
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
                "page" => match value_or(&value).as_str() {
                    "overview" => {
                        self.page = Page::Overview;
                        Ok(serde_json::json!({"page": "overview"}))
                    }
                    "processes" => {
                        self.page = Page::Processes;
                        Ok(serde_json::json!({"page": "processes"}))
                    }
                    other => Err((
                        format!("unknown page `{other}`"),
                        "pages: overview processes".to_string(),
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
                        Err((
                            format!("unknown theme `{wanted}`"),
                            "themes: system blossom_dark blossom amoled light dark funky paper \
                             basalt amber chromacore"
                                .to_string(),
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
                        Err((
                            format!("unknown graph palette `{wanted}`"),
                            "palettes: mint aqua sunset forest mono blossom funky amber terminal"
                                .to_string(),
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
                        None => Err((
                            format!("unknown section `{wanted}`"),
                            "sections: gpu memory cpu network disks sensors".to_string(),
                        )),
                    }
                }
                "compact" => {
                    let on = matches!(value_or(&value).as_str(), "on" | "true" | "1");
                    self.settings.compact_mode = on;
                    self.settings.save();
                    Ok(serde_json::json!({"compact": on}))
                }
                "units" => {
                    let binary = matches!(value_or(&value).as_str(), "binary" | "gib");
                    self.settings.use_binary_units = binary;
                    self.settings.save();
                    Ok(serde_json::json!({
                        "units": if binary { "binary" } else { "decimal" },
                    }))
                }
                "shot" => {
                    if self.pending_screenshot.is_some() {
                        Err((
                            "a screenshot is already in flight".to_string(),
                            "wait for it, then retry".to_string(),
                        ))
                    } else {
                        self.pending_screenshot = Some((reply, path));
                        ctx.send_viewport_cmd(ViewportCommand::Screenshot(
                            egui::UserData::default(),
                        ));
                        continue; // deferred reply after the render
                    }
                }
                other => Err((
                    format!("the GUI does not know `{other}`"),
                    "see `sysmon schema`".to_string(),
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
                (
                    format!("could not write the screenshot: {error}"),
                    "pass a writable path: `sysmon ctl shot /tmp/shot.png`".to_string(),
                )
            });
        let _ = reply.send(result);
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
                        &self.settings.graph_palette,
                        units,
                        &snapshot,
                        &mut self.table_state,
                        &mut self.icon_cache,
                        &mut frame_actions,
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
