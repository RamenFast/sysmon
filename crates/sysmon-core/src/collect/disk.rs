// SPDX-License-Identifier: GPL-3.0-or-later
//! Disks: mounted real block devices (mountinfo + statvfs) with
//! throughput and utilisation from /proc/diskstats deltas.
//!
//! Only real /dev/* devices are listed — no loop devices, tmpfs, or
//! bind-mount duplicates (first mount of a device wins). A partition
//! that has no diskstats row of its own borrows its parent disk's
//! counters (nvme0n1p2 → nvme0n1, sda1 → sda), exactly like v1.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::snapshot::DiskSnapshot;

/// One mounted filesystem from /proc/self/mountinfo.
#[derive(Clone, Debug, PartialEq)]
pub struct MountEntry {
    pub device: String,
    pub mount_point: String,
    pub fs_type: String,
}

/// mountinfo format: `id parent major:minor root mountpoint options
/// [optional…] - fstype source superopts`. Octal escapes (\040 for
/// space) appear in mount points.
pub fn parse_mountinfo(content: &str) -> Vec<MountEntry> {
    let mut entries = Vec::new();
    for line in content.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let left_fields: Vec<&str> = left.split(' ').collect();
        let right_fields: Vec<&str> = right.split(' ').collect();
        if left_fields.len() < 5 || right_fields.len() < 2 {
            continue;
        }
        entries.push(MountEntry {
            mount_point: unescape_octal(left_fields[4]),
            fs_type: right_fields[0].to_string(),
            device: unescape_octal(right_fields[1]),
        });
    }
    entries
}

/// `\040` → space, etc. (kernel escapes in mountinfo).
fn unescape_octal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(value as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// One device's counters from /proc/diskstats, in bytes / ms.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DiskCounters {
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub busy_ms: u64,
}

const SECTOR_BYTES: u64 = 512;

/// diskstats columns after `major minor name`: 1 reads, 2 merged,
/// 3 sectors read, 4 ms reading, 5 writes, 6 merged, 7 sectors
/// written, 8 ms writing, 9 io in flight, 10 ms doing io, …
pub fn parse_diskstats(content: &str) -> HashMap<String, DiskCounters> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let fields: Vec<&str> = line.split_ascii_whitespace().collect();
        if fields.len() < 13 {
            continue;
        }
        let name = fields[2].to_string();
        let value = |index: usize| -> u64 { fields[index].parse().unwrap_or(0) };
        map.insert(
            name,
            DiskCounters {
                read_bytes: value(5) * SECTOR_BYTES,
                write_bytes: value(9) * SECTOR_BYTES,
                busy_ms: value(12),
            },
        );
    }
    map
}

/// The diskstats key for a device path like /dev/nvme0n1p2 or
/// /dev/sda1 — itself, else its parent disk.
pub fn counter_key_for_device(
    device: &str,
    counters: &HashMap<String, DiskCounters>,
) -> Option<String> {
    let real = fs::canonicalize(device).unwrap_or_else(|_| device.into());
    let name = real.file_name()?.to_string_lossy().to_string();
    if counters.contains_key(&name) {
        return Some(name);
    }
    let parent = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if let Some(stripped) = parent.strip_suffix('p')
        && counters.contains_key(stripped)
    {
        return Some(stripped.to_string());
    }
    if counters.contains_key(parent) {
        return Some(parent.to_string());
    }
    None
}

/// Filesystem labels by real device path, from /dev/disk/by-label.
/// udev escapes odd bytes as \xNN.
fn labels_by_device() -> HashMap<String, String> {
    let mut labels = HashMap::new();
    if let Ok(entries) = fs::read_dir("/dev/disk/by-label") {
        for entry in entries.flatten() {
            let label_name = entry.file_name().to_string_lossy().to_string();
            if let Ok(real) = fs::canonicalize(entry.path()) {
                labels.insert(
                    real.to_string_lossy().to_string(),
                    unescape_udev(&label_name),
                );
            }
        }
    }
    labels
}

/// `\x20` → space (udev by-label escaping).
pub fn unescape_udev(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1] == b'x' {
            if let Ok(value) = u8::from_str_radix(&s[i + 2..i + 4], 16) {
                out.push(value as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn statvfs_usage(mount_point: &str) -> Option<(u64, u64)> {
    let c_path = CString::new(Path::new(mount_point).as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) };
    if result != 0 {
        return None;
    }
    let fragment = stats.f_frsize as u64;
    let total = stats.f_blocks as u64 * fragment;
    // df's "used": blocks minus free-for-root.
    let used = (stats.f_blocks as u64).saturating_sub(stats.f_bfree as u64) * fragment;
    Some((used, total))
}

pub struct DiskCollector {
    previous: HashMap<String, DiskCounters>,
}

impl DiskCollector {
    pub fn new() -> Self {
        DiskCollector {
            previous: HashMap::new(),
        }
    }

    pub fn collect(&mut self, interval_seconds: f64) -> Vec<DiskSnapshot> {
        let mounts = fs::read_to_string("/proc/self/mountinfo")
            .map(|content| parse_mountinfo(&content))
            .unwrap_or_default();
        let counters = fs::read_to_string("/proc/diskstats")
            .map(|content| parse_diskstats(&content))
            .unwrap_or_default();
        let labels = labels_by_device();

        let mut disks = Vec::new();
        let mut seen_devices = Vec::new();
        for mount in &mounts {
            if !mount.device.starts_with("/dev/") || mount.device.contains("/loop") {
                continue;
            }
            let real_device = fs::canonicalize(&mount.device)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| mount.device.clone());
            if seen_devices.contains(&real_device) {
                continue; // bind mounts / btrfs subvolumes of one device
            }
            seen_devices.push(real_device.clone());

            let Some((used_bytes, total_bytes)) = statvfs_usage(&mount.mount_point) else {
                continue;
            };
            if total_bytes == 0 {
                continue;
            }

            let mut read_bps = 0.0;
            let mut write_bps = 0.0;
            let mut util_percent = 0.0;
            if let Some(key) = counter_key_for_device(&mount.device, &counters) {
                let current = counters[&key];
                if let Some(previous) = self.previous.get(&key)
                    && interval_seconds > 0.0
                {
                    read_bps = current.read_bytes.saturating_sub(previous.read_bytes) as f64
                        / interval_seconds;
                    write_bps = current.write_bytes.saturating_sub(previous.write_bytes) as f64
                        / interval_seconds;
                    util_percent = (current.busy_ms.saturating_sub(previous.busy_ms) as f64
                        / (interval_seconds * 1000.0)
                        * 100.0)
                        .clamp(0.0, 100.0) as f32;
                }
                self.previous.insert(key, current);
            }

            let device_basename = real_device
                .rsplit('/')
                .next()
                .unwrap_or(&real_device)
                .to_string();
            disks.push(DiskSnapshot {
                display_name: labels
                    .get(&real_device)
                    .cloned()
                    .unwrap_or(device_basename),
                device: mount.device.clone(),
                mount_point: mount.mount_point.clone(),
                fs_type: mount.fs_type.clone(),
                read_bps,
                write_bps,
                used_bytes,
                total_bytes,
                util_percent,
            });
        }

        disks.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
        disks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_parses_and_unescapes() {
        let content = "36 25 8:2 / / rw,relatime shared:1 - ext4 /dev/sdb2 rw\n\
                       99 25 8:17 / /mnt/Mass\\040storage rw - ext4 /dev/sdc1 rw\n\
                       40 25 0:33 / /run/user/1000 rw shared:2 - tmpfs tmpfs rw\n";
        let entries = parse_mountinfo(content);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].device, "/dev/sdb2");
        assert_eq!(entries[0].fs_type, "ext4");
        assert_eq!(entries[1].mount_point, "/mnt/Mass storage");
    }

    #[test]
    fn diskstats_reads_sectors_and_busy_ms() {
        let content =
            "   8       2 sdb2 1000 5 2048 300 500 9 4096 200 0 750 500 0 0 0 0 0 0\n";
        let map = parse_diskstats(content);
        let counters = map["sdb2"];
        assert_eq!(counters.read_bytes, 2048 * 512);
        assert_eq!(counters.write_bytes, 4096 * 512);
        assert_eq!(counters.busy_ms, 750);
    }

    #[test]
    fn partition_falls_back_to_parent_disk() {
        let mut counters = HashMap::new();
        counters.insert("nvme0n1".to_string(), DiskCounters::default());
        counters.insert("sda".to_string(), DiskCounters::default());
        assert_eq!(
            counter_key_for_device("/dev/nvme0n1p2", &counters).as_deref(),
            Some("nvme0n1")
        );
        assert_eq!(
            counter_key_for_device("/dev/sda1", &counters).as_deref(),
            Some("sda")
        );
    }

    #[test]
    fn udev_label_unescaping() {
        assert_eq!(unescape_udev("Mass\\x20storage"), "Mass storage");
        assert_eq!(unescape_udev("plain"), "plain");
    }
}
