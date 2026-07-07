// SPDX-License-Identifier: GPL-3.0-or-later
//! Process icons in the GUI: resolve through sysmon-core's AppIndex
//! (desktop entries + the real icon-theme inherit chain), rasterize
//! png via `image` and svg via `resvg`, cache egui textures by file
//! path. Processes with no icon get a deterministic letter tile in a
//! theme-friendly hue — nicer than a sea of identical gear glyphs —
//! and kernel threads stay deliberately bare.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions};

use sysmon_core::apps::AppIndex;
use sysmon_core::snapshot::ProcessRecord;

const RASTER_SIZE: u32 = 32;

pub struct IconCache {
    app_index: Arc<AppIndex>,
    /// process key → resolved texture (None = draw the letter tile).
    by_process: HashMap<String, Option<TextureHandle>>,
    /// icon file path → texture (many processes share one file).
    by_path: HashMap<String, Option<TextureHandle>>,
}

impl IconCache {
    pub fn new() -> Self {
        IconCache {
            app_index: Arc::new(AppIndex::load()),
            by_process: HashMap::new(),
            by_path: HashMap::new(),
        }
    }

    /// The texture for a process, resolving + rasterizing on first
    /// sight. Kernel threads always answer None.
    pub fn texture_for(
        &mut self,
        ctx: &Context,
        record: &ProcessRecord,
    ) -> Option<TextureHandle> {
        if record.is_kernel_thread {
            return None;
        }
        let key = record
            .exe_basename
            .clone()
            .unwrap_or_else(|| record.name.clone())
            .to_lowercase();
        if let Some(cached) = self.by_process.get(&key) {
            return cached.clone();
        }

        let path = self.app_index.icon_for_process(
            &record.name,
            record.exe_basename.as_deref(),
            &record.command_line,
            RASTER_SIZE,
        );
        let texture = path.and_then(|path| self.texture_for_path(ctx, &path));
        self.by_process.insert(key, texture.clone());
        texture
    }

    fn texture_for_path(&mut self, ctx: &Context, path: &Path) -> Option<TextureHandle> {
        let path_key = path.to_string_lossy().to_string();
        if let Some(cached) = self.by_path.get(&path_key) {
            return cached.clone();
        }
        let image = rasterize(path);
        let texture = image.map(|image| {
            ctx.load_texture(
                format!("icon:{path_key}"),
                image,
                TextureOptions::LINEAR,
            )
        });
        self.by_path.insert(path_key, texture.clone());
        texture
    }
}

fn rasterize(path: &Path) -> Option<ColorImage> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("svg") => rasterize_svg(path),
        Some("png") => rasterize_png(path),
        _ => rasterize_png(path).or_else(|| rasterize_svg(path)),
    }
}

fn rasterize_png(path: &Path) -> Option<ColorImage> {
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory(&bytes).ok()?;
    let resized = image.resize_exact(
        RASTER_SIZE,
        RASTER_SIZE,
        image::imageops::FilterType::CatmullRom,
    );
    let rgba = resized.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [RASTER_SIZE as usize, RASTER_SIZE as usize],
        rgba.as_raw(),
    ))
}

fn rasterize_svg(path: &Path) -> Option<ColorImage> {
    let bytes = std::fs::read(path).ok()?;
    let options = resvg::usvg::Options {
        resources_dir: path.parent().map(|p| p.to_path_buf()),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(&bytes, &options).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(RASTER_SIZE, RASTER_SIZE)?;
    let source_size = tree.size();
    let scale = (RASTER_SIZE as f32 / source_size.width())
        .min(RASTER_SIZE as f32 / source_size.height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(ColorImage::from_rgba_unmultiplied(
        [RASTER_SIZE as usize, RASTER_SIZE as usize],
        pixmap.data(),
    ))
}

/// Deterministic letter-tile fallback: a sharp colored square with
/// the process's first letter. Same process, same hue, every run.
pub fn draw_letter_tile(
    painter: &egui::Painter,
    rect: egui::Rect,
    name: &str,
    is_kernel_thread: bool,
    muted: Color32,
) {
    if is_kernel_thread {
        // Kernel workers stay bare: a hollow frame, no letter.
        painter.rect_stroke(
            rect.shrink(2.0),
            0.0,
            egui::Stroke::new(1.0, muted.gamma_multiply(0.6)),
            egui::StrokeKind::Inside,
        );
        return;
    }
    let letter = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('·');
    let hue_index = (name
        .bytes()
        .fold(0u32, |accumulator, byte| {
            accumulator.wrapping_mul(31).wrapping_add(byte as u32)
        })
        % TILE_HUES.len() as u32) as usize;
    let color = TILE_HUES[hue_index];
    painter.rect_filled(rect.shrink(2.0), 0.0, color.gamma_multiply(0.30));
    painter.rect_stroke(
        rect.shrink(2.0),
        0.0,
        egui::Stroke::new(1.0, color.gamma_multiply(0.8)),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        letter,
        egui::FontId::monospace(rect.height() * 0.62),
        color,
    );
}

/// Hues picked to sit on every palette's surface (mid-lightness,
/// medium chroma).
const TILE_HUES: [Color32; 8] = [
    Color32::from_rgb(0xd8, 0x7a, 0x8f), // rose
    Color32::from_rgb(0xc9, 0x9a, 0x5b), // amber
    Color32::from_rgb(0x8f, 0xb0, 0x6a), // leaf
    Color32::from_rgb(0x6a, 0xa8, 0xb0), // teal
    Color32::from_rgb(0x7d, 0x93, 0xc9), // periwinkle
    Color32::from_rgb(0xa8, 0x82, 0xc9), // violet
    Color32::from_rgb(0xc9, 0x82, 0xb5), // orchid
    Color32::from_rgb(0x9a, 0x8f, 0x7d), // stone
];
