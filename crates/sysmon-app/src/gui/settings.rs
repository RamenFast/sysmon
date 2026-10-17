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

use sysmon_core::units::{TemperatureScale, Units};

pub const SECTION_KEYS: [&str; 6] = ["gpu", "memory", "cpu", "network", "disks", "sensors"];

/// Usage history (3.5): hours stay hourly gzip files for a week, then
/// whole days are packed losslessly into the archive, and nothing is
/// deleted unless the user sets a keep or a cap. Measured on this
/// machine (24 temperatures, 8 fans, 5 disks, 10 GPU programs a
/// sample): 3.4 KB a sample raw, 141 B gzip-sealed, 103 B packed (with
/// its rollup). At the 2 s default that is ~135 MB a month, ~1.6 GB a
/// year; at 5 s ~54 MB a month, ~0.65 GB a year. With no cap the menu
/// warns past 1 GB (store::WARN_BYTES): ~7 months at 2 s, ~1.5 years
/// at 5 s. A cap deletes, so it stays opt-in. The 3.4 keys
/// (`history_retention_hours`, `history_max_mb`) are retired: their
/// 7 d / 256 MB were deletion limits, and a 3.4 file wrote them even
/// when nobody chose them, so they are not carried over.
pub const DEFAULT_HOT_HOURS: f64 = 168.0;
pub const HOT_CHOICES_HOURS: [f64; 4] = [24.0, 72.0, 168.0, 720.0];
/// 0 = forever.
pub const KEEP_CHOICES_DAYS: [f64; 3] = [0.0, 365.0, 90.0];
/// 0 = no cap.
pub const CAP_CHOICES_MB: [u64; 4] = [0, 1_000, 4_000, 16_000];

/// The copy range slider's stops, seconds (cut at the oldest data on
/// disk, which is the last stop).
pub const COPY_RANGE_STOPS: [f64; 15] = [
    300.0, 900.0, 1_800.0, 3_600.0, 7_200.0, 10_800.0, 21_600.0, 43_200.0, 86_400.0, 259_200.0, 604_800.0,
    2_592_000.0, 7_776_000.0, 15_552_000.0, 31_536_000.0,
];
/// The longest copy range a settings file may hold (10 years).
pub const MAX_COPY_RANGE_SECONDS: f64 = 315_360_000.0;

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
    /// "celsius" | "fahrenheit" | "both" (TemperatureScale ids); new
    /// in 3.1 — older files keep their Celsius.
    pub temperature_scale: String,
    pub visible_sections: HashMap<String, bool>,
    pub popped_out_sections: Vec<String>,
    pub window_width: i32,
    pub window_height: i32,
    /// Process-table sort choice (stable column ids from
    /// `SortColumn::id`); new in 2.2 — absent in older files.
    pub sort_column: String,
    pub sort_descending: bool,
    /// Process table: one row per application (thorium ×12) instead
    /// of one per process. New in 3.1.
    pub group_by_app: bool,
    /// Process table: the Inspector pane beside the table. New in 3.1.
    pub show_inspector: bool,
    /// Sensors card groups the user folded shut ("board", "drives"…).
    pub folded_sensor_groups: Vec<String>,
    /// The page the window opens on: "overview" | "processes". New in
    /// 3.2; 3.6 folded the Performance page into the Overview, so a
    /// saved "performance" opens the Overview.
    pub start_page: String,
    /// The Overview's CPU graph: "total" (one trace) | "threads" (a
    /// coloured line per thread, the default) | "grid" (a small graph
    /// per thread). New in 3.2 as auto/combined/per_thread; 3.6 maps
    /// combined → total and the rest → threads.
    pub cpu_graph_mode: String,
    /// Usage history on disk (`sysmon_core::history`): record while the
    /// window runs. New in 3.4.
    pub history_enabled: bool,
    /// Hours stay hourly files this long before their day is packed
    /// into the archive, hours (1 h – 90 d). New in 3.5.
    pub history_hot_hours: f64,
    /// Remove whole days older than this, days; 0 = keep forever (the
    /// default). New in 3.5.
    pub history_keep_days: f64,
    /// Remove the oldest data past this size, MB; 0 = no cap (the
    /// default; past 1 GB the menu warns instead). New in 3.5.
    pub history_cap_mb: u64,
    /// How much history a copy button includes, seconds (the pop-outs'
    /// slider, up to the oldest data on disk). New in 3.4.
    pub copy_range_seconds: f64,
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
            temperature_scale: TemperatureScale::Celsius.id().to_string(),
            visible_sections: SECTION_KEYS.iter().map(|k| (k.to_string(), true)).collect(),
            popped_out_sections: Vec::new(),
            window_width: 430,
            window_height: 780,
            sort_column: "cpu".to_string(),
            sort_descending: true,
            group_by_app: false,
            show_inspector: true,
            folded_sensor_groups: Vec::new(),
            start_page: "overview".to_string(),
            cpu_graph_mode: "threads".to_string(),
            history_enabled: true,
            history_hot_hours: DEFAULT_HOT_HOURS,
            history_keep_days: 0.0,
            history_cap_mb: 0,
            copy_range_seconds: 3_600.0,
        }
    }
}

/// The presentation choices every renderer needs, resolved once per
/// frame (instead of re-deriving `Units` from a bool in five places).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display {
    pub units: Units,
    pub temperature: TemperatureScale,
}

impl Default for Display {
    fn default() -> Self {
        Display {
            units: Units::Decimal,
            temperature: TemperatureScale::Celsius,
        }
    }
}

/// A key of the wrong JSON type (a number where a string goes, a
/// list where a bool goes) would fail the whole typed parse and
/// throw away every other preference, and the next save would then
/// overwrite the file with defaults (reviewer R3). Drop only that key:
/// the container default fills it in.
fn drop_wrongly_typed_keys(raw: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(mut map) = raw else {
        return raw;
    };
    let shape = serde_json::to_value(Settings::default()).unwrap_or_default();
    if let serde_json::Value::Object(shape) = shape {
        map.retain(|key, value| match shape.get(key) {
            Some(expected) => {
                std::mem::discriminant(expected) == std::mem::discriminant(value)
                    // A float field accepts an integer; an integer field
                    // (window size) only an integer.
                    && (!expected.is_number() || expected.is_f64() || value.is_i64() || value.is_u64())
            }
            None => true, // unknown keys are serde's business (ignored)
        });
    }
    serde_json::Value::Object(map)
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
        let raw = drop_wrongly_typed_keys(raw);
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
        // An edited/future sort id falls back gently (the app also
        // validates through SortColumn::from_id at startup).
        if super::processes::SortColumn::from_id(&settings.sort_column).is_none() {
            settings.sort_column = Settings::default().sort_column;
        }
        if TemperatureScale::from_id(&settings.temperature_scale).is_none() {
            settings.temperature_scale = Settings::default().temperature_scale;
        }
        // 3.6: the Performance page lives inside the Overview now, and
        // the CPU graph's views were renamed. 3.2's "combined" is the
        // whole-CPU trace (total); "auto" and "per_thread" become the
        // coloured thread lines Ben asked for. Junk falls back the same
        // way instead of leaving the app on no page.
        if !matches!(settings.start_page.as_str(), "overview" | "processes") {
            settings.start_page = Settings::default().start_page;
        }
        settings.cpu_graph_mode = match settings.cpu_graph_mode.as_str() {
            "combined" => "total".to_string(),
            mode if super::overview::CpuGraphMode::from_id(mode).is_some() => mode.to_string(),
            _ => Settings::default().cpu_graph_mode,
        };
        if !settings.history_hot_hours.is_finite() {
            settings.history_hot_hours = DEFAULT_HOT_HOURS;
        }
        settings.history_hot_hours = settings.history_hot_hours.clamp(1.0, 2_160.0);
        if !settings.history_keep_days.is_finite() || settings.history_keep_days < 0.0 {
            settings.history_keep_days = 0.0;
        }
        if settings.history_keep_days > 0.0 {
            // A keep shorter than the hourly window would delete hours
            // before they could be packed: never shorter than it.
            settings.history_keep_days = settings.history_keep_days.max(settings.history_hot_hours / 24.0);
        }
        if settings.history_cap_mb > 0 {
            settings.history_cap_mb = settings.history_cap_mb.max(32);
        }
        if !settings.copy_range_seconds.is_finite() {
            settings.copy_range_seconds = Settings::default().copy_range_seconds;
        }
        settings.copy_range_seconds = settings.copy_range_seconds.clamp(60.0, MAX_COPY_RANGE_SECONDS);
        Some(settings)
    }

    /// Units + temperature scale, as the renderers want them.
    pub fn display(&self) -> Display {
        Display {
            units: if self.use_binary_units {
                Units::Binary
            } else {
                Units::Decimal
            },
            temperature: TemperatureScale::from_id(&self.temperature_scale).unwrap_or_default(),
        }
    }

    pub fn set_temperature_scale(&mut self, scale: TemperatureScale) {
        self.temperature_scale = scale.id().to_string();
    }

    pub fn sensor_group_folded(&self, group: &str) -> bool {
        self.folded_sensor_groups.iter().any(|g| g == group)
    }

    pub fn toggle_sensor_group(&mut self, group: &str) {
        if self.sensor_group_folded(group) {
            self.folded_sensor_groups.retain(|g| g != group);
        } else {
            self.folded_sensor_groups.push(group.to_string());
        }
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

    /// The history limits these settings ask for.
    pub fn history_limits(&self) -> sysmon_core::history::Limits {
        sysmon_core::history::Limits::new(
            self.history_hot_hours * 3_600.0,
            (self.history_keep_days > 0.0).then_some(self.history_keep_days * 86_400.0),
            (self.history_cap_mb > 0).then_some(self.history_cap_mb * 1_000_000),
        )
    }

    /// The copy range, never longer than the history on disk (`span`:
    /// seconds from the oldest record to now, when known).
    pub fn copy_range(&self, span: Option<f64>) -> f64 {
        let longest = span.map_or(MAX_COPY_RANGE_SECONDS, |s| s.max(300.0));
        self.copy_range_seconds.min(longest).max(60.0)
    }

    /// The slider's stops: every stop shorter than the history on
    /// disk, then all of it as the last stop (a week when unknown).
    pub fn copy_range_stops(&self, span: Option<f64>) -> Vec<f64> {
        let longest = span.map_or(604_800.0, |s| s.max(300.0).ceil());
        let mut stops: Vec<f64> = COPY_RANGE_STOPS.iter().copied().filter(|s| *s < longest).collect();
        stops.push(longest);
        stops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3.1.0 file has neither key: it opens on the Overview with the
    /// thread lines. Edited junk falls back the same way instead of
    /// leaving the app on no page.
    #[test]
    fn a_3_1_file_opens_on_the_overview_and_junk_falls_back() {
        let from_3_1 = r#"{"settings_version":2,"theme_mode":"amoled","temperature_scale":"both"}"#;
        let settings = Settings::from_json(from_3_1).unwrap();
        assert_eq!(settings.start_page, "overview");
        assert_eq!(settings.cpu_graph_mode, "threads");
        assert_eq!(settings.theme_mode, "amoled");
        assert_eq!(settings.temperature_scale, "both");
        let junk = r#"{"settings_version":2,"start_page":"kitchen","cpu_graph_mode":"sideways"}"#;
        let settings = Settings::from_json(junk).unwrap();
        assert_eq!(settings.start_page, "overview");
        assert_eq!(settings.cpu_graph_mode, "threads");
        let kept = r#"{"settings_version":2,"start_page":"processes","cpu_graph_mode":"grid"}"#;
        let settings = Settings::from_json(kept).unwrap();
        assert_eq!(settings.start_page, "processes");
        assert_eq!(settings.cpu_graph_mode, "grid");
    }

    /// 3.6 folds the Performance page into the Overview: a 3.2–3.5
    /// file (Ben's own: start page Overview, per_thread) loads onto the
    /// new names without losing anything else.
    #[test]
    fn a_3_5_file_lands_on_the_folded_overview() {
        let ben = r#"{"settings_version":2,"theme_mode":"amoled","start_page":"overview","cpu_graph_mode":"per_thread"}"#;
        let settings = Settings::from_json(ben).unwrap();
        assert_eq!(settings.start_page, "overview");
        assert_eq!(settings.cpu_graph_mode, "threads", "per thread now means a coloured line per thread");
        assert_eq!(settings.theme_mode, "amoled");
        let on_performance = r#"{"settings_version":2,"start_page":"performance","cpu_graph_mode":"combined"}"#;
        let settings = Settings::from_json(on_performance).unwrap();
        assert_eq!(settings.start_page, "overview", "the Performance page is the Overview now");
        assert_eq!(settings.cpu_graph_mode, "total", "combined is the whole-CPU trace");
        let auto = r#"{"settings_version":2,"cpu_graph_mode":"auto"}"#;
        assert_eq!(Settings::from_json(auto).unwrap().cpu_graph_mode, "threads");
    }

    /// Reviewer R3: one wrongly *typed* key (a number where a string
    /// goes) must not throw away every other preference, and the
    /// next save must not overwrite the file with defaults.
    #[test]
    fn a_wrongly_typed_key_loses_only_itself() {
        let file = r#"{"settings_version":2,"theme_mode":"light","update_interval_seconds":1.0,
                       "start_page":42,"cpu_graph_mode":["x"],"use_binary_units":"yes","window_width":"wide"}"#;
        let settings = Settings::from_json(file).unwrap();
        assert_eq!(settings.theme_mode, "light", "the good keys survive");
        assert_eq!(settings.update_interval_seconds, 1.0);
        assert_eq!(settings.start_page, "overview", "the bad key falls back alone");
        assert_eq!(settings.cpu_graph_mode, "threads");
        assert!(!settings.use_binary_units);
        assert_eq!(settings.window_width, 430);
        // A float where an int goes is not an int: that key alone falls back.
        let settings = Settings::from_json(r#"{"settings_version":2,"theme_mode":"dark","window_width":500.5}"#).unwrap();
        assert_eq!(settings.window_width, 430);
        assert_eq!(settings.theme_mode, "dark");
        // An int where a float goes is fine.
        let settings = Settings::from_json(r#"{"settings_version":2,"update_interval_seconds":3}"#).unwrap();
        assert_eq!(settings.update_interval_seconds, 3.0);
    }

    /// 3.5: nothing old is deleted by default. A 3.4 file's deletion
    /// limits (7 d / 256 MB, written even when nobody chose them) are
    /// not carried over; an edited file stays bounded.
    #[test]
    fn history_settings_keep_everything_by_default_and_stay_bounded() {
        let from_3_4 = r#"{"settings_version":2,"theme_mode":"amoled","history_retention_hours":168.0,"history_max_mb":256}"#;
        let settings = Settings::from_json(from_3_4).unwrap();
        assert!(settings.history_enabled);
        assert_eq!(settings.history_hot_hours, DEFAULT_HOT_HOURS);
        let limits = settings.history_limits();
        assert_eq!((limits.keep_seconds, limits.max_bytes), (None, None), "keep forever, no cap");
        assert_eq!(limits.hot_seconds, 7.0 * 86_400.0);
        assert_eq!(settings.copy_range(None), 3_600.0);
        let junk = r#"{"settings_version":2,"history_hot_hours":0,"history_keep_days":-3,"history_cap_mb":5,"copy_range_seconds":1e12}"#;
        let settings = Settings::from_json(junk).unwrap();
        assert_eq!(settings.history_hot_hours, 1.0);
        assert_eq!(settings.history_keep_days, 0.0);
        assert_eq!(settings.history_cap_mb, 32);
        assert_eq!(settings.copy_range(Some(3_600.0)), 3_600.0, "never longer than what is on disk");
        assert_eq!(settings.copy_range_stops(Some(3_600.0)), vec![300.0, 900.0, 1_800.0, 3_600.0]);
        // The slider reaches the oldest record: 40 days on disk.
        let stops = Settings::default().copy_range_stops(Some(40.0 * 86_400.0));
        assert_eq!(stops.last(), Some(&(40.0 * 86_400.0)));
        assert!(stops.contains(&2_592_000.0));
        // A keep never undercuts the hourly window.
        let short = Settings::from_json(r#"{"settings_version":2,"history_hot_hours":720,"history_keep_days":7}"#).unwrap();
        assert_eq!(short.history_keep_days, 30.0);
    }

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
        // sort persistence is new in 2.2 — must default sanely.
        assert_eq!(settings.sort_column, "cpu");
        assert!(settings.sort_descending);
    }

    #[test]
    fn sort_preference_round_trips_and_validates() {
        let json = r#"{"settings_version": 2, "sort_column": "vram", "sort_descending": false}"#;
        let settings = Settings::from_json(json).expect("parses");
        assert_eq!(settings.sort_column, "vram");
        assert!(!settings.sort_descending);

        let junk = r#"{"settings_version": 2, "sort_column": "no_such_column"}"#;
        let settings = Settings::from_json(junk).expect("parses");
        assert_eq!(settings.sort_column, "cpu", "unknown sort id falls back");
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

    /// A 3.0 file (Ben's own) has no temperature_scale: it loads as
    /// Celsius, keeps everything else, and a bad id falls back.
    #[test]
    fn temperature_scale_is_additive_and_validated() {
        let v3_0 = r#"{"settings_version": 2, "theme_mode": "amoled", "use_binary_units": false}"#;
        let settings = Settings::from_json(v3_0).expect("3.0 file parses");
        assert_eq!(settings.display().temperature, TemperatureScale::Celsius);
        assert_eq!(settings.theme_mode, "amoled");
        assert!(settings.show_inspector, "new panes default on");

        let both = r#"{"settings_version": 2, "temperature_scale": "both"}"#;
        assert_eq!(
            Settings::from_json(both).unwrap().display().temperature,
            TemperatureScale::Both
        );
        let junk = r#"{"settings_version": 2, "temperature_scale": "rankine"}"#;
        assert_eq!(Settings::from_json(junk).unwrap().temperature_scale, "celsius");
    }
}
