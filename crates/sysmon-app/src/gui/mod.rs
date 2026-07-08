// SPDX-License-Identifier: GPL-3.0-or-later
//! The GUI: eframe shell over the shared sampler state. `run` is the
//! binary's default command; a plain re-launch raises a same-version
//! running instance (exit 0) and REPLACES an older one (upgrade day:
//! the freshly installed binary must be the one on screen), and a
//! running `sysmon serve` hands the socket over to the GUI (the GUI
//! answers everything serve did, plus verbs).

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
/// a same-version GUI → raise it and exit 0; an OLDER GUI (upgrade
/// day: the deb was installed while the old window was up) → ask it
/// to quit and take over, so a plain relaunch always shows the
/// version you just installed; a serve daemon → quit + take over
/// (the GUI is a superset).
fn negotiate_socket(backend: Arc<backend::GuiBackend>) -> Result<Option<ControlServer>, i32> {
    // A GUI teardown (wgpu + viewports) takes longer than serve's —
    // give the old owner a few grace windows before giving up.
    const ATTEMPTS: u32 = 4;
    for attempt in 0..ATTEMPTS {
        match ControlServer::bind(backend.clone()) {
            Ok(server) => return Ok(Some(server)),
            Err(BindError::AlreadyRunning(status)) => {
                let owner_mode = status["result"]["mode"].as_str().unwrap_or("?").to_string();
                let owner_version =
                    status["result"]["version"].as_str().unwrap_or("?").to_string();
                if owner_mode == "gui" && owner_version == sysmon_core::VERSION {
                    let _ = control::request(&json!({"verb": "raise"}));
                    println!("sysmon: raised the running instance");
                    return Err(EXIT_OK);
                }
                if owner_mode == "gui" && attempt == 0 {
                    println!(
                        "sysmon: replacing the running {owner_version} instance \
                         with {}",
                        sysmon_core::VERSION
                    );
                }
                // serve, an older GUI, or something unreachable:
                // ask it to leave and take the socket over.
                let _ = control::request(&json!({"verb": "quit"}));
                std::thread::sleep(std::time::Duration::from_millis(500));
                if attempt == ATTEMPTS - 1 {
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

#[cfg(test)]
mod icon_tests {
    /// The embedded SVG must survive resvg (clipPath + filters are
    /// easy to break) — a parse failure here would ship a silently
    /// iconless window.
    #[test]
    fn app_icon_renders() {
        let icon = super::app_icon().expect("icon SVG must parse and render");
        assert_eq!((icon.width, icon.height), (128, 128));
        let lit = icon
            .rgba
            .chunks(4)
            .filter(|px| px[3] > 0 && (px[0] > 40 || px[1] > 40 || px[2] > 40))
            .count();
        assert!(lit > 500, "icon rendered nearly blank ({lit} lit pixels)");
    }
}
