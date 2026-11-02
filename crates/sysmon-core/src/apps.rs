// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop-entry and icon-theme index: process → icon file on disk.
//!
//! Two halves:
//!   1. A `.desktop` index over every applications dir (XDG data
//!      dirs, the user's local share, flatpak exports), keyed by the
//!      things a process actually exposes — exe basename, comm
//!      (15-char truncated), desktop-id and its last dot-component,
//!      StartupWMClass — each mapping to the entry's `Icon=`.
//!   2. A freedesktop icon lookup that honors the configured icon
//!      theme and its full `Inherits` chain (on Ben's machine:
//!      Blossom → Papirus-Dark → Mint-Y → Adwaita → gnome →
//!      hicolor), picking the best-sized png/svg/xpm.
//!
//! Before either, a process-icons folder
//! (`$XDG_DATA_HOME/sysmon/process-icons`, else
//! `~/.local/share/sysmon/process-icons`) can name a process's icon
//! by the process's own name: `nexus-instinct-server.svg` for the
//! `nexus-instinct-server.py` a python3.12 runs. Scripts have no
//! desktop entry, and their exe and comm name only the interpreter,
//! so without the folder every script wears the same icon.
//!
//! Everything returns *paths*; rasterizing is the GUI's business.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One directory inside an icon theme, with its size metadata.
#[derive(Clone, Debug)]
struct ThemeDirectory {
    /// Absolute path: <theme root>/<subdir>.
    path: PathBuf,
    size: u32,
    scale: u32,
    kind: DirectoryKind,
    min_size: u32,
    max_size: u32,
    threshold: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DirectoryKind {
    Fixed,
    Scalable,
    Threshold,
}

impl ThemeDirectory {
    fn matches(&self, size: u32, scale: u32) -> bool {
        if self.scale != scale {
            return false;
        }
        match self.kind {
            DirectoryKind::Fixed => self.size == size,
            DirectoryKind::Scalable => self.min_size <= size && size <= self.max_size,
            DirectoryKind::Threshold => {
                self.size.saturating_sub(self.threshold) <= size
                    && size <= self.size + self.threshold
            }
        }
    }

    fn distance(&self, size: u32, scale: u32) -> u32 {
        let wanted = size * scale;
        let have = self.size * self.scale;
        match self.kind {
            DirectoryKind::Scalable => {
                let min = self.min_size * self.scale;
                let max = self.max_size * self.scale;
                if wanted < min {
                    min - wanted
                } else { wanted.saturating_sub(max) }
            }
            _ => wanted.abs_diff(have),
        }
    }
}

pub struct AppIndex {
    /// lowercase key → icon name (or absolute path) from .desktop.
    icon_name_by_key: HashMap<String, String>,
    /// Theme chain, most specific first; each entry is that theme's
    /// directory list.
    theme_directories: Vec<ThemeDirectory>,
    /// Last-resort flat dirs (pixmaps).
    fallback_directories: Vec<PathBuf>,
    /// icon name → resolved path (misses cached as None).
    resolution_cache: std::sync::Mutex<HashMap<(String, u32), Option<PathBuf>>>,
    /// Folders of icons named after the process itself, checked
    /// before the desktop entries and the theme.
    process_icon_dirs: Vec<PathBuf>,
}

/// Script extensions dropped from a display name when a
/// process-icons folder is searched (`portal-server.py` →
/// `portal-server`).
const SCRIPT_EXTENSIONS: [&str; 10] = [
    "py", "sh", "bash", "jar", "js", "mjs", "cjs", "ts", "pl", "rb",
];

impl AppIndex {
    /// Build from the real system: XDG dirs + the configured
    /// Cinnamon/GNOME icon theme.
    pub fn load() -> Self {
        let application_dirs = default_application_dirs();
        let icon_roots = default_icon_roots();
        let theme = configured_icon_theme().unwrap_or_else(|| "hicolor".to_string());
        Self::load_with(&application_dirs, &icon_roots, &theme)
            .with_process_icon_dirs(default_process_icon_dirs())
    }

    /// Folders searched first, by the process's own name (see
    /// `icon_for_named_process`). `load_with` starts with none.
    pub fn with_process_icon_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.process_icon_dirs = dirs;
        self
    }

    pub fn load_with(
        application_dirs: &[PathBuf],
        icon_roots: &[PathBuf],
        theme_name: &str,
    ) -> Self {
        let mut icon_name_by_key = HashMap::new();
        for dir in application_dirs {
            index_desktop_files(dir, &mut icon_name_by_key);
        }

        // Walk the inherit chain breadth-first, hicolor always last.
        let mut theme_directories = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = vec![theme_name.to_string()];
        while let Some(theme) = queue.pop() {
            if !visited.insert(theme.clone()) {
                continue;
            }
            for root in icon_roots {
                let theme_root = root.join(&theme);
                let index_path = theme_root.join("index.theme");
                let Ok(content) = fs::read_to_string(&index_path) else {
                    continue;
                };
                let (directories, inherits) = parse_index_theme(&content, &theme_root);
                theme_directories.extend(directories);
                // Depth-first via a stack keeps specific themes first
                // closely enough for icon purposes; push parents in
                // reverse so the first parent is visited next.
                for parent in inherits.into_iter().rev() {
                    queue.push(parent);
                }
                break; // first root containing the theme wins
            }
        }
        if !visited.contains("hicolor") {
            for root in icon_roots {
                let theme_root = root.join("hicolor");
                if let Ok(content) = fs::read_to_string(theme_root.join("index.theme")) {
                    let (directories, _) = parse_index_theme(&content, &theme_root);
                    theme_directories.extend(directories);
                    break;
                }
            }
        }

        AppIndex {
            icon_name_by_key,
            theme_directories,
            fallback_directories: vec![PathBuf::from("/usr/share/pixmaps")],
            resolution_cache: std::sync::Mutex::new(HashMap::new()),
            process_icon_dirs: Vec::new(),
        }
    }

    /// Best icon file for a process as a person names it: a file in a
    /// process-icons folder named after the process wins (stems from
    /// `process_icon_names`, `.svg` then `.png`); otherwise the
    /// desktop-entry and theme lookup of `icon_for_process`.
    pub fn icon_for_named_process(
        &self,
        display_name: &str,
        comm: &str,
        exe_basename: Option<&str>,
        command_line: &str,
        size: u32,
    ) -> Option<PathBuf> {
        if !self.process_icon_dirs.is_empty() {
            for name in process_icon_names(display_name, comm, exe_basename) {
                for dir in &self.process_icon_dirs {
                    for extension in ["svg", "png"] {
                        let candidate = dir.join(format!("{name}.{extension}"));
                        if candidate.is_file() {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
        self.icon_for_process(comm, exe_basename, command_line, size)
    }

    /// Best icon file for a process, or None (caller draws its
    /// generic glyph). Keys are tried most-specific-first.
    pub fn icon_for_process(
        &self,
        comm: &str,
        exe_basename: Option<&str>,
        command_line: &str,
        size: u32,
    ) -> Option<PathBuf> {
        let mut keys: Vec<String> = Vec::new();
        if let Some(exe) = exe_basename {
            keys.push(exe.to_lowercase());
        }
        keys.push(comm.to_lowercase());
        if let Some(first) = command_line.split_ascii_whitespace().next() {
            let basename = first.rsplit('/').next().unwrap_or(first);
            keys.push(basename.to_lowercase());
        }

        for key in &keys {
            if key.is_empty() {
                continue;
            }
            if let Some(icon_name) = self.icon_name_by_key.get(key)
                && let Some(path) = self.icon_path(icon_name, size)
            {
                return Some(path);
            }
        }
        // No desktop entry — some binaries ship a theme icon under
        // their own name (htop, vim, …).
        for key in &keys {
            if !key.is_empty()
                && let Some(path) = self.icon_path(key, size)
            {
                return Some(path);
            }
        }
        None
    }

    /// Resolve an icon name (or absolute path) to a file, preferring
    /// an exact size match in the most specific theme.
    pub fn icon_path(&self, icon_name: &str, size: u32) -> Option<PathBuf> {
        if icon_name.starts_with('/') {
            let path = PathBuf::from(icon_name);
            return path.exists().then_some(path);
        }
        let cache_key = (icon_name.to_string(), size);
        if let Ok(cache) = self.resolution_cache.lock()
            && let Some(cached) = cache.get(&cache_key)
        {
            return cached.clone();
        }

        let resolved = self.lookup_uncached(icon_name, size);
        if let Ok(mut cache) = self.resolution_cache.lock() {
            cache.insert(cache_key, resolved.clone());
        }
        resolved
    }

    fn lookup_uncached(&self, icon_name: &str, size: u32) -> Option<PathBuf> {
        const EXTENSIONS: [&str; 3] = ["svg", "png", "xpm"];
        // Desktop files sometimes write "name.png" — strip it.
        let name = icon_name
            .strip_suffix(".png")
            .or_else(|| icon_name.strip_suffix(".svg"))
            .or_else(|| icon_name.strip_suffix(".xpm"))
            .unwrap_or(icon_name);

        // Pass 1: a directory that *matches* the requested size.
        for directory in &self.theme_directories {
            if directory.matches(size, 1) {
                for extension in EXTENSIONS {
                    let candidate = directory.path.join(format!("{name}.{extension}"));
                    if candidate.exists() {
                        return Some(candidate);
                    }
                }
            }
        }
        // Pass 2: closest size anywhere in the chain.
        let mut best: Option<(u32, PathBuf)> = None;
        for directory in &self.theme_directories {
            let distance = directory.distance(size, 1);
            if let Some((best_distance, _)) = &best
                && distance >= *best_distance
            {
                continue;
            }
            for extension in EXTENSIONS {
                let candidate = directory.path.join(format!("{name}.{extension}"));
                if candidate.exists() {
                    best = Some((distance, candidate));
                    break;
                }
            }
        }
        if let Some((_, path)) = best {
            return Some(path);
        }
        // Pass 3: pixmaps.
        for directory in &self.fallback_directories {
            for extension in EXTENSIONS {
                let candidate = directory.join(format!("{name}.{extension}"));
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        None
    }
}

/// The file stems a process-icons folder is searched for, most
/// specific first, lowercase, no repeats: the display name; it without
/// a script extension; that without a `-<version>` tail; then the exe
/// basename and comm. A name that could leave the folder (a `/`, a
/// leading `.`) is skipped.
///
/// `foxloader-2.0-alpha39-server.jar` (exe `java`) →
/// `foxloader-2.0-alpha39-server.jar`, `foxloader-2.0-alpha39-server`,
/// `foxloader`, `java`.
pub fn process_icon_names(
    display_name: &str,
    comm: &str,
    exe_basename: Option<&str>,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        let name = name.trim().to_lowercase();
        if !name.is_empty()
            && !name.contains('/')
            && !name.starts_with('.')
            && !names.contains(&name)
        {
            names.push(name);
        }
    };
    let display = display_name.trim().to_lowercase();
    push(&display);
    let stem = match display.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && SCRIPT_EXTENSIONS.contains(&extension) => {
            stem
        }
        _ => display.as_str(),
    };
    push(stem);
    if let Some(unversioned) = without_version_tail(stem) {
        push(unversioned);
    }
    if let Some(exe) = exe_basename {
        push(exe);
    }
    push(comm);
    names
}

/// `foxloader-2.0-alpha39-server` → `foxloader`: cut at the first `-`
/// or `_` that a digit follows. None when there is no such tail.
fn without_version_tail(name: &str) -> Option<&str> {
    let bytes = name.as_bytes();
    (1..bytes.len().saturating_sub(1))
        .find(|&i| (bytes[i] == b'-' || bytes[i] == b'_') && bytes[i + 1].is_ascii_digit())
        .map(|i| &name[..i])
}

/// `$XDG_DATA_HOME/sysmon/process-icons`, else
/// `~/.local/share/sysmon/process-icons`.
fn default_process_icon_dirs() -> Vec<PathBuf> {
    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        });
    data_home
        .map(|dir| vec![dir.join("sysmon").join("process-icons")])
        .unwrap_or_default()
}

/// Parse one index.theme: (directories with metadata, inherits).
fn parse_index_theme(content: &str, theme_root: &Path) -> (Vec<ThemeDirectory>, Vec<String>) {
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current = line[1..line.len() - 1].to_string();
        } else if let Some((key, value)) = line.split_once('=') {
            sections
                .entry(current.clone())
                .or_default()
                .insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    let main = sections.get("Icon Theme").cloned().unwrap_or_default();
    let inherits: Vec<String> = main
        .get("Inherits")
        .map(|list| {
            list.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let mut directories = Vec::new();
    let listed = main
        .get("Directories")
        .map(String::as_str)
        .unwrap_or_default();
    for subdir in listed.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let metadata = sections.get(subdir).cloned().unwrap_or_default();
        let number = |key: &str, default: u32| -> u32 {
            metadata
                .get(key)
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let size = number("Size", 0);
        if size == 0 {
            continue;
        }
        let kind = match metadata.get("Type").map(String::as_str) {
            Some("Fixed") => DirectoryKind::Fixed,
            Some("Scalable") => DirectoryKind::Scalable,
            _ => DirectoryKind::Threshold,
        };
        directories.push(ThemeDirectory {
            path: theme_root.join(subdir),
            size,
            scale: number("Scale", 1),
            kind,
            min_size: number("MinSize", size),
            max_size: number("MaxSize", size),
            threshold: number("Threshold", 2),
        });
    }
    (directories, inherits)
}

/// Index every .desktop entry in a directory (recursively — some
/// distros nest) into key → Icon.
fn index_desktop_files(dir: &Path, index: &mut HashMap<String, String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            index_desktop_files(&path, index);
            continue;
        }
        if path.extension().map(|e| e != "desktop").unwrap_or(true) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(parsed) = parse_desktop_entry(&content) else {
            continue;
        };
        let desktop_id = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let mut keys: Vec<String> = Vec::new();
        if let Some(exec_basename) = &parsed.exec_basename {
            keys.push(exec_basename.clone());
            // comm is kernel-truncated to 15 chars.
            if exec_basename.len() > 15 {
                keys.push(exec_basename[..15].to_string());
            }
        }
        if let Some(wm_class) = &parsed.startup_wm_class {
            keys.push(wm_class.to_lowercase());
        }
        if !desktop_id.is_empty() {
            keys.push(desktop_id.clone());
            // com.discordapp.Discord → discord
            if let Some(last) = desktop_id.rsplit('.').next()
                && last != desktop_id
            {
                keys.push(last.to_string());
            }
        }
        for key in keys {
            if !key.is_empty() {
                index.entry(key).or_insert_with(|| parsed.icon.clone());
            }
        }
    }
}

struct DesktopEntry {
    icon: String,
    exec_basename: Option<String>,
    startup_wm_class: Option<String>,
}

/// Minimal [Desktop Entry] parse: Icon (required for us), Exec's real
/// command basename (skipping `env` and VAR=val prefixes; for
/// `flatpak run … app.id` the app id's last component), and
/// StartupWMClass.
fn parse_desktop_entry(content: &str) -> Option<DesktopEntry> {
    let mut in_main_group = false;
    let mut icon = None;
    let mut exec = None;
    let mut wm_class = None;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_main_group {
            continue;
        }
        if let Some(value) = line.strip_prefix("Icon=") {
            icon = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Exec=") {
            exec = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("StartupWMClass=") {
            wm_class = Some(value.trim().to_string());
        }
    }
    let icon = icon?;

    let exec_basename = exec.as_deref().and_then(exec_command_basename);
    Some(DesktopEntry {
        icon,
        exec_basename,
        startup_wm_class: wm_class,
    })
}

/// Split an Exec value into tokens, honoring double quotes (the
/// desktop spec's quoting for paths with spaces).
fn exec_tokens(exec: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for character in exec.chars() {
        match character {
            '"' => in_quotes = !in_quotes,
            c if c.is_ascii_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// The identifying basename of an Exec line.
pub fn exec_command_basename(exec: &str) -> Option<String> {
    let all_tokens = exec_tokens(exec);
    let mut tokens = all_tokens.iter().map(String::as_str);
    let mut command = tokens.next()?;
    // Skip wrappers: env with assignments, sh -c is hopeless, flatpak.
    let command_name = |c: &str| c.rsplit('/').next().unwrap_or(c).to_string();
    if command_name(command) == "env" {
        for token in tokens.by_ref() {
            if !token.contains('=') {
                command = token;
                break;
            }
        }
    } else if command_name(command) == "flatpak" {
        // flatpak run [--options…] the.app.Id [args…]
        for token in tokens {
            if token == "run" || token.starts_with("--") {
                continue;
            }
            return token.rsplit('.').next().map(str::to_lowercase);
        }
        return None;
    }
    let base = command_name(command).to_lowercase();
    (!base.is_empty()).then_some(base)
}

fn default_application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var("HOME").unwrap_or_default();
    if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(xdg_data_home).join("applications"));
    } else if !home.is_empty() {
        dirs.push(PathBuf::from(&home).join(".local/share/applications"));
    }
    let xdg_data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for dir in xdg_data_dirs.split(':').filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(dir).join("applications"));
    }
    // Flatpak exports, in case XDG_DATA_DIRS doesn't carry them.
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    if !home.is_empty() {
        dirs.push(PathBuf::from(&home).join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

fn default_icon_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        roots.push(PathBuf::from(&home).join(".icons"));
        roots.push(PathBuf::from(&home).join(".local/share/icons"));
    }
    roots.push(PathBuf::from("/usr/share/icons"));
    roots
}

/// The configured icon theme, asked of Cinnamon first, GNOME second.
fn configured_icon_theme() -> Option<String> {
    for schema in [
        "org.cinnamon.desktop.interface",
        "org.gnome.desktop.interface",
    ] {
        if let Ok(output) = Command::new("gsettings")
            .args(["get", schema, "icon-theme"])
            .output()
            && output.status.success()
        {
            let raw = String::from_utf8_lossy(&output.stdout);
            let name = raw.trim().trim_matches('\'').trim_matches('"').to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_basename_handles_wrappers() {
        assert_eq!(
            exec_command_basename("/usr/bin/google-chrome %U").as_deref(),
            Some("google-chrome")
        );
        assert_eq!(
            exec_command_basename("env FOO=bar /opt/thing/Thing --flag").as_deref(),
            Some("thing")
        );
        assert_eq!(
            exec_command_basename(
                "/usr/bin/flatpak run --branch=stable --arch=x86_64 com.discordapp.Discord"
            )
            .as_deref(),
            Some("discord")
        );
        assert_eq!(
            exec_command_basename("\"/opt/My App/run\" --now").as_deref(),
            Some("run")
        );
    }

    #[test]
    fn desktop_entry_parse_reads_main_group_only() {
        let content = "[Desktop Entry]\nName=Test\nIcon=test-icon\n\
                       Exec=/usr/bin/testapp --flag %F\nStartupWMClass=TestApp\n\
                       [Desktop Action new]\nIcon=other\n";
        let entry = parse_desktop_entry(content).expect("parses");
        assert_eq!(entry.icon, "test-icon");
        assert_eq!(entry.exec_basename.as_deref(), Some("testapp"));
        assert_eq!(entry.startup_wm_class.as_deref(), Some("TestApp"));
    }

    #[test]
    fn process_icon_names_say_the_process_not_the_interpreter() {
        assert_eq!(
            process_icon_names("nexus-instinct-server.py", "python", Some("python3.12")),
            vec!["nexus-instinct-server.py", "nexus-instinct-server", "python3.12", "python"]
        );
        assert_eq!(
            process_icon_names("foxloader-2.0-alpha39-server.jar", "java", Some("java")),
            vec![
                "foxloader-2.0-alpha39-server.jar",
                "foxloader-2.0-alpha39-server",
                "foxloader",
                "java"
            ]
        );
        // A truncated comm's display name is the whole exe name.
        assert_eq!(
            process_icon_names("nexus-ear-server", "nexus-ear-serve", Some("nexus-ear-server")),
            vec!["nexus-ear-server", "nexus-ear-serve"]
        );
        // Version-looking names that are not a tail stay whole.
        assert_eq!(process_icon_names("python3.12", "python3", None), vec!["python3.12", "python3"]);
        assert_eq!(process_icon_names("Screen-Sense", "python3", None), vec!["screen-sense", "python3"]);
        // Nothing that could leave the folder.
        assert_eq!(process_icon_names("kworker/0:1", "kworker/0:1", None), Vec::<String>::new());
        assert_eq!(process_icon_names(".hidden", "..", None), Vec::<String>::new());
    }

    #[test]
    fn a_process_icon_folder_wins_by_the_process_name() {
        let root =
            std::env::temp_dir().join(format!("sysmon-process-icons-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("process-icons");
        let applications = root.join("applications");
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(&applications).unwrap();
        // What the interpreter would wear without the folder.
        let python_icon = root.join("python.svg");
        fs::write(&python_icon, "<svg/>").unwrap();
        fs::write(
            applications.join("python3.12.desktop"),
            format!(
                "[Desktop Entry]\nExec=/usr/bin/python3.12\nIcon={}\n",
                python_icon.display()
            ),
        )
        .unwrap();
        fs::write(folder.join("nexus-instinct-server.svg"), "<svg/>").unwrap();
        fs::write(folder.join("foxloader.png"), "png").unwrap();

        let bare = AppIndex::load_with(std::slice::from_ref(&applications), &[], "hicolor");
        let index = AppIndex::load_with(&[applications], &[], "hicolor")
            .with_process_icon_dirs(vec![folder.clone()]);
        let cmdline = "/usr/bin/python3 /srv/nexus-instinct-server.py";

        let instinct = |index: &AppIndex| {
            index.icon_for_named_process(
                "nexus-instinct-server.py",
                "python",
                Some("python3.12"),
                cmdline,
                32,
            )
        };
        assert_eq!(
            instinct(&bare),
            Some(python_icon.clone()),
            "without the folder the script wears its interpreter's icon"
        );
        assert_eq!(instinct(&index), Some(folder.join("nexus-instinct-server.svg")));
        assert_eq!(
            index.icon_for_named_process(
                "foxloader-2.0-alpha39-server.jar",
                "java",
                Some("java"),
                "java -jar x.jar",
                32,
            ),
            Some(folder.join("foxloader.png"))
        );
        // No file of its own: the old lookup still answers.
        assert_eq!(
            index.icon_for_named_process("station-helper.py", "python3", Some("python3.12"), cmdline, 32),
            Some(python_icon)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn index_theme_parses_directories_and_inherits() {
        let content = "[Icon Theme]\nName=T\nInherits=parent-a,parent-b\n\
                       Directories=16x16/apps,scalable/apps\n\n\
                       [16x16/apps]\nSize=16\nType=Fixed\n\n\
                       [scalable/apps]\nSize=128\nType=Scalable\nMinSize=8\nMaxSize=512\n";
        let (dirs, inherits) = parse_index_theme(content, Path::new("/tmp/theme"));
        assert_eq!(inherits, vec!["parent-a", "parent-b"]);
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].matches(16, 1));
        assert!(!dirs[0].matches(32, 1));
        assert!(dirs[1].matches(32, 1)); // scalable covers 8..512
        assert_eq!(dirs[1].distance(32, 1), 0);
    }
}
