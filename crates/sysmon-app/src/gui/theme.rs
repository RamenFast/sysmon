// SPDX-License-Identifier: GPL-3.0-or-later
//! The chrome design system (Ben's house style — phosphor theme.rs
//! is the canonical native implementation, mirrored here).
//!
//! Hard rules, non-negotiable:
//! - sharp corners everywhere (corner radius 0); no pills
//! - hairline 1px low-opacity strokes are the frames
//! - monospace for all DATA (values, ids, rates, labels)
//! - dimensional hierarchy: the few important controls are carved
//!   (the stone treatment); lower-tier info stays flat and boxy
//!
//! Six palettes + a System mode that maps the Cinnamon GTK theme to
//! the nearest family. Blossom Dark is the fresh-install default;
//! `amoled` is v1's true-black Blossom look (what Ben actually runs)
//! and v1 settings migrate to it.

use egui::{Color32, CornerRadius, Stroke};

#[derive(Clone, Copy)]
pub struct Palette {
    pub id: &'static str,
    pub label: &'static str,
    pub dark: bool,
    pub plane: Color32,     // window fill behind cards
    pub surface: Color32,   // card face
    pub surface_2: Color32, // recessed / lower tier
    pub ink: Color32,       // primary text
    pub ink_2: Color32,     // secondary text
    pub muted: Color32,     // faint text
    pub line: Color32,      // hairline frame
    pub line_strong: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    /// Section titles (v1 blossom's soft pink) and headline values
    /// (v1's gold) — the two signature text colors.
    pub title: Color32,
    pub value: Color32,
    // carved-stone triple for the few dimensional controls
    // (worn by the primary controls in the polish pass)
    #[allow(dead_code)]
    pub stone: Color32,
    #[allow(dead_code)]
    pub stone_hi: Color32,
    #[allow(dead_code)]
    pub stone_lo: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}
const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(r, g, b, a)
}

pub const PALETTES: [Palette; 10] = [
    // ── Blossom Dark — warm wine-plum ground, sakura-rose accent.
    //    The wanted default (house tokens verbatim). ──
    Palette {
        id: "blossom_dark",
        label: "Blossom Dark",
        dark: true,
        plane: rgb(0x1c, 0x10, 0x16),
        surface: rgb(0x28, 0x18, 0x21),
        surface_2: rgb(0x33, 0x21, 0x2c),
        ink: rgb(0xf5, 0xea, 0xef),
        ink_2: rgb(0xc9, 0xb0, 0xbc),
        muted: rgb(0x91, 0x79, 0x86),
        line: rgba(244, 233, 238, 36),
        line_strong: rgba(244, 233, 238, 82),
        accent: rgb(0xec, 0x8f, 0xac),
        on_accent: rgb(0x1a, 0x0e, 0x14),
        title: rgb(0xec, 0x8f, 0xac),
        value: rgb(0xe8, 0xc8, 0x7e),
        stone: rgb(0x3b, 0x26, 0x31),
        stone_hi: rgb(0x55, 0x39, 0x48),
        stone_lo: rgb(0x1d, 0x11, 0x17),
    },
    // ── Blossom — warm sakura petal-on-paper (house tokens) ──
    Palette {
        id: "blossom",
        label: "Blossom",
        dark: false,
        plane: rgb(0xf3, 0xe6, 0xe6),
        surface: rgb(0xfc, 0xf4, 0xf3),
        surface_2: rgb(0xf6, 0xe9, 0xe9),
        ink: rgb(0x2b, 0x21, 0x28),
        ink_2: rgb(0x5f, 0x4f, 0x57),
        muted: rgb(0x9c, 0x88, 0x90),
        line: rgba(43, 33, 40, 41),
        line_strong: rgba(43, 33, 40, 77),
        accent: rgb(0xc8, 0x5a, 0x7c),
        on_accent: rgb(0xff, 0xf7, 0xf9),
        title: rgb(0xa8, 0x3e, 0x60),
        value: rgb(0x7d, 0x4a, 0x6a),
        stone: rgb(0xe7, 0xda, 0xd9),
        stone_hi: rgb(0xff, 0xfa, 0xfa),
        stone_lo: rgb(0xc9, 0xb6, 0xb8),
    },
    // ── Blossom AMOLED — v1's true-black look: pink titles, gold
    //    numbers. Ben's daily driver; v1 settings land here. ──
    Palette {
        id: "amoled",
        label: "Blossom AMOLED",
        dark: true,
        plane: rgb(0x00, 0x00, 0x00),
        surface: rgb(0x0c, 0x08, 0x09),
        surface_2: rgb(0x14, 0x0d, 0x0f),
        ink: rgb(0xf2, 0xe9, 0xec),
        ink_2: rgb(0xc4, 0xb2, 0xb9),
        muted: rgb(0x8a, 0x77, 0x7e),
        line: rgba(243, 178, 196, 41),
        line_strong: rgba(243, 178, 196, 80),
        accent: rgb(0xf3, 0xb2, 0xc4),
        on_accent: rgb(0x14, 0x08, 0x0c),
        title: rgb(0xf3, 0xb2, 0xc4),
        value: rgb(0xe8, 0xc8, 0x7e),
        stone: rgb(0x1c, 0x12, 0x15),
        stone_hi: rgb(0x33, 0x22, 0x28),
        stone_lo: rgb(0x05, 0x03, 0x04),
    },
    // ── Light — cool neutral instrument (house tokens) ──
    Palette {
        id: "light",
        label: "Light",
        dark: false,
        plane: rgb(0xea, 0xee, 0xf2),
        surface: rgb(0xff, 0xff, 0xff),
        surface_2: rgb(0xf5, 0xf8, 0xfa),
        ink: rgb(0x0e, 0x16, 0x20),
        ink_2: rgb(0x43, 0x51, 0x5e),
        muted: rgb(0x7a, 0x88, 0x94),
        line: rgba(14, 22, 32, 31),
        line_strong: rgba(14, 22, 32, 71),
        accent: rgb(0x0c, 0x94, 0xa2),
        on_accent: rgb(0xff, 0xff, 0xff),
        title: rgb(0x28, 0x3c, 0x50),
        value: rgb(0x0c, 0x94, 0xa2),
        stone: rgb(0xe4, 0xe9, 0xee),
        stone_hi: rgb(0xff, 0xff, 0xff),
        stone_lo: rgb(0xc2, 0xcc, 0xd4),
    },
    // ── Dark — deep instrument, plum-tinted near-black ──
    Palette {
        id: "dark",
        label: "Dark",
        dark: true,
        plane: rgb(0x0a, 0x08, 0x10),
        surface: rgb(0x14, 0x10, 0x19),
        surface_2: rgb(0x1b, 0x15, 0x22),
        ink: rgb(0xf0, 0xea, 0xf0),
        ink_2: rgb(0xb3, 0xa6, 0xb3),
        muted: rgb(0x7d, 0x6f, 0x7d),
        line: rgba(240, 234, 240, 31),
        line_strong: rgba(240, 234, 240, 66),
        accent: rgb(0xe7, 0x8a, 0xa6),
        on_accent: rgb(0x16, 0x08, 0x10),
        title: rgb(0xd8, 0xc4, 0xd8),
        value: rgb(0xe7, 0x8a, 0xa6),
        stone: rgb(0x24, 0x1d, 0x29),
        stone_hi: rgb(0x33, 0x28, 0x38),
        stone_lo: rgb(0x14, 0x0f, 0x18),
    },
    // ── Funky Pink — v1's bubblegum pop-art, resurfaced with the
    //    house discipline (colors loud, corners SHARP) ──
    Palette {
        id: "funky",
        label: "Funky Pink",
        dark: false,
        plane: rgb(0xff, 0xdd, 0xee),
        surface: rgb(0xff, 0xf5, 0xfa),
        surface_2: rgb(0xff, 0xea, 0xf4),
        ink: rgb(0x4a, 0x10, 0x30),
        ink_2: rgb(0x8a, 0x3a, 0x62),
        muted: rgb(0xa8, 0x4a, 0x85),
        line: rgba(224, 33, 138, 90),
        line_strong: rgba(224, 33, 138, 160),
        accent: rgb(0xe0, 0x21, 0x8a),
        on_accent: rgb(0xff, 0xff, 0xff),
        title: rgb(0xe0, 0x21, 0x8a),
        value: rgb(0x7a, 0x1f, 0xd0),
        stone: rgb(0xff, 0xc6, 0xe2),
        stone_hi: rgb(0xff, 0xf0, 0xf8),
        stone_lo: rgb(0xe8, 0x8f, 0xc0),
    },
    // ── Paper — warm reading light: tan paper, leather accent.
    //    Deliberately paper-tan, not generic cream. ──
    Palette {
        id: "paper",
        label: "Paper",
        dark: false,
        plane: rgb(0xef, 0xe8, 0xda),
        surface: rgb(0xfa, 0xf6, 0xec),
        surface_2: rgb(0xf3, 0xed, 0xdf),
        ink: rgb(0x2e, 0x29, 0x20),
        ink_2: rgb(0x5e, 0x55, 0x44),
        muted: rgb(0x96, 0x8a, 0x70),
        line: rgba(46, 41, 32, 41),
        line_strong: rgba(46, 41, 32, 82),
        accent: rgb(0x9c, 0x5c, 0x24),
        on_accent: rgb(0xfd, 0xf8, 0xf0),
        title: rgb(0x6b, 0x4a, 0x28),
        value: rgb(0x8a, 0x58, 0x1e),
        stone: rgb(0xe4, 0xdb, 0xc8),
        stone_hi: rgb(0xff, 0xfb, 0xf2),
        stone_lo: rgb(0xc4, 0xb6, 0x9c),
    },
    // ── Basalt — bevel city: carved gray stone, steel-blue accent.
    //    The room where the stone controls do the talking. ──
    Palette {
        id: "basalt",
        label: "Basalt",
        dark: true,
        plane: rgb(0x0f, 0x11, 0x13),
        surface: rgb(0x18, 0x1b, 0x1e),
        surface_2: rgb(0x21, 0x25, 0x29),
        ink: rgb(0xe6, 0xeb, 0xf0),
        ink_2: rgb(0xad, 0xb6, 0xbf),
        muted: rgb(0x74, 0x7e, 0x88),
        line: rgba(230, 235, 240, 33),
        line_strong: rgba(230, 235, 240, 74),
        accent: rgb(0x7a, 0xa4, 0xc4),
        on_accent: rgb(0x0c, 0x12, 0x18),
        title: rgb(0xb4, 0xc2, 0xd0),
        value: rgb(0x8a, 0xb4, 0xd4),
        stone: rgb(0x26, 0x2b, 0x30),
        stone_hi: rgb(0x38, 0x3f, 0x46),
        stone_lo: rgb(0x12, 0x15, 0x18),
    },
    // ── Amber CRT — the service-terminal instrument: warm black,
    //    amber phosphor text. ──
    Palette {
        id: "amber",
        label: "Amber CRT",
        dark: true,
        plane: rgb(0x0b, 0x08, 0x04),
        surface: rgb(0x15, 0x10, 0x08),
        surface_2: rgb(0x1e, 0x17, 0x0c),
        ink: rgb(0xf4, 0xe3, 0xc2),
        ink_2: rgb(0xcd, 0xb2, 0x86),
        muted: rgb(0x8f, 0x78, 0x56),
        line: rgba(244, 227, 194, 36),
        line_strong: rgba(244, 227, 194, 82),
        accent: rgb(0xf0, 0xa8, 0x30),
        on_accent: rgb(0x1a, 0x10, 0x02),
        title: rgb(0xf0, 0xa8, 0x30),
        value: rgb(0xff, 0xd2, 0x70),
        stone: rgb(0x26, 0x1d, 0x10),
        stone_hi: rgb(0x3d, 0x30, 0x1c),
        stone_lo: rgb(0x10, 0x0b, 0x05),
    },
    // ── Chromacore — the 1905 terminal: near-black green-cast
    //    ground, phosphor-green semantic ink (the NFO discipline
    //    this design language grew from). ──
    Palette {
        id: "chromacore",
        label: "Chromacore",
        dark: true,
        plane: rgb(0x04, 0x08, 0x06),
        surface: rgb(0x09, 0x0f, 0x0c),
        surface_2: rgb(0x0f, 0x17, 0x12),
        ink: rgb(0xd2, 0xe8, 0xdc),
        ink_2: rgb(0x9c, 0xc4, 0xae),
        muted: rgb(0x5f, 0x8a, 0x72),
        line: rgba(120, 220, 160, 40),
        line_strong: rgba(120, 220, 160, 90),
        accent: rgb(0x2f, 0xd2, 0x7a),
        on_accent: rgb(0x03, 0x14, 0x0a),
        title: rgb(0x2f, 0xd2, 0x7a),
        value: rgb(0x7c, 0xe8, 0xa8),
        stone: rgb(0x12, 0x1e, 0x17),
        stone_hi: rgb(0x1f, 0x33, 0x27),
        stone_lo: rgb(0x06, 0x0c, 0x08),
    },
];

pub fn palette_by_id(id: &str) -> Option<&'static Palette> {
    PALETTES.iter().find(|palette| palette.id == id)
}

/// System mode: map the Cinnamon GTK theme to our nearest family.
/// Blossom (Ben's theme) is a dark theme → blossom_dark; anything
/// with "dark" in the name → dark; otherwise light.
pub fn palette_for_system() -> &'static Palette {
    let theme_name = gtk_theme_name().unwrap_or_default().to_lowercase();
    let prefers_dark = color_scheme_prefers_dark();
    let id = if theme_name.contains("blossom") {
        if prefers_dark.unwrap_or(true) {
            "blossom_dark"
        } else {
            "blossom"
        }
    } else if theme_name.contains("dark") || prefers_dark == Some(true) {
        "dark"
    } else {
        "light"
    };
    palette_by_id(id).unwrap_or(&PALETTES[0])
}

fn gsettings_get(schema: &str, key: &str) -> Option<String> {
    let output = std::process::Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .trim_matches('\'')
            .trim_matches('"')
            .to_string()
    })
}

fn gtk_theme_name() -> Option<String> {
    gsettings_get("org.cinnamon.desktop.interface", "gtk-theme")
        .or_else(|| gsettings_get("org.gnome.desktop.interface", "gtk-theme"))
}

fn color_scheme_prefers_dark() -> Option<bool> {
    for schema in ["org.x.apps.portal", "org.gnome.desktop.interface"] {
        if let Some(scheme) = gsettings_get(schema, "color-scheme") {
            if scheme.contains("dark") {
                return Some(true);
            }
            if scheme.contains("light") || scheme == "default" {
                return Some(false);
            }
        }
    }
    None
}

/// Apply a palette to egui: sharp corners, hairline strokes, token
/// colors, monospace-forward text styles.
pub fn apply(ctx: &egui::Context, palette: &Palette) {
    let mut style = (*ctx.style()).clone();

    style.visuals.dark_mode = palette.dark;
    style.visuals.override_text_color = Some(palette.ink);
    style.visuals.panel_fill = palette.plane;
    style.visuals.window_fill = palette.surface;
    style.visuals.extreme_bg_color = palette.surface_2;
    style.visuals.faint_bg_color = palette.surface_2;
    style.visuals.window_stroke = Stroke::new(1.0, palette.line_strong);
    style.visuals.selection.bg_fill = palette.accent.gamma_multiply(0.35);
    style.visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    style.visuals.hyperlink_color = palette.accent;
    style.visuals.warn_fg_color = palette.value;
    style.visuals.error_fg_color = palette.accent;

    let corner = CornerRadius::ZERO;
    style.visuals.window_corner_radius = corner;
    style.visuals.menu_corner_radius = corner;

    let widgets = &mut style.visuals.widgets;
    for (visuals, bg, stroke) in [
        (&mut widgets.noninteractive, palette.surface, palette.line),
        (&mut widgets.inactive, palette.surface_2, palette.line),
        (&mut widgets.hovered, palette.surface_2, palette.line_strong),
        (&mut widgets.active, palette.surface_2, palette.accent),
        (&mut widgets.open, palette.surface_2, palette.line_strong),
    ] {
        visuals.bg_fill = bg;
        visuals.weak_bg_fill = bg;
        visuals.bg_stroke = Stroke::new(1.0, stroke);
        visuals.corner_radius = corner;
    }
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.ink_2);
    widgets.inactive.fg_stroke = Stroke::new(1.0, palette.ink);
    widgets.hovered.fg_stroke = Stroke::new(1.0, palette.ink);
    widgets.active.fg_stroke = Stroke::new(1.0, palette.ink);
    widgets.open.fg_stroke = Stroke::new(1.0, palette.ink);
    widgets.hovered.expansion = 0.0;
    widgets.active.expansion = 0.0;

    // Data app: monospace numbers everywhere they matter.
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Small, FontId::new(11.0, FontFamily::Proportional));
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::new(15.0, FontFamily::Proportional));

    style.spacing.item_spacing = egui::vec2(8.0, 4.0);
    style.spacing.button_padding = egui::vec2(6.0, 2.0);
    style.spacing.menu_margin = egui::Margin::same(8);

    ctx.set_style(style);
}

/// The sharp hairline card frame every section lives in.
pub fn card_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(egui::Margin::same(8))
}

// ------------------------------------------------------ graph palettes

/// v1's seven graph palettes, carried over verbatim (light + dark
/// variants per palette; five semantic series each).
#[derive(Clone, Copy)]
pub struct GraphPalette {
    pub id: &'static str,
    pub label: &'static str,
    pub light: [Color32; 5],
    pub dark: [Color32; 5],
}

pub const GRAPH_SERIES_GPU: usize = 0;
pub const GRAPH_SERIES_MEMORY: usize = 1;
pub const GRAPH_SERIES_CPU: usize = 2;
pub const GRAPH_SERIES_NET_DOWN: usize = 3;
pub const GRAPH_SERIES_NET_UP: usize = 4;

const fn c(r: f32, g: f32, b: f32) -> Color32 {
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

pub const GRAPH_PALETTES: [GraphPalette; 9] = [
    GraphPalette {
        id: "mint",
        label: "Mint",
        light: [
            c(0.80, 0.25, 0.20),
            c(0.47, 0.32, 0.74),
            c(0.14, 0.45, 0.80),
            c(0.13, 0.55, 0.30),
            c(0.82, 0.48, 0.08),
        ],
        dark: [
            c(0.96, 0.48, 0.40),
            c(0.72, 0.58, 0.95),
            c(0.42, 0.69, 0.97),
            c(0.44, 0.83, 0.56),
            c(0.96, 0.68, 0.32),
        ],
    },
    GraphPalette {
        id: "aqua",
        label: "Aqua",
        light: [
            c(0.00, 0.50, 0.66),
            c(0.27, 0.36, 0.74),
            c(0.00, 0.56, 0.53),
            c(0.07, 0.48, 0.78),
            c(0.52, 0.38, 0.72),
        ],
        dark: [
            c(0.22, 0.76, 0.92),
            c(0.56, 0.64, 0.98),
            c(0.28, 0.85, 0.79),
            c(0.38, 0.72, 0.98),
            c(0.78, 0.63, 0.96),
        ],
    },
    GraphPalette {
        id: "sunset",
        label: "Sunset",
        light: [
            c(0.80, 0.20, 0.26),
            c(0.71, 0.25, 0.46),
            c(0.84, 0.44, 0.10),
            c(0.72, 0.54, 0.05),
            c(0.55, 0.27, 0.52),
        ],
        dark: [
            c(0.98, 0.46, 0.43),
            c(0.94, 0.52, 0.66),
            c(0.98, 0.66, 0.32),
            c(0.96, 0.79, 0.36),
            c(0.82, 0.57, 0.86),
        ],
    },
    GraphPalette {
        id: "forest",
        label: "Forest",
        light: [
            c(0.21, 0.52, 0.26),
            c(0.10, 0.45, 0.40),
            c(0.44, 0.52, 0.14),
            c(0.14, 0.49, 0.55),
            c(0.60, 0.44, 0.18),
        ],
        dark: [
            c(0.47, 0.82, 0.52),
            c(0.42, 0.77, 0.66),
            c(0.72, 0.81, 0.42),
            c(0.46, 0.79, 0.84),
            c(0.87, 0.72, 0.46),
        ],
    },
    GraphPalette {
        id: "mono",
        label: "Mono",
        light: [
            c(0.29, 0.32, 0.37),
            c(0.29, 0.32, 0.37),
            c(0.29, 0.32, 0.37),
            c(0.29, 0.32, 0.37),
            c(0.56, 0.59, 0.64),
        ],
        dark: [
            c(0.78, 0.80, 0.84),
            c(0.78, 0.80, 0.84),
            c(0.78, 0.80, 0.84),
            c(0.78, 0.80, 0.84),
            c(0.53, 0.55, 0.60),
        ],
    },
    GraphPalette {
        id: "blossom",
        label: "Blossom",
        light: [
            c(0.84, 0.41, 0.55),
            c(0.72, 0.55, 0.17),
            c(0.77, 0.32, 0.47),
            c(0.69, 0.54, 0.14),
            c(0.62, 0.40, 0.53),
        ],
        dark: [
            c(0.97, 0.66, 0.75),
            c(0.92, 0.77, 0.43),
            c(0.94, 0.52, 0.65),
            c(0.96, 0.85, 0.59),
            c(0.81, 0.56, 0.67),
        ],
    },
    GraphPalette {
        id: "funky",
        label: "Funky",
        light: [
            c(0.93, 0.13, 0.57),
            c(0.55, 0.17, 0.89),
            c(0.00, 0.65, 0.62),
            c(0.10, 0.50, 0.95),
            c(0.98, 0.55, 0.00),
        ],
        dark: [
            c(1.00, 0.42, 0.71),
            c(0.78, 0.55, 1.00),
            c(0.25, 0.90, 0.85),
            c(0.40, 0.75, 1.00),
            c(1.00, 0.72, 0.30),
        ],
    },
    GraphPalette {
        // Warm phosphor family for the Amber CRT room.
        id: "amber",
        label: "Amber",
        light: [
            c(0.69, 0.42, 0.06),
            c(0.54, 0.42, 0.06),
            c(0.75, 0.35, 0.09),
            c(0.56, 0.44, 0.06),
            c(0.48, 0.29, 0.13),
        ],
        dark: [
            c(0.94, 0.66, 0.19),
            c(1.00, 0.82, 0.44),
            c(1.00, 0.56, 0.24),
            c(0.91, 0.78, 0.49),
            c(0.75, 0.47, 0.25),
        ],
    },
    GraphPalette {
        // Green-cyan family for the Chromacore terminal.
        id: "terminal",
        label: "Terminal",
        light: [
            c(0.06, 0.54, 0.28),
            c(0.16, 0.48, 0.32),
            c(0.06, 0.54, 0.47),
            c(0.35, 0.54, 0.06),
            c(0.05, 0.42, 0.23),
        ],
        dark: [
            c(0.18, 0.82, 0.48),
            c(0.49, 0.91, 0.66),
            c(0.22, 0.91, 0.78),
            c(0.66, 0.91, 0.24),
            c(0.13, 0.66, 0.38),
        ],
    },
];

pub fn graph_palette_by_id(id: &str) -> &'static GraphPalette {
    GRAPH_PALETTES
        .iter()
        .find(|palette| palette.id == id)
        .unwrap_or(&GRAPH_PALETTES[0])
}

pub fn graph_color(palette_id: &str, series: usize, dark: bool) -> Color32 {
    let palette = graph_palette_by_id(palette_id);
    let colors = if dark { &palette.dark } else { &palette.light };
    colors[series.min(4)]
}

/// Themes that bring a matching graph palette along when selected
/// (v1 behavior; the palette stays freely changeable afterwards).
pub fn companion_graph_palette(theme_id: &str) -> Option<&'static str> {
    match theme_id {
        "blossom" | "blossom_dark" | "amoled" => Some("blossom"),
        "funky" => Some("funky"),
        "amber" => Some("amber"),
        "chromacore" => Some("terminal"),
        "basalt" => Some("mono"),
        "paper" => Some("sunset"),
        _ => None,
    }
}
