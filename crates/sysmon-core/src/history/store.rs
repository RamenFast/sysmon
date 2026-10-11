// SPDX-License-Identifier: GPL-3.0-or-later
//! The history store on disk: two tiers, one writer.
//!
//! ```text
//! ~/.local/state/sysmon/history/
//!   .lock                          flock'd by the one writer; says who
//!   2026-10-10T23.jsonl            the open hour (UTC), one record a line
//!   2026-10-10T22.jsonl.gz         sealed hours: the hot tier
//!   2026-10-10T22.rollup.zst       each sealed hour, condensed (rollup.rs)
//!   archive/2026-10-02.jsonl.zst   older days, packed (archive.rs)
//! ```
//!
//! **Hot tier.** A line is appended with one `write` and no fsync: the
//! page cache batches the disk writes, and a reader in another process
//! sees the line at once. Sealing an hour appends one gzip member to
//! `<hour>.jsonl.gz` and then removes the `.jsonl`; a crash between the
//! two leaves a line in both, which the reader drops by timestamp. A
//! restart inside the same hour appends to the open file; a leftover
//! `.jsonl` from an earlier hour is sealed on open.
//!
//! **Archive tier.** Once a whole UTC day is older than the hot window
//! (`Limits::hot_seconds`, 7 days by default), its hours are packed,
//! losslessly, into one zstd file for the day: same lines, 27% smaller
//! than the gzip hours with the rollups included (measured on the real
//! recording: 141 → 103 B a sample; raw lines are 3.4 KB). The day file is written
//! beside, decoded back and checked line for line, renamed into place,
//! and only then are the hourly files removed. A crash at any point
//! leaves the records in one tier or both, never in neither.
//!
//! **Nothing is deleted by age or size unless the user asks.** The
//! defaults keep everything. `keep_seconds` (drop whole days older
//! than that) and `max_bytes` (drop the oldest days, then hours, until
//! the store fits; never the open hour) are opt-in. Past
//! [`WARN_BYTES`] with no cap the status carries a warning instead.
//!
//! Packing, rollups and pruning run on one background thread at nice
//! 19 (`maintenance`), never on the sampler's thread, and only while
//! the writer lock is held: one maintainer at a time, always the
//! writer.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::archive::{self, HourBlob};
use super::read::{self, Growth, HotFile, StoreInfo};
use super::rollup::HourRollup;
use super::summary::GapHistogram;
use super::{HistoryRecord, utc_civil, utc_hour_start};

pub(crate) const LOCK_FILE: &str = ".lock";
const MAINTENANCE_EVERY: Duration = Duration::from_secs(300);
/// Past this size, with no cap set, the status warns (it never
/// deletes). At this machine's measured density (103 B a packed
/// sample) that is about 1.5 years of 5 s samples, or 7 months of
/// 2 s ones: late enough not to nag, early enough to decide.
pub const WARN_BYTES: u64 = 1_000_000_000;

/// How much history to keep, and how.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Limits {
    /// Hours stay hourly gzip files this long; whole days older than
    /// this are packed into the archive.
    pub hot_seconds: f64,
    /// Drop whole days older than this. None: keep forever (default).
    pub keep_seconds: Option<f64>,
    /// Drop the oldest data until the store fits. None: no cap (default).
    pub max_bytes: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { hot_seconds: 7.0 * 86_400.0, keep_seconds: None, max_bytes: None }
    }
}

impl Limits {
    /// The smallest cap accepted (32 MB): the open hour is raw until it
    /// is sealed, ~12 MB at a 1 s cadence here.
    pub const MIN_BYTES: u64 = 32_000_000;
    pub const MIN_HOT_SECONDS: f64 = 3_600.0;

    /// Clamped: a hot window of at least an hour, a keep of at least
    /// the hot window, a cap of at least MIN_BYTES.
    pub fn new(hot_seconds: f64, keep_seconds: Option<f64>, max_bytes: Option<u64>) -> Limits {
        let hot_seconds = if hot_seconds.is_finite() { hot_seconds.max(Self::MIN_HOT_SECONDS) } else { 7.0 * 86_400.0 };
        Limits {
            hot_seconds,
            keep_seconds: keep_seconds.filter(|k| k.is_finite() && *k > 0.0).map(|k| k.max(hot_seconds)),
            max_bytes: max_bytes.filter(|b| *b > 0).map(|b| b.max(Self::MIN_BYTES)),
        }
    }
}

/// Why a recorder could not open.
#[derive(Debug)]
pub enum OpenError {
    /// Another process holds the writer lock (its own words from the
    /// lock file, e.g. "pid 4242 gui").
    Busy(String),
    Io(std::io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Busy(holder) => write!(f, "another SysMon is recording ({holder})"),
            OpenError::Io(error) => write!(f, "{error}"),
        }
    }
}

/// The writer's own account, for the menu and `ctl status`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RecordingStatus {
    pub recording: bool,
    pub dir: PathBuf,
    /// Both tiers.
    pub bytes: u64,
    pub hot_bytes: u64,
    pub archive_bytes: u64,
    pub archive_days: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_ts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_ts: Option<f64>,
    pub hot_seconds: f64,
    /// null: kept forever.
    pub keep_seconds: Option<f64>,
    /// null: no size cap.
    pub max_bytes: Option<u64>,
    /// The archive's growth at the cadence being recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub growth: Option<Growth>,
    /// The background thread is packing or pruning right now.
    pub packing: bool,
    /// Set when recording failed, or maintenance hit an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// Set when the store is past WARN_BYTES with no cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// "2026-10-10T23" — the UTC hour a timestamp falls in.
pub fn hour_stamp(ts: f64) -> String {
    let (year, month, day, hour) = utc_civil(ts);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}")
}

/// The hour start of a stamp, None for a name that isn't one.
pub fn parse_hour_stamp(stamp: &str) -> Option<f64> {
    let (date, hour) = stamp.split_once('T')?;
    let mut parts = date.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    let hour = hour.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 {
        return None;
    }
    Some(utc_hour_start(year, month, day, hour))
}

fn flock(file: &File, operation: libc::c_int) -> bool {
    // SAFETY: flock on a descriptor this File owns.
    unsafe { libc::flock(file.as_raw_fd(), operation) == 0 }
}

/// Who holds the writer lock right now, if anyone ("pid 4242 gui").
/// A momentary shared lock: it succeeds only when no writer holds it.
pub fn current_writer(dir: &Path) -> Option<String> {
    let path = dir.join(LOCK_FILE);
    let file = File::open(&path).ok()?;
    if flock(&file, libc::LOCK_SH | libc::LOCK_NB) {
        flock(&file, libc::LOCK_UN);
        return None;
    }
    let holder = fs::read_to_string(&path).unwrap_or_default().trim().to_string();
    Some(if holder.is_empty() { "unknown process".to_string() } else { holder })
}

/// Take the writer lock in `dir` (created, mode 700), naming `who`.
pub(crate) fn take_lock(dir: &Path, who: &str) -> Result<File, OpenError> {
    fs::create_dir_all(dir).map_err(OpenError::Io)?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let mut lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))
        .map_err(OpenError::Io)?;
    if !flock(&lock, libc::LOCK_EX | libc::LOCK_NB) {
        let holder = fs::read_to_string(dir.join(LOCK_FILE)).unwrap_or_default().trim().to_string();
        return Err(OpenError::Busy(if holder.is_empty() { "unknown process".into() } else { holder }));
    }
    lock.set_len(0).map_err(OpenError::Io)?;
    writeln!(lock, "pid {} {who}", std::process::id()).map_err(OpenError::Io)?;
    Ok(lock)
}

/// Which hours a maintenance pass packs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PackScope {
    /// Whole days older than the hot window (the schedule).
    Due,
    /// Every sealed hour but the current one (`sysmon history --pack`).
    AllSealed,
}

/// What one maintenance pass did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MaintenanceReport {
    pub packed_hours: usize,
    pub packed_days: usize,
    /// Bytes of hourly files the packed hours took before.
    pub packed_from_bytes: u64,
    /// The day files' size after (they may have held earlier hours).
    pub archive_written_bytes: u64,
    pub rollups_built: usize,
    /// Unparseable (torn) lines left out of the archive.
    pub torn_lines: usize,
    pub removed_by_keep: usize,
    pub removed_by_cap: usize,
    pub removed_bytes: u64,
    pub cancelled: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    #[serde(skip)]
    pub info: StoreInfo,
}

type Job = std::thread::JoinHandle<MaintenanceReport>;

/// The one writer.
pub struct Recorder {
    dir: PathBuf,
    limits: Limits,
    _lock: File,
    open: Option<(String, File)>,
    last_ts: f64,
    gaps: GapHistogram,
    last_maintenance: Instant,
    job: Option<Job>,
    cancel: Arc<AtomicBool>,
    rerun: bool,
    archive_level: i32,
    problem: Option<String>,
    info: StoreInfo,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // A pass stops between hours (≤ ~1 s) and leaves no half-written
        // day behind; the lock goes only after it has stopped.
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(job) = self.job.take() {
            let _ = job.join();
        }
    }
}

impl Recorder {
    /// Take the writer lock in `dir` (created, mode 700), seal any
    /// earlier hour left open, and start a maintenance pass. `who`
    /// names the writer in the lock file ("gui", "serve").
    pub fn open(dir: &Path, limits: Limits, who: &str) -> Result<Recorder, OpenError> {
        let lock = take_lock(dir, who)?;
        let mut recorder = Recorder {
            dir: dir.to_path_buf(),
            limits: Limits::new(limits.hot_seconds, limits.keep_seconds, limits.max_bytes),
            _lock: lock,
            open: None,
            last_ts: 0.0,
            gaps: GapHistogram::default(),
            last_maintenance: Instant::now(),
            job: None,
            cancel: Arc::new(AtomicBool::new(false)),
            rerun: false,
            archive_level: archive::ARCHIVE_LEVEL,
            problem: None,
            info: StoreInfo::default(),
        };
        let current = hour_stamp(crate::collect::unix_now());
        for file in read::list_hot(dir).into_iter().filter(|f| !f.sealed && f.stamp != current) {
            recorder.seal(&file.stamp).map_err(OpenError::Io)?;
        }
        recorder.info = read::info(dir, true);
        recorder.maintain();
        Ok(recorder)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The zstd level packed hours get (default `ARCHIVE_LEVEL`). Only
    /// the scale test lowers it, to build a simulated year quickly.
    pub fn set_archive_level(&mut self, level: i32) {
        self.archive_level = level.clamp(1, 22);
    }

    /// New limits apply at once (a smaller cap prunes now).
    pub fn set_limits(&mut self, limits: Limits) {
        let limits = Limits::new(limits.hot_seconds, limits.keep_seconds, limits.max_bytes);
        if limits != self.limits {
            self.limits = limits;
            self.maintain();
        }
    }

    /// Append one record. A record not newer than the last one is
    /// skipped (a paused or re-sent sample is not a new reading).
    pub fn record(&mut self, record: &HistoryRecord) -> std::io::Result<()> {
        self.poll_job();
        if record.ts <= self.last_ts {
            return Ok(());
        }
        let stamp = hour_stamp(record.ts);
        if self.open.as_ref().is_none_or(|(open, _)| *open != stamp) {
            if let Some((previous, _)) = self.open.take() {
                self.seal(&previous)?;
                self.maintain();
            }
            let file = OpenOptions::new().create(true).append(true).open(self.dir.join(format!("{stamp}{}", read::OPEN_SUFFIX)))?;
            self.open = Some((stamp, file));
        }
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        if let Some((_, file)) = &mut self.open {
            // One write per line: a concurrent reader sees whole lines
            // or none (it skips a torn tail either way).
            file.write_all(line.as_bytes())?;
        }
        if self.last_ts > 0.0 && record.ts - self.last_ts < 600.0 {
            self.gaps.add(record.ts - self.last_ts);
        }
        self.last_ts = record.ts;
        self.info.bytes += line.len() as u64;
        self.info.hot_bytes += line.len() as u64;
        self.info.newest_ts = Some(record.ts);
        if self.info.oldest_ts.is_none() {
            self.info.oldest_ts = Some(record.ts);
        }
        if self.last_maintenance.elapsed() >= MAINTENANCE_EVERY {
            self.maintain();
        }
        Ok(())
    }

    pub fn status(&self) -> RecordingStatus {
        let cadence = self.gaps.median_seconds().or(self.info.cadence_seconds);
        let growth = cadence.and_then(|c| read::growth(&self.info, c));
        let warning = (self.limits.max_bytes.is_none() && self.info.bytes > WARN_BYTES).then(|| {
            format!(
                "history is {:.1} GB and nothing removes it; Export… then Clear…, or set a size cap",
                self.info.bytes as f64 / 1e9
            )
        });
        RecordingStatus {
            recording: true,
            dir: self.dir.clone(),
            bytes: self.info.bytes,
            hot_bytes: self.info.hot_bytes,
            archive_bytes: self.info.archive_bytes,
            archive_days: self.info.archive_days,
            oldest_ts: self.info.oldest_ts,
            newest_ts: if self.last_ts > 0.0 { Some(self.last_ts) } else { self.info.newest_ts },
            hot_seconds: self.limits.hot_seconds,
            keep_seconds: self.limits.keep_seconds,
            max_bytes: self.limits.max_bytes,
            growth,
            packing: self.job.is_some(),
            problem: self.problem.clone(),
            warning,
        }
    }

    /// Delete the whole history (both tiers), keeping the lock: the
    /// next record starts a fresh store. Waits for a running pass.
    pub fn clear(&mut self) -> std::io::Result<super::manage::ClearReport> {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(job) = self.job.take() {
            let _ = job.join();
        }
        self.cancel.store(false, Ordering::SeqCst);
        self.open = None;
        let report = super::manage::remove_history_files(&self.dir)?;
        self.info = read::info(&self.dir, false);
        self.problem = None;
        self.rerun = false;
        Ok(report)
    }

    /// Run one maintenance pass now, on this thread (`--pack`).
    pub fn maintain_blocking(&mut self, scope: PackScope) -> MaintenanceReport {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(job) = self.job.take() {
            let _ = job.join();
        }
        self.cancel.store(false, Ordering::SeqCst);
        let open_stamp = self.open.as_ref().map(|(s, _)| s.clone());
        let report =
            maintenance(&self.dir, self.limits, crate::collect::unix_now(), open_stamp, scope, self.archive_level, &self.cancel);
        self.absorb(&report);
        report
    }

    fn absorb(&mut self, report: &MaintenanceReport) {
        let mut info = report.info.clone();
        if self.last_ts > 0.0 {
            info.newest_ts = Some(self.last_ts);
        }
        info.cadence_seconds = info.cadence_seconds.or(self.info.cadence_seconds);
        self.info = info;
        self.problem = report.problems.first().cloned();
    }

    /// Collect a finished pass; start the next if one was asked for.
    fn poll_job(&mut self) {
        if self.job.as_ref().is_some_and(|job| job.is_finished())
            && let Some(job) = self.job.take()
        {
            match job.join() {
                Ok(report) => self.absorb(&report),
                Err(_) => self.problem = Some("history maintenance crashed; it retries in 5 minutes".to_string()),
            }
            if std::mem::take(&mut self.rerun) {
                self.maintain();
            }
        }
    }

    /// Start a background pass (or ask for another after the running one).
    fn maintain(&mut self) {
        self.last_maintenance = Instant::now();
        self.poll_job();
        if self.job.is_some() {
            self.rerun = true;
            return;
        }
        let dir = self.dir.clone();
        let limits = self.limits;
        let open_stamp = self.open.as_ref().map(|(s, _)| s.clone());
        let cancel = self.cancel.clone();
        let level = self.archive_level;
        let spawned = std::thread::Builder::new().name("sysmon-history-pack".to_string()).spawn(move || {
            lower_this_threads_priority();
            maintenance(&dir, limits, crate::collect::unix_now(), open_stamp, PackScope::Due, level, &cancel)
        });
        match spawned {
            Ok(job) => self.job = Some(job),
            Err(error) => self.problem = Some(format!("could not start history maintenance: {error}")),
        }
    }

    /// Append the open file of `stamp` to its sealed member, then drop
    /// the open file.
    fn seal(&mut self, stamp: &str) -> std::io::Result<()> {
        let open_path = self.dir.join(format!("{stamp}{}", read::OPEN_SUFFIX));
        let raw = match fs::read(&open_path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !raw.is_empty() {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
            encoder.write_all(&raw)?;
            let member = encoder.finish()?;
            let mut sealed =
                OpenOptions::new().create(true).append(true).open(self.dir.join(format!("{stamp}{}", read::SEALED_SUFFIX)))?;
            sealed.write_all(&member)?;
            sealed.sync_data()?;
        }
        fs::remove_file(&open_path)?;
        if self.open.as_ref().is_some_and(|(open, _)| open == stamp) {
            self.open = None;
        }
        Ok(())
    }
}

/// Packing is background work: let everything else go first.
fn lower_this_threads_priority() {
    // SAFETY: setpriority on this thread's own tid (Linux applies
    // PRIO_PROCESS with a tid to that one thread).
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 19);
    }
}

// ---------------------------------------------------------- maintenance

/// One pass: rollups for sealed hours that lack one, pack what is due,
/// then the opt-in keep and cap. Holds no lock itself: only a Recorder
/// (which holds the writer lock) runs it.
pub(crate) fn maintenance(
    dir: &Path,
    limits: Limits,
    now: f64,
    open_stamp: Option<String>,
    scope: PackScope,
    level: i32,
    cancel: &AtomicBool,
) -> MaintenanceReport {
    let mut report = MaintenanceReport::default();
    let current = hour_stamp(now);
    let hot = read::list_hot(dir);
    let due_before = now - limits.hot_seconds;
    let packable = |file: &HotFile| file.sealed && file.stamp != current && Some(&file.stamp) != open_stamp.as_ref();
    let due = |file: &HotFile| match scope {
        PackScope::AllSealed => true,
        PackScope::Due => {
            let day_start = archive::parse_day_stamp(&archive::day_stamp(file.start)).unwrap_or(file.start);
            day_start + 86_400.0 <= due_before
        }
    };

    // Rollups, so long ranges stay fast before anything is packed (an
    // hour about to be packed gets its rollup in the archive instead).
    for file in hot.iter().filter(|f| packable(f) && !due(f)) {
        if cancel.load(Ordering::SeqCst) {
            report.cancelled = true;
            break;
        }
        let fresh = read::read_rollup_file(&read::rollup_path(dir, &file.stamp))
            .is_some_and(|rollup| rollup.source_bytes == file.bytes);
        if !fresh {
            match build_hot_rollup(dir, file) {
                Ok(true) => report.rollups_built += 1,
                Ok(false) => {}
                Err(error) => report.problems.push(format!("rollup for {}: {error}", file.stamp)),
            }
        }
    }
    // A rollup whose hour is gone (packed, or removed by hand) goes too.
    for (stamp, path, _) in read::list_rollup_files(dir) {
        if !hot.iter().any(|f| f.sealed && f.stamp == stamp) {
            let _ = fs::remove_file(path);
        }
    }

    // Pack: whole due days, or every sealed hour.
    let mut days: std::collections::BTreeMap<String, Vec<HotFile>> = Default::default();
    for file in hot.into_iter().filter(|f| packable(f) && due(f)) {
        days.entry(archive::day_stamp(file.start)).or_default().push(file);
    }
    if !days.is_empty() {
        let _ = fs::create_dir_all(dir.join(archive::ARCHIVE_DIR));
    }
    for (day, files) in days {
        if cancel.load(Ordering::SeqCst) {
            report.cancelled = true;
            break;
        }
        match pack_day(dir, &day, &files, level, cancel) {
            Ok(Some(packed)) => {
                report.packed_days += 1;
                report.packed_hours += packed.hours;
                report.packed_from_bytes += packed.from_bytes;
                report.archive_written_bytes += packed.written_bytes;
                report.torn_lines += packed.torn_lines;
            }
            Ok(None) => report.cancelled = true,
            Err(error) => report.problems.push(format!("packing {day} failed (its hourly files are kept): {error}")),
        }
    }

    // Keep: only when the user set one.
    if let Some(keep) = limits.keep_seconds {
        let oldest_allowed = now - keep;
        for (start, _, path, bytes) in archive::list_days(dir) {
            if start + 86_400.0 <= oldest_allowed && fs::remove_file(&path).is_ok() {
                report.removed_by_keep += 1;
                report.removed_bytes += bytes;
            }
        }
        for file in read::list_hot(dir) {
            if file.sealed && file.start + 3_600.0 <= oldest_allowed && fs::remove_file(&file.path).is_ok() {
                let _ = fs::remove_file(read::rollup_path(dir, &file.stamp));
                report.removed_by_keep += 1;
                report.removed_bytes += file.bytes;
            }
        }
    }

    // Cap: only when the user set one. Oldest first; never the open hour.
    if let Some(cap) = limits.max_bytes {
        let mut total = read::info(dir, false).bytes;
        let mut days = archive::list_days(dir).into_iter();
        while total > cap {
            let Some((_, _, path, bytes)) = days.next() else { break };
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(bytes);
                report.removed_by_cap += 1;
                report.removed_bytes += bytes;
            }
        }
        let mut hours = read::list_hot(dir).into_iter().filter(|f| f.sealed && Some(&f.stamp) != open_stamp.as_ref());
        while total > cap {
            let Some(file) = hours.next() else { break };
            if fs::remove_file(&file.path).is_ok() {
                let rollup = read::rollup_path(dir, &file.stamp);
                let rollup_bytes = fs::metadata(&rollup).map(|m| m.len()).unwrap_or(0);
                let _ = fs::remove_file(rollup);
                total = total.saturating_sub(file.bytes + rollup_bytes);
                report.removed_by_cap += 1;
                report.removed_bytes += file.bytes + rollup_bytes;
            }
        }
        if total > cap {
            report.problems.push(format!(
                "the size cap ({} MB) is smaller than the hour being written; raise the cap",
                cap / 1_000_000
            ));
        }
    }
    report.info = read::info(dir, false);
    report
}

/// Build and write a sealed hot hour's rollup. Ok(false): no records.
fn build_hot_rollup(dir: &Path, file: &HotFile) -> std::io::Result<bool> {
    let mut records = Vec::new();
    read::read_hot_file(file, f64::MIN, f64::MAX, &mut records);
    records.sort_by(|a, b| a.ts.total_cmp(&b.ts));
    records.dedup_by(|a, b| a.ts == b.ts);
    let Some(mut rollup) = HourRollup::build(file.start, &records) else { return Ok(false) };
    rollup.source_bytes = file.bytes;
    read::write_rollup_file(dir, &file.stamp, &rollup)?;
    Ok(true)
}

struct Packed {
    hours: usize,
    from_bytes: u64,
    written_bytes: u64,
    torn_lines: usize,
}

/// One hour's lines gathered for packing: the original bytes, one line
/// per timestamp, oldest first.
fn gather_lines(sources: &[&[u8]]) -> (Vec<u8>, Vec<HistoryRecord>, usize) {
    let mut lines: Vec<(f64, &[u8], HistoryRecord)> = Vec::new();
    let mut torn = 0;
    for source in sources {
        for line in source.split(|b| *b == b'\n') {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<HistoryRecord>(line) {
                Ok(record) => lines.push((record.ts, line, record)),
                Err(_) => torn += 1,
            }
        }
    }
    // Stable: on a tie the first source (the archive) wins, as it does
    // for a reader.
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    lines.dedup_by(|a, b| a.0 == b.0);
    let mut bytes = Vec::with_capacity(lines.iter().map(|l| l.1.len() + 1).sum());
    let mut records = Vec::with_capacity(lines.len());
    for (_, line, record) in lines {
        bytes.extend_from_slice(line);
        bytes.push(b'\n');
        records.push(record);
    }
    (bytes, records, torn)
}

fn read_gz(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut decoder = flate2::read::MultiGzDecoder::new(File::open(path)?);
    match decoder.read_to_end(&mut out) {
        Ok(_) => Ok(out),
        // A member cut short by a crash: keep what decoded (the torn
        // tail line is dropped by the parse), as the reader does.
        Err(_) if !out.is_empty() => Ok(out),
        Err(error) => Err(error),
    }
}

/// Pack `files` (sealed hours of one UTC day) into the day's archive
/// file, merging what the file already holds. Ok(None): cancelled
/// before anything changed.
fn pack_day(dir: &Path, day: &str, files: &[HotFile], level: i32, cancel: &AtomicBool) -> std::io::Result<Option<Packed>> {
    let final_path = archive::day_path(dir, day);
    let existing = if final_path.exists() { Some(archive::read_index(&final_path)?) } else { None };
    let existing_rollups = match &existing {
        Some(index) => archive::read_rollups(&final_path, index).unwrap_or_default(),
        None => Vec::new(),
    };
    let mut existing_file = existing.as_ref().map(|_| File::open(&final_path)).transpose()?;

    // Every hour of the day that will be in the new file.
    let mut starts: Vec<f64> = files.iter().map(|f| f.start).collect();
    if let Some(index) = &existing {
        starts.extend(index.hours.iter().map(|h| h.start));
    }
    starts.sort_by(f64::total_cmp);
    starts.dedup();

    let mut compressor = archive::compressor_at(level)?;
    let mut blobs: Vec<HourBlob> = Vec::new();
    let mut expected: Vec<(f64, u64)> = Vec::new();
    let mut torn_lines = 0;
    let mut from_bytes = 0;
    for start in starts {
        if cancel.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let hot: Vec<&HotFile> = files.iter().filter(|f| f.start == start).collect();
        let archived = existing.as_ref().and_then(|index| index.hours.iter().position(|h| h.start == start));
        if hot.is_empty()
            && let (Some(position), Some(index), Some(file)) = (archived, &existing, existing_file.as_mut())
        {
            // Already packed and unchanged: carry its frame over as is.
            let entry = index.hours[position].clone();
            let rollup_line = match existing_rollups.get(position).cloned().flatten() {
                Some(rollup) => rollup.to_line(),
                None => {
                    let lines = archive::read_hour(file, index, &entry)?;
                    let (_, records, _) = gather_lines(&[&lines]);
                    HourRollup::build(start, &records).map(|r| r.to_line()).unwrap_or_default()
                }
            };
            let frame = {
                use std::io::{Read, Seek, SeekFrom};
                file.seek(SeekFrom::Start(index.data_start + entry.offset))?;
                let mut frame = vec![0u8; entry.length as usize];
                file.read_exact(&mut frame)?;
                frame
            };
            expected.push((start, entry.records));
            blobs.push(HourBlob { entry, frame, rollup_line });
            continue;
        }
        let mut sources: Vec<Vec<u8>> = Vec::new();
        if let (Some(position), Some(index), Some(file)) = (archived, &existing, existing_file.as_mut()) {
            sources.push(archive::read_hour(file, index, &index.hours[position])?);
        }
        for file in &hot {
            sources.push(read_gz(&file.path)?);
            from_bytes += file.bytes;
        }
        let views: Vec<&[u8]> = sources.iter().map(Vec::as_slice).collect();
        let (lines, records, torn) = gather_lines(&views);
        torn_lines += torn;
        if records.is_empty() {
            continue;
        }
        let rollup = HourRollup::build(start, &records);
        let first_ts = records.first().map_or(start, |r| r.ts);
        let last_ts = records.last().map_or(start, |r| r.ts);
        let blob = archive::hour_blob(&mut compressor, start, &lines, records.len() as u64, first_ts, last_ts, rollup.as_ref())?;
        expected.push((start, records.len() as u64));
        blobs.push(blob);
    }
    if blobs.is_empty() {
        // Nothing but torn lines: the hourly files stay for a person to look at.
        return Ok(Some(Packed { hours: 0, from_bytes: 0, written_bytes: 0, torn_lines }));
    }

    let temporary = dir.join(archive::ARCHIVE_DIR).join(format!(".{day}{}.tmp-{}", archive::DAY_SUFFIX, std::process::id()));
    let written = archive::write_day(&temporary, day, blobs)?;
    // Decode it all back before anything is removed.
    let verified = match archive::verify_day(&temporary) {
        Ok(index) => index,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
    };
    let got: Vec<(f64, u64)> = verified.hours.iter().map(|h| (h.start, h.records)).collect();
    if got != expected {
        let _ = fs::remove_file(&temporary);
        return Err(std::io::Error::other("the packed day does not list the hours that went in"));
    }
    fs::rename(&temporary, &final_path)?;
    if let Ok(handle) = File::open(dir.join(archive::ARCHIVE_DIR)) {
        let _ = handle.sync_all();
    }
    let mut hours = 0;
    for file in files {
        // Removed only if it is still exactly what was packed.
        if fs::metadata(&file.path).is_ok_and(|m| m.len() == file.bytes) {
            let _ = fs::remove_file(&file.path);
            let _ = fs::remove_file(read::rollup_path(dir, &file.stamp));
            hours += 1;
        }
    }
    Ok(Some(Packed { hours, from_bytes, written_bytes: written, torn_lines }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::read::{Recording, summarize_dir};
    use crate::history::{CpuRecord, GpuRecord};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sysmon-history-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn record(ts: f64, busy: f32) -> HistoryRecord {
        HistoryRecord {
            ts,
            interval_seconds: 2.0,
            cpu: Some(CpuRecord { busy_percent: busy, ..Default::default() }),
            gpu: Some(GpuRecord { busy_percent: busy, vram_used_bytes: 1 << 30, ..Default::default() }),
            ..Default::default()
        }
    }

    fn all(dir: &Path) -> (Vec<HistoryRecord>, read::ReadStats) {
        let mut out = Vec::new();
        let stats = Recording::for_each(dir, 0.0, f64::MAX, |r| out.push(r));
        (out, stats)
    }

    fn forever() -> Limits {
        Limits::new(1e9, None, None)
    }

    /// Sealed hours written straight to disk, as a recorder would have.
    fn write_sealed(dir: &Path, start: f64, count: u32, step: f64) {
        fs::create_dir_all(dir).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        for i in 0..count {
            let line = serde_json::to_string(&record(start + 1.0 + f64::from(i) * step, (i % 100) as f32)).unwrap() + "\n";
            encoder.write_all(line.as_bytes()).unwrap();
        }
        fs::write(dir.join(format!("{}.jsonl.gz", hour_stamp(start))), encoder.finish().unwrap()).unwrap();
    }

    #[test]
    fn hour_stamps_round_trip_and_junk_is_refused() {
        assert_eq!(hour_stamp(1_791_673_200.0), "2026-10-10T23");
        assert_eq!(parse_hour_stamp("2026-10-10T23"), Some(1_791_673_200.0));
        for junk in ["2026-10-10", "2026-13-01T00", "2026-10-10T24", "x-y-zT1", "2026-10-10-1T01"] {
            assert_eq!(parse_hour_stamp(junk), None, "{junk}");
        }
    }

    #[test]
    fn an_hour_boundary_seals_the_last_hour_and_reads_back_whole() {
        let dir = scratch("seal");
        let start = 1_791_673_200.0 - 10.0; // 22:59:50 UTC
        let mut recorder = Recorder::open(&dir, forever(), "test").unwrap();
        for i in 0..10 {
            recorder.record(&record(start + f64::from(i) * 2.0, i as f32)).unwrap();
        }
        let names: Vec<String> = read::list_hot(&dir).iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into()).collect();
        assert_eq!(names, ["2026-10-10T22.jsonl.gz", "2026-10-10T23.jsonl"], "22:xx sealed, 23:xx open");
        let (records, stats) = all(&dir);
        assert_eq!(records.len(), 10);
        assert_eq!(stats.skipped_lines, 0);
        assert!(records.windows(2).all(|w| w[0].ts < w[1].ts), "oldest first");
        // A stale or repeated sample is not a new line.
        recorder.record(&record(start, 99.0)).unwrap();
        assert_eq!(all(&dir).0.len(), 10);
        drop(recorder);
        assert!(dir.join("2026-10-10T22.rollup.zst").exists(), "the sealed hour got its rollup");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn one_writer_at_a_time_and_the_lock_names_it() {
        let dir = scratch("lock");
        let first = Recorder::open(&dir, forever(), "gui").unwrap();
        match Recorder::open(&dir, forever(), "serve") {
            Err(OpenError::Busy(holder)) => assert_eq!(holder, format!("pid {} gui", std::process::id())),
            other => panic!("a second writer must be refused, got {:?}", other.map(|_| ())),
        }
        assert_eq!(current_writer(&dir), Some(format!("pid {} gui", std::process::id())));
        drop(first);
        // The lock is released when its descriptor closes. Another test
        // in this binary may be forking a helper (`sh`, `sleep`) at that
        // instant; the child holds a copy of every descriptor until its
        // exec closes them (O_CLOEXEC), so allow that moment to pass.
        let waited = Instant::now();
        while current_writer(&dir).is_some() && waited.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(current_writer(&dir), None, "the lock goes with the writer");
        assert!(Recorder::open(&dir, forever(), "serve").is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_crash_left_open_hour_is_sealed_on_open_and_a_torn_line_is_counted() {
        let dir = scratch("crash");
        fs::create_dir_all(&dir).unwrap();
        let mut lines = String::new();
        for i in 0..3 {
            lines += &serde_json::to_string(&record(1_791_600_000.0 + f64::from(i), 1.0)).unwrap();
            lines.push('\n');
        }
        lines += "{\"ts\":1791600003.0,\"interv"; // torn by the crash
        let stamp = hour_stamp(1_791_600_000.0);
        fs::write(dir.join(format!("{stamp}.jsonl")), lines).unwrap();
        let _recorder = Recorder::open(&dir, forever(), "test").unwrap();
        assert!(dir.join(format!("{stamp}.jsonl.gz")).exists(), "sealed");
        assert!(!dir.join(format!("{stamp}.jsonl")).exists());
        let (records, stats) = all(&dir);
        assert_eq!(records.len(), 3);
        assert_eq!(stats.skipped_lines, 1, "the torn line is reported, not hidden");
        let _ = fs::remove_dir_all(dir);
    }

    /// 3.5: nothing old is deleted unless the user asks. Hours past the
    /// hot window are packed into day files, every record still there,
    /// and the same summary comes back from the packed store.
    #[test]
    fn old_hours_are_packed_never_dropped_by_default() {
        let dir = scratch("pack");
        let now = crate::collect::unix_now();
        let today = archive::parse_day_stamp(&archive::day_stamp(now)).unwrap();
        // Three hours on a day 40 days back, two on a day 10 days back,
        // and one an hour ago.
        for start in [today - 40.0 * 86_400.0 + 3_600.0, today - 40.0 * 86_400.0 + 7_200.0, today - 40.0 * 86_400.0 + 82_800.0] {
            write_sealed(&dir, start, 900, 4.0);
        }
        for start in [today - 10.0 * 86_400.0, today - 10.0 * 86_400.0 + 3_600.0] {
            write_sealed(&dir, start, 900, 4.0);
        }
        let recent = ((now - 3_600.0) / 3_600.0).floor() * 3_600.0 - 3_600.0;
        write_sealed(&dir, recent, 900, 4.0);
        let before = all(&dir).0;
        let summary_before = summarize_dir(&dir, "all", 0.0, now, 48, false);
        assert_eq!(before.len(), 6 * 900);

        let mut recorder = Recorder::open(&dir, Limits::new(7.0 * 86_400.0, None, None), "test").unwrap();
        let report = recorder.maintain_blocking(PackScope::Due);
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!((report.packed_days, report.packed_hours), (2, 5), "{report:?}");
        assert_eq!(archive::list_days(&dir).len(), 2);
        let hot: Vec<String> = read::list_hot(&dir).iter().map(|f| f.stamp.clone()).collect();
        assert_eq!(hot, [hour_stamp(recent)], "only the recent hour stays hourly");
        let after = all(&dir).0;
        assert_eq!(after, before, "lossless: every record, in order, unchanged");
        let summary_after = summarize_dir(&dir, "all", 0.0, now, 48, true);
        assert_eq!(summary_after.samples, summary_before.samples);
        assert_eq!(summary_after.metrics, summary_before.metrics);
        assert!(summary_after.rollup_hours >= 5, "the packed hours merge from their rollups");
        // Packing again changes nothing.
        let again = recorder.maintain_blocking(PackScope::Due);
        assert_eq!(again.packed_hours, 0);
        assert_eq!(all(&dir).0, before);
        let status = recorder.status();
        assert_eq!(status.archive_days, 2);
        assert_eq!(status.keep_seconds, None);
        assert_eq!(status.max_bytes, None);
        let _ = fs::remove_dir_all(dir);
    }

    /// A day that already has an archive file takes late hours in (a
    /// crash-left hour sealed long after), and an hour in both tiers
    /// (a crash between the rename and the cleanup) is read once.
    #[test]
    fn late_hours_merge_into_their_day_and_duplicates_read_once() {
        let dir = scratch("merge");
        let day = 1_791_590_400.0; // 2026-10-10 00:00 UTC
        write_sealed(&dir, day, 100, 10.0);
        let mut recorder = Recorder::open(&dir, forever(), "test").unwrap();
        recorder.maintain_blocking(PackScope::AllSealed);
        assert_eq!(read::list_hot(&dir).len(), 0);
        // The same hour again (as if the cleanup never ran), plus a new one.
        write_sealed(&dir, day, 100, 10.0);
        write_sealed(&dir, day + 3_600.0, 50, 10.0);
        let (records, _) = all(&dir);
        assert_eq!(records.len(), 150, "the duplicate hour reads once");
        recorder.maintain_blocking(PackScope::AllSealed);
        let index = archive::read_index(&archive::day_path(&dir, "2026-10-10")).unwrap();
        assert_eq!(index.hours.iter().map(|h| h.records).collect::<Vec<_>>(), [100, 50]);
        assert_eq!(all(&dir).0, records);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn keep_and_cap_remove_only_when_set() {
        let dir = scratch("limits");
        let now = crate::collect::unix_now();
        let today = archive::parse_day_stamp(&archive::day_stamp(now)).unwrap();
        for back in [100.0, 50.0, 20.0] {
            write_sealed(&dir, today - back * 86_400.0, 2_000, 1.5);
        }
        let mut recorder = Recorder::open(&dir, Limits::new(86_400.0, None, None), "test").unwrap();
        recorder.maintain_blocking(PackScope::Due);
        assert_eq!(archive::list_days(&dir).len(), 3, "nothing removed by default");
        // Keep 60 days: the 100-day-old day goes.
        recorder.set_limits(Limits::new(86_400.0, Some(60.0 * 86_400.0), None));
        recorder.maintain_blocking(PackScope::Due);
        assert_eq!(archive::list_days(&dir).len(), 2);
        // A cap below both days' total removes the older.
        let days = archive::list_days(&dir);
        let mut squeezed = Limits::new(86_400.0, None, None);
        squeezed.max_bytes = Some(days[1].3 + 100);
        let report = maintenance(&dir, squeezed, now, None, PackScope::Due, archive::ARCHIVE_LEVEL, &AtomicBool::new(false));
        assert_eq!(report.removed_by_cap, 1, "{report:?}");
        let left = archive::list_days(&dir);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].1, days[1].1, "the newest day stays");
        assert_eq!(Limits::new(86_400.0, None, Some(1)).max_bytes, Some(Limits::MIN_BYTES), "a tiny cap clamps");
        assert_eq!(Limits::new(86_400.0, Some(10.0), None).keep_seconds, Some(86_400.0), "keep ≥ hot");
        let _ = fs::remove_dir_all(dir);
    }
}
