// SPDX-License-Identifier: GPL-3.0-or-later
//! The archive tier: one file per UTC day, `archive/2026-10-03.jsonl.zst`,
//! holding that day's hours once they are older than the hot window.
//! Lossless: every record line the recorder wrote is kept byte for
//! byte, oldest first, one per timestamp (a crash can leave the same
//! line in two places; the copies are identical, one is kept).
//!
//! ```text
//! skippable frame 0x184D2A5E   the index: JSON, one entry per hour
//!                              (offset, length, records, first/last ts,
//!                              raw size, FNV-1a of the raw lines)
//! skippable frame 0x184D2A5F   the hours' rollups (zstd-compressed JSON
//!                              lines, derived data, see rollup.rs)
//! zstd frame                   hour 00's lines, level 19
//! zstd frame                   hour 01's lines
//! …
//! ```
//!
//! Skippable frames are part of the zstd format: `zstd -dc` passes over
//! them, so `zstd -dc 2026-10-03.jsonl.zst` prints exactly the day's
//! JSON lines, readable without SysMon. One frame per hour costs ~1%
//! against one frame per day and lets a reader decode a single hour.
//!
//! Measured on this machine's real recording. One hour (716 samples,
//! 2.43 MB of raw lines): gzip -6 → 84 KB (118 B a sample, the hot
//! tier), zstd -19 → 55 KB (77 B), xz -9e → 52 KB (72 B). zstd wins on
//! decode speed (an hour in ~2 ms) and is one small static dependency;
//! xz would be ~6% smaller. A whole packed day file (4.6 h, 2,781
//! samples, idle and gaming): 103 B a sample, of which the hour frames
//! are 94 B, the rollups 8.4 B, the index 0.3 B; the gzip hours it
//! replaced were 141 B a sample.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::rollup::HourRollup;

pub const ARCHIVE_DIR: &str = "archive";
pub const DAY_SUFFIX: &str = ".jsonl.zst";
/// zstd level for archived hours: the densest that still compresses an
/// hour of 2 s samples in about a second, on a background thread at
/// nice 19 (decode speed barely depends on the level).
pub const ARCHIVE_LEVEL: i32 = 19;
const INDEX_MAGIC: u32 = 0x184D_2A5E;
const ROLLUPS_MAGIC: u32 = 0x184D_2A5F;
const FORMAT: &str = "sysmon-history-archive";

/// One archived hour, as the index lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HourEntry {
    /// The hour's start, unix seconds.
    pub start: f64,
    /// Byte offset of the hour's zstd frame from the end of the index.
    pub offset: u64,
    pub length: u64,
    pub records: u64,
    pub first_ts: f64,
    pub last_ts: f64,
    /// The decoded size of the hour's lines.
    pub raw_bytes: u64,
    /// FNV-1a 64 of the decoded lines, hex.
    pub fnv64: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DayIndex {
    pub format: String,
    pub version: u32,
    /// "2026-10-03" (UTC).
    pub day: String,
    pub hours: Vec<HourEntry>,
    /// Offset and length of the rollups frame from the end of the index.
    pub rollups_offset: u64,
    pub rollups_length: u64,
    /// Where the data section starts (set when read, not stored).
    #[serde(skip)]
    pub data_start: u64,
}

/// An hour ready to be written: its compressed frame, its rollup line,
/// its index entry (offset filled in by `write_day`).
pub struct HourBlob {
    pub entry: HourEntry,
    pub frame: Vec<u8>,
    pub rollup_line: String,
}

/// FNV-1a, 64 bit: a cheap check that a repacked hour decodes to the
/// bytes that went in (integrity, not security).
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Compress one hour's lines and describe them.
pub fn hour_blob(
    compressor: &mut zstd::bulk::Compressor<'_>,
    start: f64,
    lines: &[u8],
    records: u64,
    first_ts: f64,
    last_ts: f64,
    rollup: Option<&HourRollup>,
) -> std::io::Result<HourBlob> {
    let frame = compressor.compress(lines)?;
    Ok(HourBlob {
        entry: HourEntry {
            start,
            offset: 0,
            length: frame.len() as u64,
            records,
            first_ts,
            last_ts,
            raw_bytes: lines.len() as u64,
            fnv64: format!("{:016x}", fnv64(lines)),
        },
        frame,
        rollup_line: rollup.map(HourRollup::to_line).unwrap_or_default(),
    })
}

/// A compressor with checksums on (a flipped bit on disk is an error,
/// not a silently wrong number). At level 19, window, chain and hash
/// logs 22/22/21 instead of the level's 23/23/22: the same bytes out on
/// a real hour (≤ 6 MB of lines even at 1 s), ~32 MB peak memory
/// instead of ~57 MB, for a recorder that idles at 5 MB.
pub fn compressor_at(level: i32) -> std::io::Result<zstd::bulk::Compressor<'static>> {
    use zstd::zstd_safe::CParameter;
    let mut compressor = zstd::bulk::Compressor::new(level)?;
    compressor.include_checksum(true)?;
    if level >= 16 {
        compressor.set_parameter(CParameter::WindowLog(22))?;
        compressor.set_parameter(CParameter::ChainLog(22))?;
        compressor.set_parameter(CParameter::HashLog(21))?;
    }
    Ok(compressor)
}

/// "2026-10-03" for a unix time (UTC).
pub fn day_stamp(ts: f64) -> String {
    let (year, month, day, _) = super::utc_civil(ts);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The start of a "2026-10-03" day, None for a name that isn't one.
pub fn parse_day_stamp(stamp: &str) -> Option<f64> {
    let mut parts = stamp.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(super::utc_hour_start(year, month, day, 0))
}

pub fn day_path(dir: &Path, day: &str) -> PathBuf {
    dir.join(ARCHIVE_DIR).join(format!("{day}{DAY_SUFFIX}"))
}

fn skippable(magic: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&magic.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Write a day file at `path` (any name; the caller renames it into
/// place). Hours must be in time order. Returns the bytes written.
pub fn write_day(path: &Path, day: &str, mut hours: Vec<HourBlob>) -> std::io::Result<u64> {
    let mut rollups = String::new();
    for hour in &hours {
        rollups.push_str(&hour.rollup_line);
        rollups.push('\n');
    }
    // Level 19 like the hours: ~9% smaller than level 9 on real rollups.
    let rollups = zstd::bulk::compress(rollups.as_bytes(), 19)?;
    let rollups_frame = skippable(ROLLUPS_MAGIC, &rollups);
    let mut offset = rollups_frame.len() as u64;
    for hour in &mut hours {
        hour.entry.offset = offset;
        offset += hour.frame.len() as u64;
    }
    let index = DayIndex {
        format: FORMAT.to_string(),
        version: 1,
        day: day.to_string(),
        hours: hours.iter().map(|h| h.entry.clone()).collect(),
        rollups_offset: 8,
        rollups_length: rollups.len() as u64,
        data_start: 0,
    };
    let index = serde_json::to_vec(&index).map_err(std::io::Error::other)?;
    let mut file = File::create(path)?;
    let index_frame = skippable(INDEX_MAGIC, &index);
    file.write_all(&index_frame)?;
    file.write_all(&rollups_frame)?;
    for hour in &hours {
        file.write_all(&hour.frame)?;
    }
    file.sync_all()?;
    Ok(index_frame.len() as u64 + offset)
}

fn read_u32(file: &mut File) -> std::io::Result<u32> {
    let mut bytes = [0u8; 4];
    file.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

/// The index of a day file.
pub fn read_index(path: &Path) -> std::io::Result<DayIndex> {
    let mut file = File::open(path)?;
    if read_u32(&mut file)? != INDEX_MAGIC {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not a SysMon archive day (no index frame)"));
    }
    let size = read_u32(&mut file)? as usize;
    if size > 16 << 20 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "archive index too large"));
    }
    let mut payload = vec![0u8; size];
    file.read_exact(&mut payload)?;
    let mut index: DayIndex = serde_json::from_slice(&payload).map_err(std::io::Error::other)?;
    if index.format != FORMAT {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not a SysMon archive day"));
    }
    index.data_start = 8 + size as u64;
    Ok(index)
}

/// The day's rollups, one per hour in index order (None where an
/// hour's line did not parse: its records are read instead).
pub fn read_rollups(path: &Path, index: &DayIndex) -> std::io::Result<Vec<Option<HourRollup>>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(index.data_start + index.rollups_offset))?;
    let mut compressed = vec![0u8; index.rollups_length as usize];
    file.read_exact(&mut compressed)?;
    let text = zstd::stream::decode_all(&compressed[..])?;
    let text = String::from_utf8_lossy(&text);
    let mut rollups: Vec<Option<HourRollup>> = text.lines().map(HourRollup::from_line).collect();
    rollups.resize(index.hours.len(), None);
    Ok(rollups)
}

/// One hour's lines, decoded and checked against the index.
pub fn read_hour(file: &mut File, index: &DayIndex, entry: &HourEntry) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(index.data_start + entry.offset))?;
    let mut frame = vec![0u8; entry.length as usize];
    file.read_exact(&mut frame)?;
    let lines = zstd::bulk::decompress(&frame, entry.raw_bytes as usize)?;
    if lines.len() as u64 != entry.raw_bytes {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "archived hour decoded to the wrong size"));
    }
    Ok(lines)
}

/// Decode every hour and check sizes and checksums: the repack calls
/// this on the new file before any source is removed.
pub fn verify_day(path: &Path) -> std::io::Result<DayIndex> {
    let index = read_index(path)?;
    let mut file = File::open(path)?;
    for entry in &index.hours {
        let lines = read_hour(&mut file, &index, entry)?;
        if format!("{:016x}", fnv64(&lines)) != entry.fnv64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("archived hour {} does not decode to what was written", entry.start),
            ));
        }
        let count = lines.split(|b| *b == b'\n').filter(|l| !l.is_empty()).count() as u64;
        if count != entry.records {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "archived hour lost lines"));
        }
    }
    let rollups = read_rollups(path, &index)?;
    if rollups.len() != index.hours.len() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "archive rollups do not match its hours"));
    }
    Ok(index)
}

/// Archive day files, oldest first: (day start, day stamp, path, bytes).
pub fn list_days(dir: &Path) -> Vec<(f64, String, PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(dir.join(ARCHIVE_DIR)) else {
        return Vec::new();
    };
    let mut days: Vec<_> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let stamp = name.strip_suffix(DAY_SUFFIX)?.to_string();
            let start = parse_day_stamp(&stamp)?;
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            Some((start, stamp, entry.path(), bytes))
        })
        .collect();
    days.sort_by(|a, b| a.0.total_cmp(&b.0));
    days
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_file_round_trips_and_plain_zstd_reads_it() {
        let dir = std::env::temp_dir().join(format!("sysmon-archive-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(ARCHIVE_DIR)).unwrap();
        let mut compressor = compressor_at(ARCHIVE_LEVEL).unwrap();
        let hour = |h: u32, compressor: &mut zstd::bulk::Compressor<'_>| {
            let lines = format!("{{\"ts\":{}.5,\"interval_seconds\":2.0}}\n{{\"ts\":{}.5,\"interval_seconds\":2.0}}\n", 3600 * h, 3600 * h + 2);
            hour_blob(compressor, f64::from(3600 * h), lines.as_bytes(), 2, f64::from(3600 * h) + 0.5, f64::from(3600 * h) + 2.5, None).unwrap()
        };
        let blobs = vec![hour(0, &mut compressor), hour(1, &mut compressor)];
        let path = day_path(&dir, "1970-01-01");
        write_day(&path, "1970-01-01", blobs).unwrap();
        let index = verify_day(&path).unwrap();
        assert_eq!(index.hours.len(), 2);
        assert_eq!(index.hours[1].start, 3600.0);
        let mut file = File::open(&path).unwrap();
        let second = read_hour(&mut file, &index, &index.hours[1]).unwrap();
        assert!(String::from_utf8(second).unwrap().starts_with("{\"ts\":3600.5,"));
        // The whole file is a valid zstd stream of the day's lines: the
        // index and rollups are skippable frames.
        let plain = zstd::stream::decode_all(File::open(&path).unwrap()).unwrap();
        assert_eq!(String::from_utf8(plain).unwrap().lines().count(), 4);
        assert_eq!(list_days(&dir).len(), 1);
        assert_eq!(parse_day_stamp("1970-01-02"), Some(86_400.0));
        assert_eq!(day_stamp(86_399.9), "1970-01-01");
        // A flipped byte in an hour is caught before anything trusts it.
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 6;
        bytes[last] ^= 0x55;
        fs::write(&path, bytes).unwrap();
        assert!(verify_day(&path).is_err(), "corruption must not verify");
        let _ = fs::remove_dir_all(dir);
    }
}
