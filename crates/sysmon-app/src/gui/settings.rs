// SPDX-License-Identifier: GPL-3.0-or-later
//! Persisted preferences at ~/.config/sysmon/settings.json — the
//! SAME file v1 wrote, read compatibly so upgrade day keeps every
//! choice. v1's `theme_mode:"blossom"` meant the AMOLED true-black
//! look; a v1 file (no settings_version) maps it to `amoled` so
//! Ben's daily look survives. Files we write carry
//! `settings_version: 2` and the new theme ids.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const SECTION_KEYS: [&str; 6] = ["gpu", "memory", "cpu", "network", "disks", "sensors"];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub settings_version: u32,
    /// "system" | palette id ("blossom_dark", "amoled", …).
    pub theme_mode: String,
    pub update_interval_seconds: f64,
    pub always_on_top: bool,
    pub compact_mode: bool,
    pub show_pin_button: bool,
    pub graph_palette: String,
    pub use_binary_units: bool,
    pub visible_sections: HashMap<String, bool>,
    pub popped_out_sections: Vec<String>,
    pub window_width: i32,
    pub window_height: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            settings_version: 2,
            theme_mode: "blossom_dark".to_string(),
            update_interval_seconds: 2.0,
            always_on_top: false,
            compact_mode: false,
            show_pin_button: true,
            graph_palette: "blossom".to_string(),
            use_binary_units: false,
            visible_sections: SECTION_KEYS.iter().map(|k| (k.to_string(), true)).collect(),
            popped_out_sections: Vec::new(),
            window_width: 430,
            window_height: 780,
        }
    }
}

fn settings_path() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    config_home.join("sysmon").join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        let Ok(content) = std::fs::read_to_string(settings_path()) else {
            return Settings::default();
        };
        Settings::from_json(&content).unwrap_or_default()
    }

    /// Parse either era of the file. The container-level serde
    /// default would invent `settings_version: 2` for v1 files, so
    /// era detection happens on the raw JSON before typed parsing.
    pub fn from_json(content: &str) -> Option<Settings> {
        let raw: serde_json::Value = serde_json::from_str(content).ok()?;
        let is_v1_file = raw.get("settings_version").is_none();
        let mut settings: Settings = serde_json::from_value(raw).ok()?;

        if is_v1_file {
            // v1's "blossom" theme was the AMOLED look.
            settings.theme_mode = match settings.theme_mode.as_str() {
                "blossom" => "amoled".to_string(),
                "system" | "light" | "dark" | "funky" => settings.theme_mode.clone(),
                _ => Settings::default().theme_mode,
            };
        }
        settings.settings_version = 2;
        // Unknown ids (edited file, future versions) fall back gently.
        if settings.theme_mode != "system"
            && super::theme::palette_by_id(&settings.theme_mode).is_none()
        {
            settings.theme_mode = Settings::default().theme_mode;
        }
        for key in SECTION_KEYS {
            settings.visible_sections.entry(key.to_string()).or_insert(true);
        }
        settings
            .popped_out_sections
            .retain(|key| SECTION_KEYS.contains(&key.as_str()));
        settings.update_interval_seconds = settings.update_interval_seconds.clamp(0.5, 60.0);
        Some(settings)
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(serialized) = serde_json::to_string_pretty(self) {
            // Preferences are a convenience; never crash over them.
            let _ = std::fs::write(path, serialized);
        }
    }

    pub fn section_visible(&self, key: &str) -> bool {
        self.visible_sections.get(key).copied().unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_blossom_migrates_to_amoled() {
        let v1_json = r#"{
            "theme_mode": "blossom",
            "update_interval_seconds": 2.0,
            "always_on_top": false,
            "compact_mode": false,
            "show_pin_button": true,
            "graph_palette": "blossom",
            "use_binary_units": false,
            "visible_sections": {"gpu": true, "memory": true, "cpu": true,
                                  "network": true, "disks": true},
            "popped_out_sections": [],
            "window_width": 428,
            "window_height": 780
        }"#;
        let settings = Settings::from_json(v1_json).expect("v1 file parses");
        assert_eq!(settings.theme_mode, "amoled", "v1 blossom = the AMOLED look");
        assert_eq!(settings.settings_version, 2);
        assert_eq!(settings.graph_palette, "blossom");
        assert_eq!(settings.window_width, 428);
        // sensors is new in v2 — must default visible.
        assert!(settings.section_visible("sensors"));
    }

    #[test]
    fn v2_file_keeps_blossom_as_the_light_petal_theme() {
        let v2_json = r#"{"settings_version": 2, "theme_mode": "blossom"}"#;
        let settings = Settings::from_json(v2_json).expect("v2 file parses");
        assert_eq!(settings.theme_mode, "blossom");
    }

    #[test]
    fn missing_sensors_section_defaults_visible() {
        let settings = Settings::default();
        assert!(settings.section_visible("sensors"));
        assert!(settings.section_visible("gpu"));
    }
}
