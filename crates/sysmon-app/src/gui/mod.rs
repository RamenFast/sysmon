// SPDX-License-Identifier: GPL-3.0-or-later
//! The GUI: eframe shell over the shared sampler state. `run` is the
//! binary's default command; a plain re-launch raises the running
//! instance (exit 0), and a running `sysmon serve` hands the socket
//! over to the GUI (the GUI answers everything serve did, plus verbs).

pub mod actions;
pub mod app;
pub mod backend;
pub mod cards;
pub mod details;
pub mod graphs;
pub mod icons;
pub mod processes;
pub mod settings;
pub mod theme;

use std::sync::Arc;

use serde_json::json;

use crate::control::{self, BindError, ControlServer};
use crate::envelope::{EXIT_OK, EXIT_RUNTIME, EXIT_UNAVAILABLE};

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

/// Bind the control socket, negotiating with whoever holds it:
/// another GUI → raise it and exit 0; a serve daemon → ask it to
/// quit and take over (the GUI is a superset).
fn negotiate_socket(backend: Arc<backend::GuiBackend>) -> Result<Option<ControlServer>, i32> {
    for attempt in 0..2 {
        match ControlServer::bind(backend.clone()) {
            Ok(server) => return Ok(Some(server)),
            Err(BindError::AlreadyRunning(status)) => {
                let owner_mode = status["result"]["mode"].as_str().unwrap_or("?").to_string();
                if owner_mode == "gui" {
                    let _ = control::request(&json!({"verb": "raise"}));
                    println!("sysmon: raised the running instance");
                    return Err(EXIT_OK);
                }
                // serve (or something unreachable): ask it to leave.
                let _ = control::request(&json!({"verb": "quit"}));
                std::thread::sleep(std::time::Duration::from_millis(400));
                if attempt == 1 {
                    eprintln!("sysmon: the control socket is held by `{owner_mode}` and won't yield");
                    eprintln!("fix: `sysmon ctl quit`, then relaunch");
                    return Err(EXIT_UNAVAILABLE);
                }
            }
            Err(BindError::Io(io_error)) => {
                // The GUI can still run without its socket — degraded,
                // said out loud, never fatal.
                eprintln!(
                    "sysmon: control socket unavailable ({io_error}) — ctl/probe-via-socket \
                     won't reach this instance"
                );
                return Ok(None);
            }
        }
    }
    Ok(None)
}

pub fn run(_arguments: &[String]) -> i32 {
    let settings = settings::Settings::load();
    let shared = Arc::new(app::SharedUi::new(settings.update_interval_seconds));
    let (command_tx, command_rx) = std::sync::mpsc::channel();
    let gui_backend = Arc::new(backend::GuiBackend::new(shared.clone(), command_tx));

    let control_server = match negotiate_socket(gui_backend.clone()) {
        Ok(server) => server,
        Err(exit_code) => return exit_code,
    };

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
        Box::new(move |cc| {
            let _ = gui_backend.ctx.set(cc.egui_ctx.clone());
            Ok(Box::new(app::SysMonApp::new(
                cc,
                settings,
                shared,
                command_rx,
                control_server,
            )))
        }),
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
