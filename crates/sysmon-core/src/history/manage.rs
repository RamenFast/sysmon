// SPDX-License-Identifier: GPL-3.0-or-later
//! Backup and clear: the two things a person does to the whole
//! history at once.
//!
//! **Export** writes one `.tar.zst` of the history directory, laid out
//! so that `tar --zstd -xf FILE -C ~/.local/state/sysmon` puts it back
//! (stop the recorder first). It needs no lock: hourly files are read
//! first and archive days after, so an hour packed mid-export is
//! caught in its day file (packing writes the day before it removes
//! the hour). The tar is written beside the target and renamed into
//! place when complete.
//!
//! **Clear** removes only files the store itself names: hourly files,
//! their rollups, archive days, and its own temp files. Anything else
//! in the directory (a note, a stray backup) is left and counted, the
//! `.lock` stays (its holder may be writing), and nothing outside the
//! directory is ever touched: names are matched in the directory's own
//! listing, symlinks are skipped, and `archive/` is removed only when
//! empty.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::archive;
use super::read;
use super::store::{self, LOCK_FILE, OpenError};

/// What a clear removed.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClearReport {
    pub dir: PathBuf,
    pub removed_files: usize,
    pub removed_bytes: u64,
    /// Files in the directory the store does not name, left alone.
    pub left_unknown: usize,
}

/// What clearing would remove right now (the confirm step shows it).
pub fn clear_preview(dir: &Path) -> ClearReport {
    let mut report = ClearReport { dir: dir.to_path_buf(), ..Default::default() };
    for (_, bytes) in history_files(dir) {
        report.removed_files += 1;
        report.removed_bytes += bytes;
    }
    report.left_unknown = unknown_files(dir);
    report
}

/// `archive/` as a real directory inside `dir`. A symlink there (to
/// a backup drive, say) is not the store's to empty: listing through
/// it would reach files outside the history directory.
fn archive_is_ours(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join(archive::ARCHIVE_DIR)).is_ok_and(|m| m.is_dir())
}

/// The store's own files in `dir`: (path, bytes).
fn history_files(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut files: Vec<(PathBuf, u64)> = read::list_hot(dir).into_iter().map(|f| (f.path, f.bytes)).collect();
    files.extend(read::list_rollup_files(dir).into_iter().map(|(_, path, bytes)| (path, bytes)));
    if archive_is_ours(dir) {
        files.extend(archive::list_days(dir).into_iter().map(|(_, _, path, bytes)| (path, bytes)));
    }
    files.extend(temp_files(dir));
    files
}

/// Temp files a crashed write left: `.<stamp>.rollup.tmp-<pid>` and
/// `archive/.<day>.jsonl.zst.tmp-<pid>`.
fn temp_files(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    for (folder, suffix_check) in [(dir.to_path_buf(), ".rollup.tmp-"), (dir.join(archive::ARCHIVE_DIR), ".jsonl.zst.tmp-")] {
        if folder != dir && !archive_is_ours(dir) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else { continue };
            if name.starts_with('.') && name.contains(suffix_check) && entry.file_type().is_ok_and(|t| t.is_file()) {
                out.push((entry.path(), entry.metadata().map(|m| m.len()).unwrap_or(0)));
            }
        }
    }
    out
}

fn unknown_files(dir: &Path) -> usize {
    let known: std::collections::HashSet<PathBuf> = history_files(dir).into_iter().map(|(p, _)| p).collect();
    let mut count = 0;
    for folder in [dir.to_path_buf(), dir.join(archive::ARCHIVE_DIR)] {
        if folder != dir && !archive_is_ours(dir) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_archive_dir = path == dir.join(archive::ARCHIVE_DIR) && archive_is_ours(dir);
            if !known.contains(&path) && !is_archive_dir && entry.file_name() != LOCK_FILE {
                count += 1;
            }
        }
    }
    count
}

/// Remove the store's files (the caller holds the writer lock).
pub(crate) fn remove_history_files(dir: &Path) -> std::io::Result<ClearReport> {
    let mut report = ClearReport { dir: dir.to_path_buf(), ..Default::default() };
    let mut first_error = None;
    for (path, bytes) in history_files(dir) {
        // Belt and braces: only direct children of the dir or its archive/.
        let parent_ok = path.parent().is_some_and(|p| p == dir || p == dir.join(archive::ARCHIVE_DIR));
        if !parent_ok {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => {
                report.removed_files += 1;
                report.removed_bytes += bytes;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    let _ = fs::remove_dir(dir.join(archive::ARCHIVE_DIR)); // only if empty
    report.left_unknown = unknown_files(dir);
    match first_error {
        Some(error) => Err(error),
        None => Ok(report),
    }
}

/// Clear with no recorder running: take the writer lock for the
/// duration. `Busy` names whoever is recording (ask it instead).
pub fn clear_unlocked(dir: &Path) -> Result<ClearReport, OpenError> {
    if !dir.exists() {
        return Ok(ClearReport { dir: dir.to_path_buf(), ..Default::default() });
    }
    let _lock = store::take_lock(dir, "clear")?;
    remove_history_files(dir).map_err(OpenError::Io)
}

/// What an export wrote.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ExportReport {
    pub path: PathBuf,
    pub files: usize,
    /// The history's size on disk when read.
    pub source_bytes: u64,
    /// The .tar.zst's size.
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_ts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_ts: Option<f64>,
}

/// The name an export gets when only a directory is given.
pub fn default_export_name(now: f64) -> String {
    format!("sysmon-history-{}.tar.zst", archive::day_stamp(now))
}

fn append(builder: &mut tar::Builder<impl Write>, name: &str, bytes: &[u8], mtime: u64) -> std::io::Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_mtime(mtime);
    header.set_entry_type(tar::EntryType::Regular);
    builder.append_data(&mut header, name, bytes)
}

fn mtime_of(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Write the whole history of `dir` to `path` as a `.tar.zst`, entries
/// under `history/`. An existing `path` is replaced only with
/// `overwrite`. Returns what was written.
pub fn export(dir: &Path, path: &Path, overwrite: bool) -> std::io::Result<ExportReport> {
    if path.exists() && !overwrite {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", path.display())));
    }
    let info = read::info(dir, true);
    let file_name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "export".into());
    let temporary = path.with_file_name(format!(".{file_name}.partial-{}", std::process::id()));
    let result = (|| -> std::io::Result<ExportReport> {
        let output = File::create(&temporary)?;
        let encoder = zstd::stream::write::Encoder::new(output, 3)?;
        let mut builder = tar::Builder::new(encoder);
        let mut report = ExportReport { path: path.to_path_buf(), oldest_ts: info.oldest_ts, newest_ts: info.newest_ts, ..Default::default() };
        // Hourly files and rollups first, archive days after (see top).
        let mut hot: Vec<(PathBuf, String)> = read::list_hot(dir)
            .into_iter()
            .map(|f| {
                let name = f.path.file_name().unwrap_or_default().to_string_lossy().to_string();
                (f.path, name)
            })
            .collect();
        hot.extend(read::list_rollup_files(dir).into_iter().map(|(_, path, _)| {
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            (path, name)
        }));
        let mut included = std::collections::HashSet::new();
        for (source, name) in hot {
            let mut bytes = Vec::new();
            match File::open(&source).and_then(|mut f| f.read_to_end(&mut bytes)) {
                Ok(_) => {}
                // Packed or sealed since the listing: its data is in a
                // later file (the day, or the sealed hour picked up next).
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
            append(&mut builder, &format!("history/{name}"), &bytes, mtime_of(&source))?;
            included.insert(name);
            report.files += 1;
            report.source_bytes += bytes.len() as u64;
        }
        // An hour sealed while the export ran: its open file vanished
        // above, and its .jsonl.gz is new since the listing.
        for file in read::list_hot(dir).into_iter().filter(|f| f.sealed) {
            let name = file.path.file_name().unwrap_or_default().to_string_lossy().to_string();
            if included.contains(&name) {
                continue;
            }
            if let Ok(bytes) = fs::read(&file.path) {
                append(&mut builder, &format!("history/{name}"), &bytes, mtime_of(&file.path))?;
                report.files += 1;
                report.source_bytes += bytes.len() as u64;
            }
        }
        for (_, stamp, source, _) in archive::list_days(dir) {
            let mut bytes = Vec::new();
            match File::open(&source).and_then(|mut f| f.read_to_end(&mut bytes)) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
            append(&mut builder, &format!("history/{}/{stamp}{}", archive::ARCHIVE_DIR, archive::DAY_SUFFIX), &bytes, mtime_of(&source))?;
            report.files += 1;
            report.source_bytes += bytes.len() as u64;
        }
        let encoder = builder.into_inner()?;
        let output = encoder.finish()?;
        output.sync_all()?;
        report.bytes = output.metadata()?.len();
        Ok(report)
    })();
    match result {
        Ok(report) => {
            fs::rename(&temporary, path)?;
            Ok(report)
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::store::{Limits, PackScope, Recorder};
    use crate::history::{CpuRecord, HistoryRecord};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sysmon-manage-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn fill(dir: &Path) {
        let mut recorder = Recorder::open(dir, Limits::new(1e9, None, None), "test").unwrap();
        // Three hours on 2026-10-09 UTC, then pack them, then an open hour.
        for i in 0..5_400 {
            let ts = 1_791_504_000.0 + f64::from(i) * 2.0;
            recorder.record(&HistoryRecord { ts, interval_seconds: 2.0, cpu: Some(CpuRecord { busy_percent: (i % 97) as f32, ..Default::default() }), ..Default::default() }).unwrap();
        }
        recorder.record(&HistoryRecord { ts: 1_791_600_000.0, interval_seconds: 2.0, ..Default::default() }).unwrap();
        recorder.maintain_blocking(PackScope::AllSealed);
    }

    #[test]
    fn export_round_trips_through_tar_and_restores_the_same_history() {
        let dir = scratch("export");
        fill(&dir);
        let before: Vec<f64> = {
            let mut ts = Vec::new();
            crate::history::Recording::for_each(&dir, 0.0, f64::MAX, |r| ts.push(r.ts));
            ts
        };
        assert_eq!(before.len(), 5_401);
        let target = std::env::temp_dir().join(format!("sysmon-export-{}.tar.zst", std::process::id()));
        let _ = fs::remove_file(&target);
        let report = export(&dir, &target, false).unwrap();
        assert!(report.files >= 2 && report.bytes > 0, "{report:?}");
        assert!(export(&dir, &target, false).is_err(), "never replaces a file unasked");
        // Restore into a fresh state dir: the same records come back.
        let restore = scratch("restore");
        fs::create_dir_all(&restore).unwrap();
        let decoder = zstd::stream::read::Decoder::new(File::open(&target).unwrap()).unwrap();
        tar::Archive::new(decoder).unpack(&restore).unwrap();
        let mut after = Vec::new();
        crate::history::Recording::for_each(&restore.join("history"), 0.0, f64::MAX, |r| after.push(r.ts));
        assert_eq!(after, before);
        let _ = fs::remove_file(&target);
        let _ = fs::remove_dir_all(&restore);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn clear_removes_the_history_and_nothing_else() {
        let dir = scratch("clear");
        fill(&dir);
        let note = dir.join("my-notes.txt");
        fs::write(&note, "keep me").unwrap();
        let outside = std::env::temp_dir().join(format!("sysmon-outside-{}.jsonl.gz", std::process::id()));
        fs::write(&outside, "not ours").unwrap();
        // A symlink named like an hour, pointing outside: skipped.
        let link = dir.join("2026-01-01T00.jsonl.gz");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let preview = clear_preview(&dir);
        assert_eq!(preview.removed_files, 2, "the packed day and the open hour: {preview:?}");
        assert_eq!(preview.left_unknown, 2, "the note and the symlink");
        let report = clear_unlocked(&dir).unwrap();
        assert_eq!(report.removed_files, preview.removed_files);
        assert!(note.exists(), "an unknown file is left alone");
        assert!(outside.exists() && fs::symlink_metadata(&link).is_ok(), "a symlink and its target are left alone");
        assert!(!dir.join(archive::ARCHIVE_DIR).exists(), "the empty archive dir goes");
        assert!(dir.join(LOCK_FILE).exists(), "the lock stays");
        let mut left = 0;
        crate::history::Recording::for_each(&dir, 0.0, f64::MAX, |_| left += 1);
        assert_eq!(left, 0);
        // A running writer's clear goes through its own lock.
        let busy = Recorder::open(&dir, Limits::default(), "gui").unwrap();
        assert!(matches!(clear_unlocked(&dir), Err(OpenError::Busy(_))));
        drop(busy);
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(dir);
    }

    /// `archive/` as a symlink to somewhere else: Clear must not reach
    /// through it, even for files named exactly like day files.
    #[test]
    fn clear_never_reaches_through_a_symlinked_archive() {
        let dir = scratch("clear-link");
        fs::create_dir_all(&dir).unwrap();
        let elsewhere = scratch("clear-elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let day = elsewhere.join(format!("2026-10-01{}", archive::DAY_SUFFIX));
        fs::write(&day, "someone else's").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join(archive::ARCHIVE_DIR)).unwrap();
        let preview = clear_preview(&dir);
        assert_eq!(preview.removed_files, 0, "{preview:?}");
        clear_unlocked(&dir).unwrap();
        assert!(day.exists(), "a file behind the symlink is left alone");
        assert!(fs::symlink_metadata(dir.join(archive::ARCHIVE_DIR)).is_ok(), "the symlink itself stays");
        let _ = fs::remove_dir_all(&elsewhere);
        let _ = fs::remove_dir_all(dir);
    }
}
