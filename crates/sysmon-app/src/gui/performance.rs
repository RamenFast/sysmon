// SPDX-License-Identifier: GPL-3.0-or-later
//! The Performance page, compiled from docs/dev/PERFORMANCE-VIEW.md.
//! The XP Task Manager's Performance tab in SysMon's own language: an
//! LED meter beside a scope graph for each resource, etched group
//! boxes of totals, a status bar. Everything on one screen.
//!
//! Every number here is a snapshot field the audit checks; the meter
//! and the graph's newest point are the same field in the same frame
//! (V1). Nothing on this page warns about temperature: the Thermals
//! graph puts it next to the busy clock and lets the data speak.

use egui::{Align, Color32, Layout, RichText, Ui, vec2};

use sysmon_core::snapshot::SystemSnapshot;
use sysmon_core::units::{format_frequency_mhz, format_percent, format_rate, format_size, format_temperature_in};

use super::app::Histories;
use super::cards::AppAction;
use super::graphs::{self, History, ScopeConfig};
use super::processes::SortColumn;
use super::settings::{Display, Settings};
use super::theme::{
    GRAPH_SERIES_CPU, GRAPH_SERIES_GPU, GRAPH_SERIES_MEMORY, GRAPH_SERIES_NET_DOWN, GRAPH_SERIES_NET_UP, Palette,
    graph_color,
};
use super::widgets::stone_switch;

/// Two columns from here (PERFORMANCE-VIEW.md "Wide").
pub const WIDE_BREAKPOINT: f32 = 760.0;
/// `auto` CPU graph mode: per thread when the column is this wide.
pub const PER_THREAD_AUTO_WIDTH: f32 = 420.0;
pub const MINI_GRAPH_MIN_HEIGHT: f32 = 28.0;
/// Scope height by layout: wide windows have the room, the 430 px
/// default must fit six rows and four boxes on one 780 px screen.
const SCOPE_HEIGHT_WIDE: f32 = 64.0;
const SCOPE_HEIGHT_NARROW: f32 = 42.0;
const ROW_GAP_WIDE: f32 = 6.0;
const ROW_GAP_NARROW: f32 = 2.0;

/// Which CPU graph a given setting + width resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuGraphMode {
    Combined,
    PerThread,
}

impl CpuGraphMode {
    pub fn resolve(setting: &str, column_width: f32) -> CpuGraphMode {
        match setting {
            "combined" => CpuGraphMode::Combined,
            "per_thread" => CpuGraphMode::PerThread,
            _ if column_width >= PER_THREAD_AUTO_WIDTH => CpuGraphMode::PerThread,
            _ => CpuGraphMode::Combined,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            CpuGraphMode::Combined => "combined",
            CpuGraphMode::PerThread => "per_thread",
        }
    }
}

/// Columns of the per-thread grid for a width (V9): never fewer than
/// 2, never more than 8, each at least ~96 px.
pub fn per_thread_columns(width: f32) -> usize {
    ((width / 96.0).floor() as usize).clamp(2, 8)
}

/// Everything the page reads for one frame.
pub struct PerformanceContext<'a> {
    pub palette: &'a Palette,
    pub settings: &'a Settings,
    pub display: Display,
    pub snapshot: &'a SystemSnapshot,
    pub histories: &'a Histories,
    pub actions: &'a mut Vec<AppAction>,
    /// Set by the page from the panel width each frame.
    pub wide: bool,
    /// Written by the page: the width the CPU graph mode resolved
    /// against this frame (the menu's "decide by width" reads it).
    pub cpu_column_width: f32,
}

impl PerformanceContext<'_> {
    fn scope_height(&self) -> f32 {
        if self.wide { SCOPE_HEIGHT_WIDE } else { SCOPE_HEIGHT_NARROW }
    }

    fn row_gap(&self) -> f32 {
        if self.wide { ROW_GAP_WIDE } else { ROW_GAP_NARROW }
    }

    fn series(&self, index: usize) -> Color32 {
        graph_color(&self.settings.graph_palette, index, self.palette.dark)
    }

    /// The quieter sibling of a series colour (kernel time, VRAM):
    /// mixed toward the ink that reads ON THE FIELD. On light themes
    /// the page ink is the field colour itself, so mixing toward it
    /// sank the second trace to 2:1 (reviewer R13).
    fn sibling(&self, index: usize) -> Color32 {
        self.series(index).lerp_to_gamma(self.palette.on_field(), 0.45)
    }

    /// Thermal traces: the theme's two signature text colours on dark
    /// themes; on light themes those are dark on a dark field, so the
    /// two warmest series colours stand in (R13).
    fn thermal_colors(&self) -> (Color32, Color32) {
        if self.palette.dark {
            (self.palette.value, self.palette.title)
        } else {
            (self.series(GRAPH_SERIES_MEMORY), self.series(GRAPH_SERIES_GPU))
        }
    }

    fn dashed_second(&self) -> bool {
        self.palette.id == "greyscale"
    }

    fn size(&self, bytes: u64) -> String {
        format_size(bytes, self.display.units)
    }

    fn rate(&self, bps: f64) -> String {
        format_rate(bps, self.display.units)
    }

    fn temperature(&self, celsius: f32) -> String {
        format_temperature_in(celsius, self.display.temperature)
    }
}

// ------------------------------------------------------------------ page

pub fn performance_page(ui: &mut Ui, cx: &mut PerformanceContext) {
    let wide = ui.available_width() >= WIDE_BREAKPOINT;
    cx.wide = wide;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(2.0);
        if wide {
            let gap = 8.0;
            let column = (ui.available_width() - gap) / 2.0;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.allocate_ui_with_layout(vec2(column, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(column);
                    cpu_row(ui, cx);
                    memory_row(ui, cx);
                    gpu_row(ui, cx);
                });
                ui.allocate_ui_with_layout(vec2(column, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(column);
                    disk_row(ui, cx);
                    network_row(ui, cx);
                    thermals_row(ui, cx);
                });
            });
            ui.add_space(4.0);
            let box_width = (ui.available_width() - gap * 3.0) / 4.0;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for draw in [totals_box, physical_box, commit_box, kernel_box] {
                    ui.allocate_ui_with_layout(vec2(box_width, 0.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(box_width);
                        draw(ui, cx);
                    });
                }
            });
        } else {
            cpu_row(ui, cx);
            memory_row(ui, cx);
            gpu_row(ui, cx);
            disk_row(ui, cx);
            network_row(ui, cx);
            thermals_row(ui, cx);
            ui.add_space(2.0);
            two_boxes(ui, cx, totals_box, physical_box);
            two_boxes(ui, cx, commit_box, kernel_box);
        }
        ui.add_space(4.0);
    });
}

fn two_boxes(ui: &mut Ui, cx: &mut PerformanceContext, left: BoxFn, right: BoxFn) {
    let gap = 8.0;
    let width = (ui.available_width() - gap) / 2.0;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(width);
            left(ui, cx);
        });
        ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(width);
            right(ui, cx);
        });
    });
}

type BoxFn = fn(&mut Ui, &mut PerformanceContext);

// ------------------------------------------------------------ resource rows

/// A legend swatch + word, in the box title row.
fn legend(ui: &mut Ui, entries: &[(Color32, &str)]) {
    for (color, word) in entries {
        let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, *color);
        ui.label(RichText::new(*word).size(10.0).monospace().color(ui.visuals().weak_text_color()));
    }
}

/// One resource: a group box holding a meter beside a scope graph.
/// `meter_target` is the Overview card a meter click opens; `graph_target`
/// the Processes sort a graph click opens.
#[allow(clippy::too_many_arguments)]
fn resource_row(
    ui: &mut Ui,
    cx: &mut PerformanceContext,
    title: &str,
    legend_entries: &[(Color32, &str)],
    meter: (f32, Color32, String),
    meter_target: &'static str,
    graph_target: Option<SortColumn>,
    draw_graph: impl FnOnce(&mut Ui, &mut PerformanceContext, f32) -> egui::Response,
    title_extra: impl FnOnce(&mut Ui, &mut PerformanceContext),
) {
    let palette = cx.palette;
    let scope_height = cx.scope_height();
    let row_gap = cx.row_gap();
    graphs::group_box(ui, palette, title, |ui| {
        ui.horizontal(|ui| {
            legend(ui, legend_entries);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| title_extra(ui, cx));
        });
        ui.add_space(2.0);
        ui.horizontal_top(|ui| {
            let (fraction, color, label) = meter;
            let meter_response = graphs::led_meter(ui, palette, title, fraction, color, &label, scope_height)
                .on_hover_text(format!("Open the {title} card on the Overview"))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if meter_response.clicked() {
                cx.actions.push(AppAction::ShowOverview(meter_target));
            }
            ui.add_space(6.0);
            ui.vertical(|ui| {
                let graph_response = draw_graph(ui, cx, scope_height);
                if let Some(column) = graph_target {
                    let graph_response = graph_response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(format!("Click: Processes sorted by {}", column.id()));
                    if graph_response.clicked() {
                        cx.actions.push(AppAction::ShowProcessesBy(column));
                    }
                }
            });
        });
    });
    ui.add_space(row_gap);
}

fn percent_formatter(labels: &'static [&'static str]) -> impl Fn(&[f64]) -> String {
    move |values: &[f64]| {
        values
            .iter()
            .zip(labels)
            .map(|(value, label)| format!("{label} {value:.0}%"))
            .collect::<Vec<_>>()
            .join("  ")
    }
}

/// Before the first sample a row keeps its place: the same box, an
/// empty field, so the page does not jump when the data lands (R23).
fn placeholder_row(ui: &mut Ui, cx: &mut PerformanceContext, title: &str) {
    let palette = cx.palette;
    let height = cx.scope_height();
    let gap = cx.row_gap();
    graphs::group_box(ui, palette, title, |ui| {
        // Same height as a legend row, so the rows line up across columns.
        ui.horizontal(|ui| {
            legend(ui, &[(palette.muted, "waiting for the first sample")]);
        });
        ui.add_space(2.0);
        ui.horizontal_top(|ui| {
            graphs::led_meter(ui, palette, title, 0.0, palette.muted, "–", height);
            ui.add_space(6.0);
            let empty = History::default();
            graphs::scope_graph(
                ui,
                palette,
                &[&empty],
                &ScopeConfig {
                    name: title,
                    height,
                    fixed_maximum: Some(100.0),
                    minimum_autoscale: 100.0,
                    colors: &[palette.muted],
                    dashed_second: false,
                    ceiling_formatter: &|v| format!("{v:.0}"),
                    hover_formatter: &|_| String::new(),
                    tag: None,
                    mini: false,
                    fill_all: false,
                },
            );
        });
    });
    ui.add_space(gap);
}

fn cpu_row(ui: &mut Ui, cx: &mut PerformanceContext) {
    let Some(cpu) = cx.snapshot.cpu.as_ref() else {
        placeholder_row(ui, cx, "CPU");
        return;
    };
    let busy_color = cx.series(GRAPH_SERIES_CPU);
    let kernel_color = cx.sibling(GRAPH_SERIES_CPU);
    let percent = cpu.overall_percent;
    let column_width = ui.available_width();
    cx.cpu_column_width = column_width;
    let mode = CpuGraphMode::resolve(&cx.settings.cpu_graph_mode, column_width);
    let per_core = &cx.histories.per_core;
    let kernel = &cx.histories.cpu_kernel;
    let busy = &cx.histories.cpu;
    let core_count = cpu.per_core_percent.len();
    resource_row(
        ui,
        cx,
        "CPU",
        &[(busy_color, "busy"), (kernel_color, "kernel")],
        (percent / 100.0, busy_color, format_percent(percent)),
        "cpu",
        Some(SortColumn::Cpu),
        |ui, cx, height| match mode {
            CpuGraphMode::Combined => graphs::scope_graph(
                ui,
                cx.palette,
                &[busy, kernel],
                &ScopeConfig {
                    name: "CPU history",
                    height,
                    fixed_maximum: Some(100.0),
                    minimum_autoscale: 100.0,
                    colors: &[busy_color, kernel_color],
                    dashed_second: cx.dashed_second(),
                    ceiling_formatter: &|v| format!("{v:.0}%"),
                    hover_formatter: &percent_formatter(&["busy", "kernel"]),
                    tag: None,
                    mini: false,
                    fill_all: false,
                },
            ),
            CpuGraphMode::PerThread => per_thread_grid(ui, cx, per_core, core_count, busy_color),
        },
        |ui, cx| {
            let selected = match mode {
                CpuGraphMode::Combined => 0,
                CpuGraphMode::PerThread => 1,
            };
            if let Some(chosen) = stone_switch(
                ui,
                cx.palette,
                ["combined", "per thread"],
                selected,
                "CPU graph: one trace for the whole CPU, or one small graph per thread",
            ) {
                cx.actions.push(AppAction::SetCpuGraphMode(if chosen == 0 { "combined" } else { "per_thread" }));
            }
        },
    );
}

/// The per-thread grid: columns follow the width (V9), /proc/stat
/// order row-major (V3), each mini graph tagged with its number.
fn per_thread_grid(
    ui: &mut Ui,
    cx: &mut PerformanceContext,
    per_core: &[History],
    core_count: usize,
    color: Color32,
) -> egui::Response {
    let width = ui.available_width();
    let columns = per_thread_columns(width);
    let rows = core_count.div_ceil(columns).max(1);
    let gap = 2.0;
    let cell_width = (width - gap * (columns as f32 - 1.0)) / columns as f32;
    // Keep the whole grid near the height of a single scope graph
    // (two rows of it at most), never squashing a cell below the floor.
    let budget = cx.scope_height() * 2.0;
    let cell_height = ((budget - gap * (rows as f32 - 1.0)) / rows as f32).max(MINI_GRAPH_MIN_HEIGHT);
    let total_height = cell_height * rows as f32 + gap * (rows as f32 - 1.0);
    let (outer, outer_response) = ui.allocate_exact_size(vec2(width, total_height), egui::Sense::click());
    outer_response.widget_info(move || {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("CPU per-thread grid: {core_count} threads"))
    });
    let formatter = |values: &[f64]| format!("{:.0}%", values[0]);
    for index in 0..core_count {
        let row = index / columns;
        let column = index % columns;
        let min = outer.min + vec2(column as f32 * (cell_width + gap), row as f32 * (cell_height + gap));
        let cell = egui::Rect::from_min_size(min, vec2(cell_width, cell_height));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cell).layout(Layout::top_down(Align::Min)));
        let tag = index.to_string();
        let name = format!("CPU thread {index}");
        let empty = History::default();
        let history = per_core.get(index).unwrap_or(&empty);
        graphs::scope_graph(
            &mut child,
            cx.palette,
            &[history],
            &ScopeConfig {
                name: &name,
                height: cell_height,
                fixed_maximum: Some(100.0),
                minimum_autoscale: 100.0,
                colors: &[color],
                dashed_second: false,
                ceiling_formatter: &|v| format!("{v:.0}%"),
                hover_formatter: &formatter,
                tag: Some(&tag),
                mini: true,
                fill_all: false,
            },
        );
    }
    outer_response
}

fn memory_row(ui: &mut Ui, cx: &mut PerformanceContext) {
    let Some(memory) = cx.snapshot.memory.as_ref() else {
        placeholder_row(ui, cx, "Memory");
        return;
    };
    let used_color = cx.series(GRAPH_SERIES_MEMORY);
    let cache_color = used_color.gamma_multiply(0.55);
    let percent = memory.used_percent;
    let used = &cx.histories.memory;
    // Stacked (spec, R8): the lower trace is used, the upper trace is
    // used + cache, and the band between them is the cache. The
    // sampler keeps `memory_stacked` as that upper trace.
    let stacked = &cx.histories.memory_stacked;
    let label = format_percent(percent);
    resource_row(
        ui,
        cx,
        "Memory",
        &[(used_color, "used"), (cache_color, "cache")],
        (percent / 100.0, used_color, label),
        "memory",
        Some(SortColumn::Memory),
        |ui, cx, height| {
            graphs::scope_graph(
                ui,
                cx.palette,
                &[stacked, used],
                &ScopeConfig {
                    name: "Memory history",
                    height,
                    fixed_maximum: Some(100.0),
                    minimum_autoscale: 100.0,
                    colors: &[cache_color, used_color],
                    dashed_second: cx.dashed_second(),
                    ceiling_formatter: &|v| format!("{v:.0}%"),
                    hover_formatter: &|values: &[f64]| {
                        format!("used {:.0}%  cache {:.0}%", values[1], values[0] - values[1])
                    },
                    tag: None,
                    mini: false,
                    fill_all: true,
                },
            )
        },
        |_, _| {},
    );
}

fn gpu_row(ui: &mut Ui, cx: &mut PerformanceContext) {
    let Some(gpu) = cx.snapshot.gpu.as_ref().filter(|gpu| gpu.available) else {
        return;
    };
    let busy_color = cx.series(GRAPH_SERIES_GPU);
    let vram_color = cx.sibling(GRAPH_SERIES_GPU);
    let percent = gpu.busy_percent;
    let busy = &cx.histories.gpu;
    let vram = &cx.histories.vram;
    resource_row(
        ui,
        cx,
        "GPU",
        &[(busy_color, "busy"), (vram_color, "VRAM")],
        (percent / 100.0, busy_color, format_percent(percent)),
        "gpu",
        Some(SortColumn::Gpu),
        |ui, cx, height| {
            graphs::scope_graph(
                ui,
                cx.palette,
                &[busy, vram],
                &ScopeConfig {
                    name: "GPU history",
                    height,
                    fixed_maximum: Some(100.0),
                    minimum_autoscale: 100.0,
                    colors: &[busy_color, vram_color],
                    dashed_second: cx.dashed_second(),
                    ceiling_formatter: &|v| format!("{v:.0}%"),
                    hover_formatter: &percent_formatter(&["busy", "VRAM"]),
                    tag: None,
                    mini: false,
                    fill_all: false,
                },
            )
        },
        |_, _| {},
    );
}

fn disk_row(ui: &mut Ui, cx: &mut PerformanceContext) {
    let Some(disks) = cx.snapshot.disks.as_ref() else {
        placeholder_row(ui, cx, "Disk");
        return;
    };
    let read_color = cx.series(GRAPH_SERIES_NET_DOWN);
    let write_color = cx.series(GRAPH_SERIES_NET_UP);
    let util = disks.iter().map(|d| d.util_percent).fold(0.0f32, f32::max);
    let read = &cx.histories.disk_read;
    let write = &cx.histories.disk_write;
    let units = cx.display.units;
    let label = format!("{util:.0}%");
    let hover = move |values: &[f64]| {
        format!("read {}  write {}", format_rate(values[0], units), format_rate(values[1], units))
    };
    let ceiling = move |v: f64| format_rate(v, units);
    resource_row(
        ui,
        cx,
        "Disk",
        &[(read_color, "read"), (write_color, "write")],
        (util / 100.0, read_color, label),
        "disks",
        Some(SortColumn::Read),
        |ui, cx, height| {
            graphs::scope_graph(
                ui,
                cx.palette,
                &[read, write],
                &ScopeConfig {
                    name: "Disk history",
                    height,
                    fixed_maximum: None,
                    minimum_autoscale: 1.0e6,
                    colors: &[read_color, write_color],
                    dashed_second: cx.dashed_second(),
                    ceiling_formatter: &ceiling,
                    hover_formatter: &hover,
                    tag: None,
                    mini: false,
                    fill_all: false,
                },
            )
        },
        |_, _| {},
    );
}

fn network_row(ui: &mut Ui, cx: &mut PerformanceContext) {
    let Some(network) = cx.snapshot.network.as_ref() else {
        placeholder_row(ui, cx, "Network");
        return;
    };
    let down_color = cx.series(GRAPH_SERIES_NET_DOWN);
    let up_color = cx.series(GRAPH_SERIES_NET_UP);
    let down = &cx.histories.net_down;
    let up = &cx.histories.net_up;
    let units = cx.display.units;
    // The meter shows the share of the fastest physical link.
    let link_bps = network
        .interfaces
        .iter()
        .filter(|i| i.is_up)
        .filter_map(|i| i.speed_mbps)
        .max()
        .map(|mbps| mbps as f64 * 125_000.0);
    // The meter lights for whichever direction is busier, and the label
    // names that direction so the bar and the number agree (R5).
    let (busiest, arrow) = if network.upload_bps > network.download_bps {
        (network.upload_bps, "↑")
    } else {
        (network.download_bps, "↓")
    };
    let fraction = link_bps.map(|link| (busiest / link) as f32).unwrap_or(0.0);
    let label = format!("{arrow} {}", cx.rate(busiest));
    let hover = move |values: &[f64]| {
        format!("↓ {}  ↑ {}", format_rate(values[0], units), format_rate(values[1], units))
    };
    let ceiling = move |v: f64| format_rate(v, units);
    resource_row(
        ui,
        cx,
        "Network",
        &[(down_color, "down"), (up_color, "up")],
        (fraction, down_color, label),
        "network",
        Some(SortColumn::NetDown),
        |ui, cx, height| {
            graphs::scope_graph(
                ui,
                cx.palette,
                &[down, up],
                &ScopeConfig {
                    name: "Network history",
                    height,
                    fixed_maximum: None,
                    minimum_autoscale: 125_000.0,
                    colors: &[down_color, up_color],
                    dashed_second: cx.dashed_second(),
                    ceiling_formatter: &ceiling,
                    hover_formatter: &hover,
                    tag: None,
                    mini: false,
                    fill_all: false,
                },
            )
        },
        |_, _| {},
    );
}

fn thermals_row(ui: &mut Ui, cx: &mut PerformanceContext) {
    let Some(cpu) = cx.snapshot.cpu.as_ref() else {
        placeholder_row(ui, cx, "Thermals");
        return;
    };
    let (cpu_color, gpu_color) = cx.thermal_colors();
    let cpu_temperature = &cx.histories.cpu_temperature;
    let gpu_temperature = &cx.histories.gpu_temperature;
    let busy_clock = &cx.histories.busy_clock;
    let scale = cx.display.temperature;
    let celsius = cpu.temperature_celsius.unwrap_or(0.0);
    let label = cpu.temperature_celsius.map(|t| cx.temperature(t)).unwrap_or_else(|| "–".to_string());
    let clock = cpu.frequency_busy_mhz.or(cpu.frequency_mhz);
    let hover = move |values: &[f64]| {
        let mut parts = vec![format!("CPU {}", format_temperature_in(values[0] as f32, scale))];
        if let Some(gpu) = values.get(1) {
            parts.push(format!("GPU {}", format_temperature_in(*gpu as f32, scale)));
        }
        parts.join("  ")
    };
    let has_gpu = !gpu_temperature.is_empty();
    let hottest = cpu_temperature.max().max(gpu_temperature.max());
    resource_row(
        ui,
        cx,
        "Thermals",
        &[(cpu_color, "CPU"), (gpu_color, "GPU")],
        // The meter is the fraction of 100 °C, the number everyone
        // knows; it is a position, not a verdict.
        (celsius / 100.0, cpu_color, label),
        "sensors",
        None,
        |ui, cx, height| {
            let series: Vec<&History> = if has_gpu {
                vec![cpu_temperature, gpu_temperature]
            } else {
                vec![cpu_temperature]
            };
            graphs::scope_graph(
                ui,
                cx.palette,
                &series,
                &ScopeConfig {
                    name: "Thermal history",
                    height,
                    // 0..100 °C; a reading past 100 raises the ceiling
                    // and writes it in the corner (R22).
                    fixed_maximum: if hottest > 100.0 { None } else { Some(100.0) },
                    minimum_autoscale: 100.0,
                    colors: &[cpu_color, gpu_color],
                    dashed_second: cx.dashed_second(),
                    ceiling_formatter: &|v| format!("{v:.0}°C"),
                    hover_formatter: &hover,
                    tag: None,
                    mini: false,
                    fill_all: false,
                },
            )
        },
        |ui, _cx| {
            // The busy clock beside the heat, as a number: the
            // underpowered cooler shows up here as data.
            if let Some(mhz) = clock {
                ui.label(RichText::new(format_frequency_mhz(mhz)).monospace().size(10.5).color(ui.visuals().weak_text_color()))
                    .on_hover_text(format!("busy clock now; {} samples kept", busy_clock.len()));
            }
        },
    );
}

// ------------------------------------------------------------ group boxes

fn rows(ui: &mut Ui, palette: &Palette, entries: &[(&str, String, Option<String>)]) {
    egui::Grid::new(ui.next_auto_id()).num_columns(2).spacing(vec2(8.0, 1.0)).show(ui, |ui| {
        for (label, value, note) in entries {
            ui.label(RichText::new(*label).color(palette.muted).monospace().size(10.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let response = ui.add(egui::Label::new(RichText::new(value).monospace().size(11.0)).truncate());
                if let Some(note) = note {
                    response.on_hover_text(note);
                }
            });
            ui.end_row();
        }
    });
}

fn totals_box(ui: &mut Ui, cx: &mut PerformanceContext) {
    let processes = cx.snapshot.processes.as_ref().map(|p| p.len()).unwrap_or(0);
    // The kernel's own task count (loadavg's 4th field), the same
    // number the Overview's CPU tooltip shows (R18).
    let threads: u64 = cx.snapshot.cpu.as_ref().map(|c| c.tasks_total as u64).unwrap_or(0);
    let ctx = cx.snapshot.cpu.as_ref().map(|c| c.context_switches_per_second).unwrap_or(0.0);
    let uptime = cx.snapshot.system.as_ref().map(|s| s.uptime_seconds).unwrap_or(0.0);
    let entries = [
        ("Processes", processes.to_string(), None),
        ("Threads", threads.to_string(), None),
        ("Ctx/s", format!("{ctx:.0}"), Some("context switches per second, machine-wide".to_string())),
        ("Uptime", format_uptime(uptime), None),
    ];
    let palette = cx.palette;
    graphs::group_box(ui, palette, "Totals", |ui| rows(ui, palette, &entries));
    ui.add_space(cx.row_gap());
}

fn physical_box(ui: &mut Ui, cx: &mut PerformanceContext) {
    let Some(memory) = cx.snapshot.memory.as_ref() else {
        return;
    };
    let entries = [
        (
            "Installed",
            memory.installed_bytes.map(|b| cx.size(b)).unwrap_or_else(|| "–".to_string()),
            Some("sum of the fitted modules (SMBIOS)".to_string()),
        ),
        ("Usable", cx.size(memory.total_bytes), Some("MemTotal: after firmware and kernel reservations".to_string())),
        ("Available", cx.size(memory.available_bytes), None),
        ("Cache", cx.size(memory.cached_bytes), Some("Cached + SReclaimable".to_string())),
    ];
    let palette = cx.palette;
    graphs::group_box(ui, palette, "Physical memory", |ui| rows(ui, palette, &entries));
    ui.add_space(cx.row_gap());
}

fn commit_box(ui: &mut Ui, cx: &mut PerformanceContext) {
    let Some(memory) = cx.snapshot.memory.as_ref() else {
        return;
    };
    let entries = [
        ("Total", cx.size(memory.committed_bytes), Some("Committed_AS: address space promised to processes".to_string())),
        (
            "Limit",
            cx.size(memory.commit_limit_bytes),
            Some("CommitLimit: advisory under the default overcommit heuristic".to_string()),
        ),
        (
            "Peak*",
            cx.size(cx.histories.commit_peak_bytes.max(memory.committed_bytes)),
            Some("* since SysMon launched: SysMon's own high-water mark, not a kernel figure".to_string()),
        ),
    ];
    let palette = cx.palette;
    graphs::group_box(ui, palette, "Commit charge", |ui| rows(ui, palette, &entries));
    ui.add_space(cx.row_gap());
}

fn kernel_box(ui: &mut Ui, cx: &mut PerformanceContext) {
    let Some(memory) = cx.snapshot.memory.as_ref() else {
        return;
    };
    let entries = [
        ("Slab", cx.size(memory.slab_bytes), Some("every kernel slab cache, reclaimable and not".to_string())),
        ("Page tables", cx.size(memory.page_tables_bytes), None),
        ("Stacks", cx.size(memory.kernel_stack_bytes), Some("KernelStack".to_string())),
    ];
    let palette = cx.palette;
    graphs::group_box(ui, palette, "Kernel memory", |ui| rows(ui, palette, &entries));
    ui.add_space(cx.row_gap());
}

/// `3d 04:12` style uptime: days, then hours:minutes.
pub fn format_uptime(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours:02}:{minutes:02}")
    } else {
        format!("{hours:02}:{minutes:02}")
    }
}

// ------------------------------------------------------------ status bar

/// The status line's text: the same snapshot fields as the boxes. A
/// narrow window drops the commit limit so nothing truncates (V7).
pub fn status_text(snapshot: &SystemSnapshot, display: Display, wide: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(processes) = &snapshot.processes {
        parts.push(format!("{} proc", processes.len()));
    }
    if let Some(cpu) = &snapshot.cpu {
        parts.push(format!("CPU {:.0}%", cpu.overall_percent));
    }
    if let Some(memory) = &snapshot.memory {
        if wide {
            parts.push(format!(
                "Commit {} / {}",
                format_size(memory.committed_bytes, display.units),
                format_size(memory.commit_limit_bytes, display.units)
            ));
        } else {
            parts.push(format!("Commit {}", format_size(memory.committed_bytes, display.units)));
        }
    }
    if let Some(gpu) = snapshot.gpu.as_ref().filter(|g| g.available) {
        parts.push(format!("GPU {:.0}%", gpu.busy_percent));
    }
    if let Some(t) = snapshot.cpu.as_ref().and_then(|c| c.temperature_celsius) {
        parts.push(format_temperature_in(t, display.temperature));
    }
    parts.join(" │ ")
}

pub fn status_bar(ctx: &egui::Context, palette: &Palette, text: &str) {
    egui::TopBottomPanel::bottom("performance_status")
        .frame(egui::Frame::new().fill(palette.surface_2).inner_margin(egui::Margin::symmetric(8, 3)))
        .show(ctx, |ui| {
            // A sensed label gets a real accessibility node (the plain
            // label's text is otherwise invisible to the harness).
            let response = ui.add(
                egui::Label::new(RichText::new(text).monospace().size(11.0).color(palette.ink_2))
                    .truncate()
                    .sense(egui::Sense::click()),
            );
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("status: {text}")));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_follow_the_width_within_bounds() {
        assert_eq!(per_thread_columns(100.0), 2);
        assert_eq!(per_thread_columns(300.0), 3);
        assert_eq!(per_thread_columns(400.0), 4);
        assert_eq!(per_thread_columns(1200.0), 8);
        assert_eq!(per_thread_columns(5000.0), 8);
    }

    #[test]
    fn auto_mode_decides_by_width_and_a_choice_overrides_it() {
        assert_eq!(CpuGraphMode::resolve("auto", 300.0), CpuGraphMode::Combined);
        assert_eq!(CpuGraphMode::resolve("auto", 420.0), CpuGraphMode::PerThread);
        assert_eq!(CpuGraphMode::resolve("combined", 1200.0), CpuGraphMode::Combined);
        assert_eq!(CpuGraphMode::resolve("per_thread", 100.0), CpuGraphMode::PerThread);
    }

    #[test]
    fn uptime_reads_like_a_clock() {
        assert_eq!(format_uptime(0.0), "00:00");
        assert_eq!(format_uptime(3_600.0 * 4.0 + 60.0 * 12.0 + 86_400.0 * 3.0), "3d 04:12");
        assert_eq!(format_uptime(59.0), "00:00");
        assert_eq!(format_uptime(61.0), "00:01");
    }
}
