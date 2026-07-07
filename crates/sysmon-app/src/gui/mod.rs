// SPDX-License-Identifier: GPL-3.0-or-later
//! The GUI: eframe shell over the shared sampler state. `run` is the
//! binary's default command.

pub mod actions;
pub mod app;
pub mod cards;
pub mod details;
pub mod graphs;
pub mod icons;
pub mod processes;
pub mod settings;
pub mod theme;

use crate::envelope::{EXIT_OK, EXIT_RUNTIME};

const APP_ICON_SVG: &[u8] = include_bytes!("../../../../assets/sysmon.svg");

fn app_icon() -> Option<egui::IconData> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(APP_ICON_SVG, &options).ok()?;
    let size = 128u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let scale = (size as f32 / tree.size().width()).min(size as f32 / tree.size().height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(egui::IconData {
        rgba: pixmap.data().to_vec(),
        width: size,
        height: size,
    })
}

pub fn run(_arguments: &[String]) -> i32 {
    let settings = settings::Settings::load();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("SysMon")
        .with_app_id("sysmon")
        .with_inner_size([
            settings.window_width.max(340) as f32,
            settings.window_height.max(420) as f32,
        ])
        .with_min_inner_size([340.0, 420.0]);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    if settings.always_on_top {
        viewport = viewport.with_always_on_top();
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    match eframe::run_native(
        "SysMon",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::SysMonApp::new(cc, settings)))),
    ) {
        Ok(()) => EXIT_OK,
        Err(error) => {
            eprintln!("sysmon: the GUI could not start: {error}");
            eprintln!(
                "fix: check a display is reachable ($DISPLAY/$WAYLAND_DISPLAY), or use \
                 `sysmon --background` / `sysmon serve`"
            );
            EXIT_RUNTIME
        }
    }
}
