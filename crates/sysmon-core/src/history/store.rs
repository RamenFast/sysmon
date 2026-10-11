// SPDX-License-Identifier: GPL-3.0-or-later
//! The history store on disk: hourly files, sealed with gzip, pruned
//! by age and by size.
//!
//! ```text
//! ~/.local/state/sysmon/history/
//!   .lock                      flock'd by the one writer; says who
//!   2026-10-10T23.jsonl        the open hour (UTC), one record a line
//!   2026-10-10T22.jsonl.gz     sealed hours
//! ```
//!
//! A line is appended with one `write` and no fsync: the page cache
//! batches the disk writes (a 2 s cadence is ~1 KB every 2 s), and a
//! reader in another process sees the line at once. Sealing an hour
//! appends one gzip member to `<hour>.jsonl.gz` and then removes the
//! `.jsonl`; a crash between the two leaves a line in both, which the
//! reader drops by timestamp. A restart inside the same hour appends to
//! the open file; a leftover `.jsonl` from an earlier hour is sealed on
//! open.
//!
//! Limits are promises. Every hour, every 5 minutes of writing, and on
//! every limits change, files older than the retention are removed,
//! then the oldest hours go until the total fits the byte cap. If the
//! open hour alone is over the cap it is sealed early (≈10× smaller);
//! if that is still over, it goes too, and the status says the cap
//! trimmed it.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::{HistoryRecord, utc_civil, utc_hour_start};

const LOCK_FILE: &str = ".lock";
const OPEN_SUFFIX: &str = ".jsonl";
const SEALED_SUFFIX: &str = ".jsonl.gz";
const MAINTENANCE_EVERY: Duration = Duration::from_secs(300);

/// How much history to keep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub retention_seconds: f64,
    pub max_bytes: u64,
}

impl Limits {
    /// The smallest cap accepted (32 MB). The hour being written is raw
    /// until it is sealed: ~12 MB at a 1 s cadence here (3.4 KB a
    /// sample), so it fits under the smallest cap with room.
    pub const MIN_BYTES: u64 = 32_000_000;
    pub const MIN_RETENTION_SECONDS: f64 = 3_600.0;

    pub fn new(retention_seconds: f64, max_bytes: u64) -> Limits {
        Limits {
            retention_seconds: retention_seconds.max(Self::MIN_RETENTION_SECONDS),
            max_bytes: max_bytes.max(Self::MIN_BYTES),
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

/// What is on disk.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StoreInfo {
    pub dir: PathBuf,
    /// Every history file, the open hour included.
    pub bytes: u64,
    pub files: usize,
    /// The first and last record on disk (read from the files).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_ts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_ts: Option<f64>,
}

/// The writer's own account, for the menu and `ctl status`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RecordingStatus {
    pub recording: bool,
    pub dir: PathBuf,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_ts: Option<f64>,
    pub retention_seconds: f64,
    pub max_bytes: u64,
    /// Set when recording failed or the cap had to drop the open hour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// The one writer.
pub struct Recorder {
    dir: PathBuf,
    limits: Limits,
    _lock: File,
    open: Option<(String, File)>,
    last_ts: f64,
    last_maintenance: Instant,
    problem: Option<String>,
    info: StoreInfo,
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

/// One history file on disk.
#[derive(Clone, Debug)]
struct HourFile {
    stamp: String,
    start: f64,
    path: PathBuf,
    sealed: bool,
    bytes: u64,
}

fn list_files(dir: &Path) -> Vec<HourFile> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<HourFile> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (stamp, sealed) = if let Some(stamp) = name.strip_suffix(SEALED_SUFFIX) {
                (stamp.to_string(), true)
            } else {
                (name.strip_suffix(OPEN_SUFFIX)?.to_string(), false)
            };
            let start = parse_hour_stamp(&stamp)?;
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            Some(HourFile { stamp, start, path: entry.path(), sealed, bytes })
        })
        .collect();
    // Oldest first; a sealed member before the open file of its hour.
    files.sort_by(|a, b| a.start.total_cmp(&b.start).then(b.sealed.cmp(&a.sealed)));
    files
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

impl Recorder {
    /// Take the writer lock in `dir` (created, mode 700), seal any
    /// earlier hour left open, and apply the limits. `who` names the
    /// writer in the lock file ("gui", "serve").
    pub fn open(dir: &Path, limits: Limits, who: &str) -> Result<Recorder, OpenError> {
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

        let mut recorder = Recorder {
            dir: dir.to_path_buf(),
            limits: Limits::new(limits.retention_seconds, limits.max_bytes),
            _lock: lock,
            open: None,
            last_ts: 0.0,
            last_maintenance: Instant::now(),
            problem: None,
            info: StoreInfo::default(),
        };
        let current = hour_stamp(crate::collect::unix_now());
        for file in list_files(dir).into_iter().filter(|f| !f.sealed && f.stamp != current) {
            recorder.seal(&file.stamp).map_err(OpenError::Io)?;
        }
        recorder.maintain(crate::collect::unix_now());
        Ok(recorder)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// New limits apply at once (a smaller cap prunes now).
    pub fn set_limits(&mut self, limits: Limits) {
        let limits = Limits::new(limits.retention_seconds, limits.max_bytes);
        if limits != self.limits {
            self.limits = limits;
            self.maintain(crate::collect::unix_now());
        }
    }

    /// Append one record. A record not newer than the last one is
    /// skipped (a paused or re-sent sample is not a new reading).
    pub fn record(&mut self, record: &HistoryRecord) -> std::io::Result<()> {
        if record.ts <= self.last_ts {
            return Ok(());
        }
        let stamp = hour_stamp(record.ts);
        if self.open.as_ref().is_none_or(|(open, _)| *open != stamp) {
            if let Some((previous, _)) = self.open.take() {
                self.seal(&previous)?;
                self.maintain(record.ts);
            }
            let file = OpenOptions::new().create(true).append(true).open(self.dir.join(format!("{stamp}{OPEN_SUFFIX}")))?;
            self.open = Some((stamp, file));
        }
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        if let Some((_, file)) = &mut self.open {
            // One write per line: a concurrent reader sees whole lines
            // or none (it skips a torn tail either way).
            file.write_all(line.as_bytes())?;
        }
        self.last_ts = record.ts;
        self.info.bytes += line.len() as u64;
        self.info.newest_ts = Some(record.ts);
        if self.info.oldest_ts.is_none() {
            self.info.oldest_ts = Some(record.ts);
        }
        if self.last_maintenance.elapsed() >= MAINTENANCE_EVERY {
            self.maintain(record.ts);
        }
        Ok(())
    }

    pub fn status(&self) -> RecordingStatus {
        RecordingStatus {
            recording: true,
            dir: self.dir.clone(),
            bytes: self.info.bytes,
            oldest_ts: self.info.oldest_ts,
            retention_seconds: self.limits.retention_seconds,
            max_bytes: self.limits.max_bytes,
            problem: self.problem.clone(),
        }
    }

    /// Append the open file of `stamp` to its sealed member, then drop
    /// the open file.
    fn seal(&mut self, stamp: &str) -> std::io::Result<()> {
        let open_path = self.dir.join(format!("{stamp}{OPEN_SUFFIX}"));
        let raw = match fs::read(&open_path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !raw.is_empty() {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
            encoder.write_all(&raw)?;
            let member = encoder.finish()?;
            let mut sealed = OpenOptions::new().create(true).append(true).open(self.dir.join(format!("{stamp}{SEALED_SUFFIX}")))?;
            sealed.write_all(&member)?;
            sealed.sync_data()?;
        }
        fs::remove_file(&open_path)?;
        if self.open.as_ref().is_some_and(|(open, _)| open == stamp) {
            self.open = None;
        }
        Ok(())
    }

    /// Age first, then size. Refreshes the cached store info.
    fn maintain(&mut self, now: f64) {
        self.last_maintenance = Instant::now();
        let oldest_allowed = now - self.limits.retention_seconds;
        for file in list_files(&self.dir) {
            if file.start + 3_600.0 <= oldest_allowed {
                let _ = fs::remove_file(&file.path);
            }
        }
        let mut files = list_files(&self.dir);
        let mut total: u64 = files.iter().map(|f| f.bytes).sum();
        let open_stamp = self.open.as_ref().map(|(stamp, _)| stamp.clone());
        while total > self.limits.max_bytes {
            // The oldest file that isn't the hour being written.
            let Some(index) = files.iter().position(|f| Some(&f.stamp) != open_stamp.as_ref()) else {
                break;
            };
            let file = files.remove(index);
            if fs::remove_file(&file.path).is_ok() {
                total = total.saturating_sub(file.bytes);
            }
        }
        if total > self.limits.max_bytes
            && let Some(stamp) = open_stamp
        {
            // Only the open hour is left and it is over the cap.
            if self.seal(&stamp).is_ok() {
                let files = list_files(&self.dir);
                total = files.iter().map(|f| f.bytes).sum();
                if total > self.limits.max_bytes {
                    for file in files {
                        let _ = fs::remove_file(&file.path);
                    }
                    total = 0;
                    self.problem = Some(format!(
                        "the size cap ({} MB) dropped the current hour; raise the cap to keep more",
                        self.limits.max_bytes / 1_000_000
                    ));
                }
            }
        }
        let files = list_files(&self.dir);
        self.info = StoreInfo {
            dir: self.dir.clone(),
            bytes: total.min(files.iter().map(|f| f.bytes).sum()),
            files: files.len(),
            oldest_ts: files.first().and_then(|f| first_ts(&f.path, f.sealed)),
            newest_ts: if self.last_ts > 0.0 { Some(self.last_ts) } else { None },
        };
    }
}

fn first_ts(path: &Path, sealed: bool) -> Option<f64> {
    let file = File::open(path).ok()?;
    let reader: Box<dyn Read> =
        if sealed { Box::new(flate2::read::MultiGzDecoder::new(file)) } else { Box::new(file) };
    BufReader::new(reader)
        .lines()
        .map_while(Result::ok)
        .find_map(|line| serde_json::from_str::<HistoryRecord>(&line).ok())
        .map(|record| record.ts)
}

/// What a read covered.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReadStats {
    /// Lines that did not parse: a torn last line while a writer is
    /// mid-append, or a crash-truncated gzip tail. Reported, never
    /// silently dropped.
    pub skipped_lines: usize,
}

/// Read-only access to a history directory (no lock needed).
pub struct Recording;

impl Recording {
    /// Every record with `from_ts <= ts <= to_ts`, oldest first, one per
    /// timestamp, handed to `each` one at a time (a week of samples is
    /// never all in memory at once).
    pub fn for_each(dir: &Path, from_ts: f64, to_ts: f64, mut each: impl FnMut(HistoryRecord)) -> ReadStats {
        let mut stats = ReadStats::default();
        let files = list_files(dir);
        let mut hours: Vec<f64> = files
            .iter()
            .map(|f| f.start)
            .filter(|start| start + 3_600.0 > from_ts && *start <= to_ts)
            .collect();
        hours.dedup();
        for hour in hours {
            let mut records = Vec::new();
            for file in files.iter().filter(|f| f.start == hour) {
                let Ok(handle) = File::open(&file.path) else { continue };
                let reader: Box<dyn Read> = if file.sealed {
                    Box::new(flate2::read::MultiGzDecoder::new(handle))
                } else {
                    Box::new(handle)
                };
                let mut lines = BufReader::new(reader).lines();
                loop {
                    match lines.next() {
                        None => break,
                        Some(Ok(line)) => match serde_json::from_str::<HistoryRecord>(&line) {
                            Ok(record) if record.ts >= from_ts && record.ts <= to_ts => records.push(record),
                            Ok(_) => {}
                            Err(_) if line.trim().is_empty() => {}
                            Err(_) => stats.skipped_lines += 1,
                        },
                        Some(Err(_)) => {
                            // A gzip member cut short by a crash: keep
                            // what decoded, count the loss once.
                            stats.skipped_lines += 1;
                            break;
                        }
                    }
                }
            }
            records.sort_by(|a, b| a.ts.total_cmp(&b.ts));
            records.dedup_by(|a, b| a.ts == b.ts);
            records.into_iter().for_each(&mut each);
        }
        stats
    }

    /// What is on disk: files, bytes, first and last record.
    pub fn info(dir: &Path) -> StoreInfo {
        let files = list_files(dir);
        let newest_ts = files.last().and_then(|last| {
            let mut newest = None;
            Recording::for_each(dir, last.start, f64::MAX, |record| newest = Some(record.ts));
            newest
        });
        StoreInfo {
            dir: dir.to_path_buf(),
            bytes: files.iter().map(|f| f.bytes).sum(),
            files: files.len(),
            oldest_ts: files.first().and_then(|f| first_ts(&f.path, f.sealed)),
            newest_ts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn all(dir: &Path) -> (Vec<HistoryRecord>, ReadStats) {
        let mut out = Vec::new();
        let stats = Recording::for_each(dir, 0.0, f64::MAX, |r| out.push(r));
        (out, stats)
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
        let mut recorder = Recorder::open(&dir, Limits::new(1e9, 1 << 30), "test").unwrap();
        for i in 0..10 {
            recorder.record(&record(start + f64::from(i) * 2.0, i as f32)).unwrap();
        }
        let names: Vec<String> = list_files(&dir).iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into()).collect();
        assert_eq!(names, ["2026-10-10T22.jsonl.gz", "2026-10-10T23.jsonl"], "22:xx sealed, 23:xx open");
        let (records, stats) = all(&dir);
        assert_eq!(records.len(), 10);
        assert_eq!(stats.skipped_lines, 0);
        assert!(records.windows(2).all(|w| w[0].ts < w[1].ts), "oldest first");
        // A stale or repeated sample is not a new line.
        recorder.record(&record(start, 99.0)).unwrap();
        assert_eq!(all(&dir).0.len(), 10);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn one_writer_at_a_time_and_the_lock_names_it() {
        let dir = scratch("lock");
        let first = Recorder::open(&dir, Limits::new(1e9, 1 << 30), "gui").unwrap();
        match Recorder::open(&dir, Limits::new(1e9, 1 << 30), "serve") {
            Err(OpenError::Busy(holder)) => assert_eq!(holder, format!("pid {} gui", std::process::id())),
            other => panic!("a second writer must be refused, got {:?}", other.map(|_| ())),
        }
        assert_eq!(current_writer(&dir), Some(format!("pid {} gui", std::process::id())));
        drop(first);
        assert_eq!(current_writer(&dir), None, "the lock goes with the writer");
        assert!(Recorder::open(&dir, Limits::new(1e9, 1 << 30), "serve").is_ok());
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
        let _recorder = Recorder::open(&dir, Limits::new(1e9, 1 << 30), "test").unwrap();
        assert!(dir.join(format!("{stamp}.jsonl.gz")).exists(), "sealed");
        assert!(!dir.join(format!("{stamp}.jsonl")).exists());
        let (records, stats) = all(&dir);
        assert_eq!(records.len(), 3);
        assert_eq!(stats.skipped_lines, 1, "the torn line is reported, not hidden");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn age_and_size_limits_are_kept() {
        let dir = scratch("limits");
        let now = crate::collect::unix_now();
        fs::create_dir_all(&dir).unwrap();
        // Three sealed hours: 50 h, 30 h and 2 h old.
        for age_hours in [50.0, 30.0, 2.0] {
            let ts = now - age_hours * 3_600.0;
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            for i in 0..2_000 {
                let line = serde_json::to_string(&record(ts + f64::from(i) * 0.5, (i % 100) as f32)).unwrap() + "\n";
                encoder.write_all(line.as_bytes()).unwrap();
            }
            fs::write(dir.join(format!("{}.jsonl.gz", hour_stamp(ts))), encoder.finish().unwrap()).unwrap();
        }
        // 48 h retention drops the 50 h file only.
        let mut recorder = Recorder::open(&dir, Limits::new(48.0 * 3_600.0, 1 << 30), "test").unwrap();
        assert_eq!(list_files(&dir).len(), 2);
        // A cap below the two files drops the older of them.
        let sizes: Vec<u64> = list_files(&dir).iter().map(|f| f.bytes).collect();
        recorder.set_limits(Limits { retention_seconds: 48.0 * 3_600.0, max_bytes: sizes[1] + 100 });
        // MIN_BYTES clamps a tiny cap, so test the clamp and the rule apart.
        assert_eq!(recorder.limits().max_bytes, Limits::MIN_BYTES.max(sizes[1] + 100));
        let mut squeezed = Recorder { limits: Limits { retention_seconds: 1e9, max_bytes: sizes[1] + 100 }, ..recorder };
        squeezed.maintain(now);
        let left = list_files(&dir);
        assert_eq!(left.len(), 1, "the cap removed the oldest hour");
        assert_eq!(left[0].bytes, sizes[1], "and kept the newest");
        assert!(squeezed.status().bytes <= sizes[1] + 100);
        let _ = fs::remove_dir_all(dir);
    }
}
