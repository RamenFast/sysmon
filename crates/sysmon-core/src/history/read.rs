// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the history, both tiers at once. A reader never needs to
//! know where an hour lives: the hot tier's hourly files, the
//! archive's day files, or (for a moment during a repack) both. Every
//! hour's records are read from wherever they are, merged, sorted and
//! deduplicated by timestamp, so a crash mid-repack can only ever show
//! a record once.
//!
//! [`summarize_dir`] is the fast path for long ranges: an hour that
//! lies whole inside the range and inside one timeline block is merged
//! from its rollup instead of read record by record. The numbers are
//! the same (`summary.rs` explains why); `use_rollups = false` reads
//! every record, for checking exactly that.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::HistoryRecord;
use super::archive::{self, DayIndex};
use super::rollup::HourRollup;
use super::store::parse_hour_stamp;
use super::summary::{GapHistogram, Summarizer, Summary};

pub(crate) const OPEN_SUFFIX: &str = ".jsonl";
pub(crate) const SEALED_SUFFIX: &str = ".jsonl.gz";
/// A sealed hot hour's rollup: one JSON line, zstd-compressed (~5 KB
/// an hour instead of ~25 KB plain; it rides beside a ~100 KB hour).
pub(crate) const ROLLUP_SUFFIX: &str = ".rollup.zst";

/// What a packed sample costs against its gzip-sealed hour, measured
/// on this machine's real recording (four hours, 2026-10-10): zstd -19
/// frames are 0.65–0.68× the gzip size, and the day file with its
/// index and rollups 0.73×. Used only to estimate growth before the
/// first day is packed; after that the archive itself is measured.
pub const PACKED_PER_GZIP_ESTIMATE: f64 = 0.73;
/// And against raw lines (the open hour): gzip is 27× smaller.
const PACKED_PER_RAW_ESTIMATE: f64 = PACKED_PER_GZIP_ESTIMATE / 27.0;

/// One hourly file in the hot tier.
#[derive(Clone, Debug)]
pub(crate) struct HotFile {
    pub stamp: String,
    pub start: f64,
    pub path: PathBuf,
    pub sealed: bool,
    pub bytes: u64,
}

/// The hot tier's hourly files, oldest first; a sealed member before
/// the open file of its hour. Symlinks and directories are not ours.
pub(crate) fn list_hot(dir: &Path) -> Vec<HotFile> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<HotFile> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (stamp, sealed) = if let Some(stamp) = name.strip_suffix(SEALED_SUFFIX) {
                (stamp.to_string(), true)
            } else {
                (name.strip_suffix(OPEN_SUFFIX)?.to_string(), false)
            };
            let start = parse_hour_stamp(&stamp)?;
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            Some(HotFile { stamp, start, path: entry.path(), sealed, bytes })
        })
        .collect();
    files.sort_by(|a, b| a.start.total_cmp(&b.start).then(b.sealed.cmp(&a.sealed)));
    files
}

pub(crate) fn rollup_path(dir: &Path, stamp: &str) -> PathBuf {
    dir.join(format!("{stamp}{ROLLUP_SUFFIX}"))
}

/// A hot hour's rollup file, decoded (None: missing or unreadable).
pub(crate) fn read_rollup_file(path: &Path) -> Option<HourRollup> {
    let bytes = fs::read(path).ok()?;
    let line = zstd::bulk::decompress(&bytes, 4 << 20).ok()?;
    HourRollup::from_line(String::from_utf8_lossy(&line).trim_end())
}

/// Write a hot hour's rollup file (through a temp name, then renamed).
pub(crate) fn write_rollup_file(dir: &Path, stamp: &str, rollup: &HourRollup) -> std::io::Result<()> {
    let compressed = zstd::bulk::compress(rollup.to_line().as_bytes(), 19)?;
    let temporary = dir.join(format!(".{stamp}.rollup.tmp-{}", std::process::id()));
    fs::write(&temporary, compressed)?;
    fs::rename(&temporary, rollup_path(dir, stamp))
}

/// Hot-hour rollup files: (stamp, path, bytes).
pub(crate) fn list_rollup_files(dir: &Path) -> Vec<(String, PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let stamp = name.strip_suffix(ROLLUP_SUFFIX)?.to_string();
            parse_hour_stamp(&stamp)?;
            Some((stamp, entry.path(), entry.metadata().map(|m| m.len()).unwrap_or(0)))
        })
        .collect()
}

/// Records of a line buffer: (records, unreadable lines).
pub(crate) fn parse_lines(bytes: &[u8], from_ts: f64, to_ts: f64, out: &mut Vec<HistoryRecord>) -> usize {
    let mut skipped = 0;
    for line in bytes.split(|b| *b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<HistoryRecord>(line) {
            Ok(record) if record.ts >= from_ts && record.ts <= to_ts => out.push(record),
            Ok(_) => {}
            Err(_) => skipped += 1,
        }
    }
    skipped
}

/// One hot file's records in range: (unreadable lines). A gzip member
/// cut short by a crash keeps what decoded and counts the loss once.
pub(crate) fn read_hot_file(file: &HotFile, from_ts: f64, to_ts: f64, out: &mut Vec<HistoryRecord>) -> usize {
    let Ok(handle) = File::open(&file.path) else { return 0 };
    let reader: Box<dyn Read> =
        if file.sealed { Box::new(flate2::read::MultiGzDecoder::new(handle)) } else { Box::new(handle) };
    let mut skipped = 0;
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next() {
            None => break,
            Some(Ok(line)) => match serde_json::from_str::<HistoryRecord>(&line) {
                Ok(record) if record.ts >= from_ts && record.ts <= to_ts => out.push(record),
                Ok(_) => {}
                Err(_) if line.trim().is_empty() => {}
                Err(_) => skipped += 1,
            },
            Some(Err(_)) => {
                skipped += 1;
                break;
            }
        }
    }
    skipped
}

/// The first record of a hot file.
pub(crate) fn first_ts(path: &Path, sealed: bool) -> Option<f64> {
    let file = File::open(path).ok()?;
    let reader: Box<dyn Read> =
        if sealed { Box::new(flate2::read::MultiGzDecoder::new(file)) } else { Box::new(file) };
    BufReader::new(reader)
        .lines()
        .map_while(Result::ok)
        .find_map(|line| serde_json::from_str::<HistoryRecord>(&line).ok())
        .map(|record| record.ts)
}

/// One archive day, its index and rollups loaded on demand.
pub(crate) struct Day {
    pub start: f64,
    pub path: PathBuf,
    pub bytes: u64,
    index: Option<Option<DayIndex>>,
    rollups: Option<Vec<Option<HourRollup>>>,
    file: Option<File>,
}

impl Day {
    pub fn index(&mut self) -> Option<&DayIndex> {
        if self.index.is_none() {
            self.index = Some(archive::read_index(&self.path).ok());
        }
        self.index.as_ref().and_then(Option::as_ref)
    }
}

/// Where one hour's records live.
#[derive(Clone, Debug, Default)]
pub(crate) struct HourPlan {
    /// (day, entry) in the archive.
    pub archive: Option<(usize, usize)>,
    /// Indexes into the hot list.
    pub hot: Vec<usize>,
}

/// Both tiers of a directory, listed.
pub(crate) struct Tiers {
    pub dir: PathBuf,
    pub hot: Vec<HotFile>,
    pub days: Vec<Day>,
}

impl Tiers {
    pub fn list(dir: &Path) -> Tiers {
        Tiers {
            dir: dir.to_path_buf(),
            hot: list_hot(dir),
            days: archive::list_days(dir)
                .into_iter()
                .map(|(start, _, path, bytes)| Day { start, path, bytes, index: None, rollups: None, file: None })
                .collect(),
        }
    }

    /// Every hour with data overlapping [from, to], oldest first.
    pub fn hours(&mut self, from_ts: f64, to_ts: f64) -> Vec<HourPlan> {
        let mut plans: BTreeMap<i64, HourPlan> = BTreeMap::new();
        for (d, day) in self.days.iter_mut().enumerate() {
            if day.start + 86_400.0 <= from_ts || day.start > to_ts {
                continue;
            }
            let Some(index) = day.index() else { continue };
            for (e, entry) in index.hours.iter().enumerate() {
                if entry.start + 3_600.0 > from_ts && entry.start <= to_ts {
                    plans.entry(entry.start as i64).or_default().archive = Some((d, e));
                }
            }
        }
        for (h, file) in self.hot.iter().enumerate() {
            if file.start + 3_600.0 > from_ts && file.start <= to_ts {
                plans.entry(file.start as i64).or_default().hot.push(h);
            }
        }
        plans.into_values().collect()
    }

    /// An archived hour's raw lines.
    fn archived_lines(&mut self, d: usize, e: usize) -> std::io::Result<Vec<u8>> {
        let day = &mut self.days[d];
        let index = day.index().cloned().ok_or_else(|| std::io::Error::other("unreadable archive index"))?;
        if day.file.is_none() {
            day.file = Some(File::open(&day.path)?);
        }
        let file = day.file.as_mut().expect("just opened");
        archive::read_hour(file, &index, &index.hours[e])
    }

    /// One hour's records in range, oldest first, one per timestamp:
    /// (records, unreadable lines).
    pub fn read_hour(&mut self, plan: &HourPlan, from_ts: f64, to_ts: f64) -> (Vec<HistoryRecord>, usize) {
        let mut records = Vec::new();
        let mut skipped = 0;
        if let Some((d, e)) = plan.archive {
            match self.archived_lines(d, e) {
                Ok(lines) => skipped += parse_lines(&lines, from_ts, to_ts, &mut records),
                Err(_) => {
                    // A damaged frame: its lines are lost to this read,
                    // and counted, never silently dropped.
                    skipped += self.days[d].index().map_or(1, |i| i.hours[e].records as usize);
                }
            }
        }
        for h in &plan.hot {
            skipped += read_hot_file(&self.hot[*h], from_ts, to_ts, &mut records);
        }
        records.sort_by(|a, b| a.ts.total_cmp(&b.ts));
        records.dedup_by(|a, b| a.ts == b.ts);
        (records, skipped)
    }

    /// The hour's rollup, when one exists and matches its records.
    pub fn rollup(&mut self, plan: &HourPlan) -> Option<HourRollup> {
        match (plan.archive, plan.hot.as_slice()) {
            (Some((d, e)), []) => {
                let day = &mut self.days[d];
                let records = day.index()?.hours[e].records;
                if day.rollups.is_none() {
                    let index = day.index()?.clone();
                    day.rollups = Some(archive::read_rollups(&day.path, &index).unwrap_or_default());
                }
                let rollup = day.rollups.as_ref()?.get(e)?.as_ref()?;
                (rollup.samples == records).then(|| rollup.clone())
            }
            (None, [h]) if self.hot[*h].sealed => {
                let file = &self.hot[*h];
                let rollup = read_rollup_file(&rollup_path(&self.dir, &file.stamp))?;
                (rollup.source_bytes == file.bytes).then_some(rollup)
            }
            _ => None,
        }
    }
}

/// What a read covered.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReadStats {
    /// Lines that did not parse: a torn last line while a writer is
    /// mid-append, or a crash-truncated gzip tail. Reported, never
    /// silently dropped.
    pub skipped_lines: usize,
}

/// Summarize a directory's history over [from, to]: hours merged from
/// their rollups where that gives the same numbers, read record by
/// record elsewhere (everywhere, with `use_rollups = false`).
pub fn summarize_dir(dir: &Path, section: &str, from_ts: f64, to_ts: f64, blocks: usize, use_rollups: bool) -> Summary {
    let mut summarizer = Summarizer::new(section, from_ts, to_ts, blocks);
    let mut tiers = Tiers::list(dir);
    for plan in tiers.hours(from_ts, to_ts) {
        if use_rollups
            && let Some(rollup) = tiers.rollup(&plan)
            && summarizer.can_merge(&rollup)
        {
            summarizer.add_rollup(&rollup);
            continue;
        }
        let (records, skipped) = tiers.read_hour(&plan, from_ts, to_ts);
        summarizer.note_skipped(skipped);
        records.iter().for_each(|record| summarizer.add_record(record));
    }
    summarizer.finish()
}

/// Read-only access to a history directory (no lock needed).
pub struct Recording;

impl Recording {
    /// Every record with `from_ts <= ts <= to_ts`, oldest first, one per
    /// timestamp, from both tiers, handed to `each` one at a time (a
    /// year of samples is never all in memory at once).
    pub fn for_each(dir: &Path, from_ts: f64, to_ts: f64, mut each: impl FnMut(HistoryRecord)) -> ReadStats {
        let mut stats = ReadStats::default();
        let mut tiers = Tiers::list(dir);
        for plan in tiers.hours(from_ts, to_ts) {
            let (records, skipped) = tiers.read_hour(&plan, from_ts, to_ts);
            stats.skipped_lines += skipped;
            records.into_iter().for_each(&mut each);
        }
        stats
    }

    /// What is on disk: both tiers' sizes, the first and last record,
    /// and what a packed sample costs.
    pub fn info(dir: &Path) -> StoreInfo {
        info(dir, true)
    }

    /// The oldest record on disk, either tier (cheap: one index, or the
    /// first line of the oldest hour).
    pub fn oldest_ts(dir: &Path) -> Option<f64> {
        let mut tiers = Tiers::list(dir);
        let archived = tiers.days.first_mut().and_then(|d| d.index()).and_then(|i| i.hours.first()).map(|h| h.first_ts);
        let hot = tiers.hot.first().and_then(|f| first_ts(&f.path, f.sealed));
        match (archived, hot) {
            (Some(a), Some(h)) => Some(a.min(h)),
            (a, h) => a.or(h),
        }
    }
}

/// What is on disk.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StoreInfo {
    pub dir: PathBuf,
    /// Everything SysMon keeps there, both tiers.
    pub bytes: u64,
    pub files: usize,
    /// Hourly files (`.jsonl` while open, `.jsonl.gz` once sealed) and
    /// their rollups: the last `hot_seconds` of history.
    pub hot_bytes: u64,
    pub hot_files: usize,
    /// Day files (`archive/<day>.jsonl.zst`): everything older.
    pub archive_bytes: u64,
    pub archive_days: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_ts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_ts: Option<f64>,
    /// The cadence the newest hour was recorded at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cadence_seconds: Option<f64>,
    /// Bytes one sample costs once packed: measured on the newest
    /// archived day, else estimated from the newest sealed hour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packed_bytes_per_sample: Option<f64>,
    /// "archive" (measured) or "estimate" (no day packed yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packed_basis: Option<&'static str>,
}

/// How fast the archive grows if SysMon records around the clock at
/// `cadence_seconds`. The hot tier stays the size of its window, so
/// the archive's growth is the store's.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Growth {
    pub cadence_seconds: f64,
    pub bytes_per_sample: f64,
    pub per_month_bytes: f64,
    pub per_year_bytes: f64,
    /// "archive" or "estimate", from StoreInfo.
    pub basis: &'static str,
}

pub fn growth(info: &StoreInfo, cadence_seconds: f64) -> Option<Growth> {
    let per_sample = info.packed_bytes_per_sample?;
    if !(cadence_seconds.is_finite() && cadence_seconds > 0.0) {
        return None;
    }
    let per_hour = per_sample * 3_600.0 / cadence_seconds;
    Some(Growth {
        cadence_seconds,
        bytes_per_sample: per_sample,
        per_month_bytes: per_hour * 730.0,
        per_year_bytes: per_hour * 8_760.0,
        basis: info.packed_basis.unwrap_or("estimate"),
    })
}

/// `read_newest`: read the newest hour for its last record and cadence
/// (a writer that knows its own newest record skips it).
pub(crate) fn info(dir: &Path, read_newest: bool) -> StoreInfo {
    let mut tiers = Tiers::list(dir);
    let rollups = list_rollup_files(dir);
    let hot_bytes = tiers.hot.iter().map(|f| f.bytes).sum::<u64>() + rollups.iter().map(|r| r.2).sum::<u64>();
    let archive_bytes: u64 = tiers.days.iter().map(|d| d.bytes).sum();
    let mut info = StoreInfo {
        dir: dir.to_path_buf(),
        bytes: hot_bytes + archive_bytes,
        files: tiers.hot.len() + rollups.len() + tiers.days.len(),
        hot_bytes,
        hot_files: tiers.hot.len(),
        archive_bytes,
        archive_days: tiers.days.len(),
        ..Default::default()
    };

    let archive_oldest = tiers.days.first_mut().and_then(|d| d.index()).and_then(|i| i.hours.first()).map(|h| h.first_ts);
    let hot_oldest = tiers.hot.first().and_then(|f| first_ts(&f.path, f.sealed));
    info.oldest_ts = match (archive_oldest, hot_oldest) {
        (Some(a), Some(h)) => Some(a.min(h)),
        (a, h) => a.or(h),
    };

    // Density: the newest archived day, measured.
    if let Some(day) = tiers.days.last_mut() {
        let bytes = day.bytes;
        if let Some(index) = day.index() {
            let records: u64 = index.hours.iter().map(|h| h.records).sum();
            if records > 0 {
                info.packed_bytes_per_sample = Some(bytes as f64 / records as f64);
                info.packed_basis = Some("archive");
            }
        }
    }
    // Else the newest sealed hour with a rollup (it counts its samples).
    if info.packed_bytes_per_sample.is_none() {
        let estimate = tiers.hot.iter().rev().filter(|f| f.sealed).find_map(|file| {
            let rollup = read_rollup_file(&rollup_path(dir, &file.stamp))?;
            (rollup.source_bytes == file.bytes && rollup.samples > 0)
                .then(|| file.bytes as f64 * PACKED_PER_GZIP_ESTIMATE / rollup.samples as f64)
        });
        if estimate.is_some() {
            info.packed_bytes_per_sample = estimate;
            info.packed_basis = Some("estimate");
        }
    }

    if read_newest {
        let newest_hour = tiers.hours(f64::MIN, f64::MAX).pop();
        if let Some(plan) = newest_hour {
            let (records, _) = tiers.read_hour(&plan, f64::MIN, f64::MAX);
            info.newest_ts = records.last().map(|r| r.ts);
            let mut gaps = GapHistogram::default();
            records.windows(2).for_each(|pair| gaps.add(pair[1].ts - pair[0].ts));
            info.cadence_seconds = gaps.median_seconds();
            // Nothing sealed yet: estimate from the open hour's raw lines.
            if info.packed_bytes_per_sample.is_none() && !records.is_empty() {
                let raw: u64 = plan.hot.iter().map(|h| &tiers.hot[*h]).filter(|f| !f.sealed).map(|f| f.bytes).sum();
                if raw > 0 {
                    info.packed_bytes_per_sample = Some(raw as f64 * PACKED_PER_RAW_ESTIMATE / records.len() as f64);
                    info.packed_basis = Some("estimate");
                }
            }
        }
    }
    info
}
