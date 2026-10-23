// SPDX-License-Identifier: GPL-3.0-or-later
//! The accuracy-discrepancy suite: every number SysMon reports is
//! cross-checked live against an independent authority — the same
//! tools a skeptical human would open next to it (free, df, ps) and
//! direct /proc & /sys re-reads.
//!
//! Tolerances exist because the machine keeps running between two
//! reads of the same counter; each one is justified in place. A
//! failure here is a real discrepancy: fix the collector, never the
//! tolerance (the tolerance is the contract).

use std::process::Command;
use std::time::Duration;

use sysmon_core::collect::Sampler;
use sysmon_core::snapshot::{SystemSnapshot, Wants};

fn sampled(wants: Wants, window_ms: u64) -> SystemSnapshot {
    let mut sampler = Sampler::new();
    let _prime = sampler.sample(wants);
    std::thread::sleep(Duration::from_millis(window_ms));
    sampler.sample(wants)
}

fn shell(command: &str) -> String {
    let output = Command::new("sh").args(["-c", command]).output().expect("run");
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn memory_matches_free_dash_b() {
    let mut wants = Wants::none();
    wants.memory = true;
    let ours = sampled(wants, 50).memory.expect("memory");

    // free -b row: total used free shared buff/cache available
    let free_output = shell("free -b | awk '/^Mem:/ {print $2, $6, $7}'");
    let fields: Vec<u64> = free_output
        .split_ascii_whitespace()
        .map(|f| f.parse().expect("free field"))
        .collect();
    let (free_total, free_buff_cache, free_available) = (fields[0], fields[1], fields[2]);

    // Total RAM is static — must agree to the byte.
    assert_eq!(ours.total_bytes, free_total, "MemTotal must match free -b exactly");

    // Available moves as programs allocate: 3% of RAM covers the
    // biggest swing seen between two reads on a busy desktop.
    let tolerance = free_total / 33;
    assert!(
        ours.available_bytes.abs_diff(free_available) < tolerance,
        "available: ours {} vs free {} (tolerance {tolerance})",
        ours.available_bytes,
        free_available
    );

    // free's buff/cache = Buffers + Cached + SReclaimable; ours is
    // split into buffers_bytes + cached_bytes. Same identity.
    let ours_buff_cache = ours.buffers_bytes + ours.cached_bytes;
    assert!(
        ours_buff_cache.abs_diff(free_buff_cache) < tolerance,
        "buff/cache: ours {} vs free {}",
        ours_buff_cache,
        free_buff_cache
    );
}

#[test]
fn disks_match_df_dash_b1() {
    let mut wants = Wants::none();
    wants.disks = true;
    let ours = sampled(wants, 50).disks.expect("disks");

    for disk in &ours {
        let df_output = shell(&format!(
            "df -B1 --output=size,used '{}' | tail -1",
            disk.mount_point
        ));
        let fields: Vec<u64> = df_output
            .split_ascii_whitespace()
            .map(|f| f.parse().unwrap_or(0))
            .collect();
        if fields.len() != 2 {
            continue;
        }
        let (df_total, df_used) = (fields[0], fields[1]);
        assert_eq!(
            disk.total_bytes, df_total,
            "{}: filesystem size must match df exactly",
            disk.mount_point
        );
        // Used moves with writes between the two reads: 0.5% + 32 MiB.
        let tolerance = df_total / 200 + (32 << 20);
        assert!(
            disk.used_bytes.abs_diff(df_used) < tolerance,
            "{}: used ours {} vs df {}",
            disk.mount_point,
            disk.used_bytes,
            df_used
        );
    }
    assert!(
        ours.iter().any(|d| d.mount_point == "/"),
        "root filesystem must be listed"
    );
}

#[test]
fn cpu_busy_registers_a_pinned_core_and_stays_consistent() {
    let mut wants = Wants::none();
    wants.cpu = true;

    // Pin one core at 100% for the whole window.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let spinner_stop = stop.clone();
    let spinner = std::thread::spawn(move || {
        while !spinner_stop.load(std::sync::atomic::Ordering::Relaxed) {
            std::hint::black_box(1u64.wrapping_mul(3));
        }
    });

    let cpu = sampled(wants, 600).cpu.expect("cpu");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    spinner.join().unwrap();

    let cores = cpu.core_count as f32;
    assert!(cores >= 1.0);
    // One pinned core contributes 100/cores points; allow scheduler
    // wobble down to 60% of that.
    let expected_floor = 100.0 / cores * 0.6;
    assert!(
        cpu.overall_percent >= expected_floor,
        "pinned core invisible: overall {}% with {} cores",
        cpu.overall_percent,
        cores
    );
    // Internal consistency: overall is the mean of per-core (±1pp of
    // rounding drift — they come from the same /proc/stat read).
    let mean = cpu.per_core_percent.iter().sum::<f32>() / cores;
    assert!(
        (mean - cpu.overall_percent).abs() < 1.0,
        "overall {} vs per-core mean {}",
        cpu.overall_percent,
        mean
    );
}

#[test]
fn own_rss_matches_ps() {
    // ps before and after the sample brackets ours. Other tests in this
    // binary run on sibling threads of this same process, and one of
    // them loads NVML (~30 MB in one step, 3.7), so a single ps read
    // can land on either side of that step. The tolerance is unchanged.
    let pid = std::process::id() as i32;
    let ps_rss = || -> u64 {
        let kib: u64 = shell(&format!("ps -o rss= -p {pid}")).trim().parse().expect("ps rss");
        kib * 1024
    };
    let before = ps_rss();

    let mut wants = Wants::none();
    wants.processes = true;
    wants.gpu = true;
    let snapshot = sampled(wants, 50);
    let after = ps_rss();
    let me = snapshot
        .processes
        .as_ref()
        .unwrap()
        .iter()
        .find(|p| p.pid == pid)
        .expect("own process record");

    let (low, high) = (before.min(after), before.max(after));
    // 8 MiB or 10%: the test allocates while running. (Since 3.1 the
    // value comes from statm, the counter ps itself reads; before, the
    // approximate stat field ran up to 6 MiB low on busy processes.)
    let tolerance = (high / 10).max(8 << 20);
    assert!(
        me.memory_rss_bytes + tolerance > low && me.memory_rss_bytes < high + tolerance,
        "own RSS ours {} vs ps {before} before / {after} after",
        me.memory_rss_bytes,
    );
    assert_eq!(me.user, shell("id -un").trim(), "own user must match id -un");
}

#[test]
fn process_census_matches_ps_dash_e() {
    let mut wants = Wants::none();
    wants.processes = true;
    wants.gpu = true;
    let snapshot = sampled(wants, 50);
    let ours = snapshot.processes.as_ref().unwrap().len() as i64;
    let ps_count: i64 = shell("ps -e --no-headers | wc -l").trim().parse().unwrap();
    // Processes spawn and die between the scans; a busy desktop
    // churns a few dozen a second at worst.
    assert!(
        (ours - ps_count).abs() < 50,
        "process count ours {ours} vs ps {ps_count}"
    );
}

#[test]
fn network_totals_match_proc_net_dev() {
    let mut wants = Wants::none();
    wants.network = true;
    let ours = sampled(wants, 50).network.expect("network");

    // Independent parse: sum rx/tx for interfaces with a real device
    // behind them (same physical-only definition).
    let mut expected_rx = 0u64;
    let mut expected_tx = 0u64;
    let content = std::fs::read_to_string("/proc/net/dev").unwrap();
    for line in content.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !std::path::Path::new(&format!("/sys/class/net/{name}/device")).exists() {
            continue;
        }
        let fields: Vec<u64> = rest
            .split_ascii_whitespace()
            .map(|f| f.parse().unwrap_or(0))
            .collect();
        expected_rx += fields[0];
        expected_tx += fields[8];
    }
    // Traffic flows between the reads: 16 MiB covers a fast download.
    let tolerance = 16u64 << 20;
    assert!(
        ours.total_received_bytes.abs_diff(expected_rx) < tolerance,
        "rx total ours {} vs /proc/net/dev {}",
        ours.total_received_bytes,
        expected_rx
    );
    assert!(
        ours.total_sent_bytes.abs_diff(expected_tx) < tolerance,
        "tx total ours {} vs /proc/net/dev {}",
        ours.total_sent_bytes,
        expected_tx
    );
}

#[test]
fn gpu_matches_sysfs_re_read() {
    let mut wants = Wants::none();
    wants.gpu = true;
    let ours = sampled(wants, 50).gpu.expect("gpu section");
    if !ours.available {
        eprintln!("no amdgpu card here — skipping (honest empty state is its own test)");
        return;
    }

    // Find the card the same way a human would.
    let device = (0..8)
        .map(|n| format!("/sys/class/drm/card{n}/device"))
        .find(|d| {
            std::fs::read_to_string(format!("{d}/vendor"))
                .map(|v| v.trim() == "0x1002")
                .unwrap_or(false)
                && std::path::Path::new(&format!("{d}/gpu_busy_percent")).exists()
        })
        .expect("amdgpu sysfs device");

    let sysfs_vram_total: u64 = std::fs::read_to_string(format!("{device}/mem_info_vram_total"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        ours.vram_total_bytes, sysfs_vram_total,
        "VRAM size is static and must match sysfs exactly"
    );

    let sysfs_vram_used: u64 = std::fs::read_to_string(format!("{device}/mem_info_vram_used"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Allocations move: 256 MiB tolerance.
    assert!(
        ours.vram_used_bytes.abs_diff(sysfs_vram_used) < (256 << 20),
        "VRAM used ours {} vs sysfs {}",
        ours.vram_used_bytes,
        sysfs_vram_used
    );

    // Per-process VRAM (seam #196: 3.4.0 said sway held 1.98 TB of a
    // 32 GB card). No process can hold more than the card, and the
    // unshared parts of every process are buffers in VRAM, so they sum
    // to at most what the card says is used. Shared buffers are left
    // out of the sum because they legitimately count in each process
    // that maps them. Same 256 MiB for allocations moving between reads.
    for process in &ours.processes {
        assert!(
            process.vram_bytes <= ours.vram_total_bytes,
            "pid {} holds {} bytes of VRAM on a {}-byte card",
            process.pid,
            process.vram_bytes,
            ours.vram_total_bytes
        );
        assert!(process.vram_shared_bytes <= process.vram_bytes, "pid {} shares more than it holds", process.pid);
    }
    let unshared: u64 = ours.processes.iter().map(|p| p.vram_bytes - p.vram_shared_bytes).sum();
    assert!(
        unshared <= sysfs_vram_used + (256 << 20),
        "per-process unshared VRAM sums to {unshared}, the card has {sysfs_vram_used} in use"
    );

    // Temperature drifts a degree or two between reads, not ten.
    if let Some(edge) = ours.temperature_edge_celsius {
        let sysfs_edge: f32 = std::fs::read_to_string(format!("{device}/hwmon/hwmon2/temp1_input"))
            .or_else(|_| {
                // hwmon index varies; find it.
                let hwmon_dir = std::fs::read_dir(format!("{device}/hwmon"))
                    .unwrap()
                    .flatten()
                    .next()
                    .unwrap()
                    .path();
                std::fs::read_to_string(hwmon_dir.join("temp1_input"))
            })
            .unwrap()
            .trim()
            .parse::<f32>()
            .unwrap()
            / 1000.0;
        assert!(
            (edge - sysfs_edge).abs() < 10.0,
            "edge temp ours {edge} vs sysfs {sysfs_edge}"
        );
    }
}

#[test]
fn load_and_uptime_match_proc() {
    let mut wants = Wants::none();
    wants.cpu = true;
    wants.system = true;
    let snapshot = sampled(wants, 50);
    let cpu = snapshot.cpu.as_ref().unwrap();
    let system = snapshot.system.as_ref().unwrap();

    let loadavg = std::fs::read_to_string("/proc/loadavg").unwrap();
    let expected_1m: f64 = loadavg.split_whitespace().next().unwrap().parse().unwrap();
    // loadavg re-computes on 5s ticks; one tick may land between the
    // reads. 1-minute load moves at most ~0.2 per tick at load 12.
    assert!(
        (cpu.load_1m - expected_1m).abs() < 0.5,
        "load 1m ours {} vs /proc {}",
        cpu.load_1m,
        expected_1m
    );

    let uptime: f64 = std::fs::read_to_string("/proc/uptime")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (uptime - system.uptime_seconds).abs() < 3.0,
        "uptime ours {} vs /proc {}",
        system.uptime_seconds,
        uptime
    );
    // boot_ts + uptime ≈ now.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    assert!(
        (system.boot_ts + uptime - now).abs() < 5.0,
        "btime + uptime should be now: {} + {} vs {}",
        system.boot_ts,
        uptime,
        now
    );
}

#[test]
fn sensor_channels_match_sysfs_files() {
    // Identity: one reading per readable hwmon channel file, per
    // family — temps/fans/voltages sum over `<prefix>N_input`,
    // power over `powerN_average`-or-`powerN_input` (dedup, the
    // collector's rule). Channel indices GAP (k10temp: temp1 Tctl,
    // temp3 Tccd1, no temp2); counting files instead of counting up
    // from 1 is exactly the regression this test pins.
    let mut wants = Wants::none();
    wants.sensors = true;
    let sensors = sampled(wants, 0).sensors.expect("sensors");

    let readable = |dir: &std::path::Path, prefix: &str, suffixes: &[&str]| -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut indices: Vec<u32> = entries
            .flatten()
            .filter_map(|entry| {
                let file = entry.file_name();
                let file = file.to_string_lossy();
                let rest = file.strip_prefix(prefix)?;
                let index = suffixes
                    .iter()
                    .find_map(|suffix| rest.strip_suffix(suffix))?
                    .parse::<u32>()
                    .ok()?;
                std::fs::read_to_string(entry.path()).ok().map(|_| index)
            })
            .collect();
        indices.sort_unstable();
        indices.dedup();
        indices.len()
    };

    let mut expected_temps = 0;
    let mut expected_fans = 0;
    let mut expected_voltages = 0;
    let mut expected_power = 0;
    for entry in std::fs::read_dir("/sys/class/hwmon").expect("hwmon").flatten() {
        let dir = entry.path();
        expected_temps += readable(&dir, "temp", &["_input"]);
        expected_fans += readable(&dir, "fan", &["_input"]);
        expected_voltages += readable(&dir, "in", &["_input"]);
        expected_power += readable(&dir, "power", &["_average", "_input"]);
    }

    let temps: usize = sensors.chips.iter().map(|chip| chip.temps.len()).sum();
    let fans: usize = sensors.chips.iter().map(|chip| chip.fans.len()).sum();
    let voltages: usize = sensors.chips.iter().map(|chip| chip.voltages.len()).sum();
    let power: usize = sensors.chips.iter().map(|chip| chip.power.len()).sum();
    assert_eq!(temps, expected_temps, "one TempReading per readable tempN_input");
    assert_eq!(fans, expected_fans, "one FanReading per readable fanN_input");
    assert_eq!(voltages, expected_voltages, "one VoltageReading per readable inN_input");
    assert_eq!(power, expected_power, "one PowerReading per readable power channel");

    // Every reading names itself uniquely: four NVMe channels used to
    // render as four identical "nvme0n1 · CT2000P3PSSD8" rows.
    let mut identities: Vec<String> = sensors
        .chips
        .iter()
        .flat_map(|chip| {
            chip.temps.iter().map(move |t| {
                format!("{}|{}|{}", chip.name, chip.device.as_deref().unwrap_or(""), t.label)
            })
        })
        .collect();
    let total = identities.len();
    identities.sort();
    identities.dedup();
    assert_eq!(identities.len(), total, "two temperature readings share one identity");

    // A limit no sensor could reach is a "not set" sentinel, never a
    // reported threshold (NVMe: 65261.85 °C).
    for chip in &sensors.chips {
        for t in &chip.temps {
            for limit in [t.max_celsius, t.crit_celsius].into_iter().flatten() {
                assert!(limit < 200.0, "{} {}: absurd limit {limit} °C", chip.name, t.label);
            }
            // Plausibility is a pure function of the reading.
            assert_eq!(t.plausible, sysmon_core::collect::sensors::temperature_is_plausible(t.celsius));
        }
    }
}

/// Installed RAM is the sum of the SMBIOS memory devices (udev's DMI
/// export), and the hierarchy holds: usable (MemTotal) ≤ what the
/// firmware hands the OS (memmap "System RAM") ≤ installed.
#[test]
fn installed_memory_matches_firmware_tables() {
    let Ok(dmi) = std::fs::read_to_string("/run/udev/data/+dmi:id") else {
        eprintln!("no udev DMI database here (VM/container) — skipping");
        return;
    };
    let expected: u64 = dmi
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("E:MEMORY_DEVICE_")?;
            let (key, value) = rest.split_once('=')?;
            key.ends_with("_SIZE")
                .then(|| key.split('_').nth(1) == Some("SIZE"))
                .filter(|is_size| *is_size)
                .and_then(|_| value.parse::<u64>().ok())
        })
        .sum();
    let mut wants = Wants::none();
    wants.memory = true;
    let memory = sampled(wants, 0).memory.expect("memory");
    if expected == 0 {
        assert_eq!(memory.installed_bytes, None, "no modules listed → no claim");
        return;
    }
    assert_eq!(memory.installed_bytes, Some(expected), "installed = Σ module sizes");

    let mut system_ram = 0u64;
    for entry in std::fs::read_dir("/sys/firmware/memmap").expect("memmap").flatten() {
        let read = |file: &str| std::fs::read_to_string(entry.path().join(file)).unwrap();
        if read("type").trim() == "System RAM" {
            let start = u64::from_str_radix(read("start").trim().trim_start_matches("0x"), 16).unwrap();
            let end = u64::from_str_radix(read("end").trim().trim_start_matches("0x"), 16).unwrap();
            system_ram += end - start + 1;
        }
    }
    assert!(memory.total_bytes <= system_ram, "usable {} > firmware RAM {system_ram}", memory.total_bytes);
    assert!(system_ram <= expected, "firmware RAM {system_ram} > installed {expected}");
}

/// The card is never named by a bare PCI id ("Device 7551" — what a
/// pci.ids older than the card says).
#[test]
fn gpu_has_a_real_name() {
    let mut wants = Wants::none();
    wants.gpu = true;
    let gpu = sampled(wants, 0).gpu.expect("gpu");
    if !gpu.available {
        return;
    }
    let name = gpu.device_name.as_str();
    let bare_id = name
        .strip_prefix("Device ")
        .is_some_and(|id| id.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!bare_id && !name.is_empty(), "GPU named by a bare id: {name:?}");
}

/// The NVIDIA card against nvidia-smi, the driver's own tool (it reads
/// the same NVML, so this pins that SysMon asks the right calls and
/// decodes them right, not a second physics). Skips where nvidia-smi
/// finds no card; fails where it finds one and SysMon doesn't.
#[test]
fn nvidia_matches_nvidia_smi() {
    let query = "nvidia-smi -i 0 --format=csv,noheader,nounits \
                 --query-gpu=name,memory.total,memory.reserved,memory.used,temperature.gpu,power.limit 2>/dev/null";
    let row = shell(query);
    let fields: Vec<&str> = row.trim().split(", ").collect();
    let mut wants = Wants::none();
    wants.nvidia = true;
    let ours = sampled(wants, 50).nvidia.expect("nvidia section");
    if fields.len() != 6 {
        assert!(!ours.available, "SysMon found an NVIDIA card nvidia-smi can't read: {}", ours.device_name);
        eprintln!("no NVIDIA card for nvidia-smi — skipping");
        return;
    }
    assert!(ours.available, "nvidia-smi reads {:?}, SysMon says unavailable", fields[0]);
    let mib = |field: &str| field.parse::<u64>().expect("MiB field");
    let (smi_total, smi_reserved, smi_used) = (mib(fields[1]), mib(fields[2]), mib(fields[3]));
    const MIB: u64 = 1 << 20;

    assert_eq!(ours.device_name, fields[0], "the card's name, verbatim");
    // Static: equal to the MiB nvidia-smi rounds to.
    assert_eq!(ours.vram_total_bytes / MIB, smi_total, "total VRAM");
    let reserved = ours.vram_reserved_bytes.expect("driver has nvmlMemory_v2");
    assert!((reserved / MIB).abs_diff(smi_reserved) <= 1, "reserve: ours {} MiB vs {smi_reserved}", reserved / MIB);
    // Allocations move between two reads; 2% of the card covers a
    // working desktop's churn. The driver's reserve is 3.6% of an
    // 8 GB card, so reading v1 (reserve folded into used) fails here.
    let tolerance = smi_total / 50;
    assert!(
        (ours.vram_used_bytes / MIB).abs_diff(smi_used) <= tolerance,
        "used: ours {} MiB vs nvidia-smi {smi_used} MiB (tolerance {tolerance})",
        ours.vram_used_bytes / MIB
    );
    // The die warms or cools a degree or two between reads under load.
    if let (Some(celsius), Ok(smi)) = (ours.temperature_core_celsius, fields[4].parse::<f32>()) {
        assert!((celsius - smi).abs() <= 3.0, "die temp: ours {celsius} vs {smi}");
    }
    // The enforced limit is a setting: equal to nvidia-smi's 0.01 W.
    if let (Some(cap), Ok(smi)) = (ours.power_cap_watts, fields[5].parse::<f32>()) {
        assert!((cap - smi).abs() < 0.02, "power limit: ours {cap} vs {smi}");
    }

    // Every compute process nvidia-smi lists is ours, memory within
    // the same churn (a process can grow between the two reads).
    let apps = shell("nvidia-smi -i 0 --query-compute-apps=pid,used_memory --format=csv,noheader,nounits 2>/dev/null");
    for line in apps.lines().filter(|l| !l.trim().is_empty()) {
        let (pid, used) = line.split_once(", ").expect("pid, used");
        let pid: i32 = pid.trim().parse().expect("pid");
        let ours_row = ours.processes.iter().find(|p| p.pid == pid);
        let Ok(used) = used.trim().parse::<u64>() else {
            continue; // [N/A]: another user's process
        };
        let row = ours_row.unwrap_or_else(|| panic!("nvidia-smi lists pid {pid}, SysMon doesn't"));
        assert!((row.vram_bytes / MIB).abs_diff(used) <= tolerance, "pid {pid}: ours {} MiB vs {used}", row.vram_bytes / MIB);
    }
}

/// Busy + iowait ≤ 100, and iowait matches the kernel's own split
/// over the same window, read independently.
#[test]
fn cpu_iowait_is_reported_apart_from_busy() {
    let read = || -> (u64, u64) {
        let stat = std::fs::read_to_string("/proc/stat").unwrap();
        let fields: Vec<u64> = stat.lines().next().unwrap().split_ascii_whitespace().skip(1)
            .map(|f| f.parse().unwrap()).collect();
        (fields.iter().take(8).sum(), fields[4])
    };
    let mut wants = Wants::none();
    wants.cpu = true;
    let mut sampler = Sampler::new();
    let _ = sampler.sample(wants);
    let (total_before, iowait_before) = read();
    std::thread::sleep(Duration::from_millis(800));
    let cpu = sampler.sample(wants).cpu.expect("cpu");
    let (total_after, iowait_after) = read();
    let independent = (iowait_after - iowait_before) as f32 / (total_after - total_before).max(1) as f32 * 100.0;
    assert!(cpu.overall_percent + cpu.iowait_percent <= 100.5);
    // Adjacent, not identical windows: a disk-bound desktop moves
    // iowait a few points between reads.
    assert!(
        (cpu.iowait_percent - independent).abs() < 10.0,
        "iowait ours {} vs /proc/stat {independent}",
        cpu.iowait_percent
    );
}

/// F15 / G8: the GPU clock is the mean across the sample window, not
/// one instantaneous read. The authority is an independent 100 Hz poll
/// of the same `gpu_metrics` field over the same windows, run here.
///
/// The statistic: over six 2 s windows (the GUI's default interval),
/// the root-mean-square of (ours − independent mean) ÷ σ. With 10 of
/// our reads against ~200 independent ones, a true window mean is off
/// by about σ·√(1/10 + 1/200) ≈ 0.32σ; a single read is off by about σ
/// (the signal's own spread). The bar is 0.6σ: ≈ 1.9× the honest
/// estimator's expected RMS, ≈ 0.6× the cheat's. Idle-gated windows
/// (σ = 0, nothing to estimate) are skipped.
#[test]
fn gpu_clock_is_the_window_mean_not_one_read() {
    let Some(card) = std::fs::read_dir("/sys/class/drm").ok().and_then(|entries| {
        entries.flatten().map(|e| e.path().join("device")).find(|d| {
            std::fs::read_to_string(d.join("vendor")).is_ok_and(|v| v.trim() == "0x1002")
                && d.join("gpu_metrics").exists()
        })
    }) else {
        eprintln!("no amdgpu gpu_metrics here — skipping");
        return;
    };
    let read_gfx = |card: &std::path::Path| -> Option<f64> {
        let blob = std::fs::read(card.join("gpu_metrics")).ok()?;
        let layout = blob.get(2..4)?;
        (layout[0] == 1 && (1..=3).contains(&layout[1]))
            .then(|| u16::from_le_bytes([blob[0x28], blob[0x29]]))
            .filter(|v| *v != u16::MAX)
            .map(f64::from)
    };
    if read_gfx(&card).is_none() {
        eprintln!("gpu_metrics layout not v1.1–v1.3 — skipping");
        return;
    }

    let mut wants = Wants::none();
    wants.gpu = true;
    let mut sampler = Sampler::new();
    let _prime = sampler.sample(wants);
    let mut z_squares = Vec::new();
    let mut report = Vec::new();
    for _ in 0..6 {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let poll = {
            let (stop, card) = (stop.clone(), card.clone());
            std::thread::spawn(move || {
                let mut reads = Vec::new();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    reads.extend(read_gfx(&card));
                    std::thread::sleep(Duration::from_millis(10));
                }
                reads
            })
        };
        std::thread::sleep(Duration::from_millis(2000));
        let ours = sampler.sample(wants).gpu.expect("gpu section");
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let reads = poll.join().unwrap();

        let source = ours.clock_source.clone().unwrap_or_default();
        assert!(source.starts_with("mean of "), "clock_source should be a window mean, got {source:?}");
        let n = reads.len() as f64;
        let mean = reads.iter().sum::<f64>() / n;
        let sigma = (reads.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
        let core = ours.core_clock_mhz.expect("core clock");
        report.push(format!("{core:.0} vs {mean:.0}±σ{sigma:.0}"));
        if sigma >= 5.0 {
            z_squares.push(((core - mean) / sigma).powi(2));
        }
    }
    if z_squares.len() < 3 {
        eprintln!("GPU clock-gated (σ≈0) in most windows, nothing to estimate — skipping: {report:?}");
        return;
    }
    let rms = (z_squares.iter().sum::<f64>() / z_squares.len() as f64).sqrt();
    assert!(rms <= 0.6, "RMS error {rms:.2}σ — that's a single read, not a window mean: {report:?}");
}

/// C1a: the busy clock is what the busy cores actually delivered over
/// the window, not one instant read per core (which caught idle cores
/// at their idle clock and ran 0.6–2.1 GHz low on this machine).
///
/// Authority: ACPI CPPC feedback counters (delivered/reference, the
/// APERF/MPERF ratio turbostat reads), sampled by this test at the
/// window's edges and busy-weighted from /proc/stat over the same
/// window. Tolerance 150 MHz: the two windows' edges differ by the
/// few ms it takes to read 32 cores' counters (~0.6 ms each).
#[test]
fn cpu_busy_clock_is_what_the_busy_cores_delivered() {
    let cppc = |core: usize| -> Option<(u64, u64)> {
        let text = std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{core}/acpi_cppc/feedback_ctrs")).ok()?;
        let mut fields = text.split_whitespace();
        let reference = fields.next()?.strip_prefix("ref:")?.parse().ok()?;
        let delivered = fields.next()?.strip_prefix("del:")?.parse().ok()?;
        Some((reference, delivered))
    };
    let Some(nominal_mhz) = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/acpi_cppc/nominal_freq")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
    else {
        eprintln!("no ACPI CPPC here — skipping");
        return;
    };
    let busy_ticks = || -> Vec<(u64, u64)> {
        std::fs::read_to_string("/proc/stat")
            .unwrap()
            .lines()
            .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
            .map(|l| {
                let v: Vec<u64> = l.split_whitespace().skip(1).map(|f| f.parse().unwrap()).collect();
                let total: u64 = v.iter().sum();
                (total, total - v[3] - v[4])
            })
            .collect()
    };

    // The load changes between consecutive windows (C1c): half the
    // cores, then one, then half, then one. A clock that answers with
    // the previous window's delivery is caught on every switch; under
    // constant load it would look right. Each window's truth is read
    // at the same edges the sampler uses (right after each sample).
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    let spin = |count: usize| {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let threads: Vec<_> = (0..count)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        std::hint::black_box(1u64.wrapping_mul(3));
                    }
                })
            })
            .collect();
        move || {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            for thread in threads {
                thread.join().unwrap();
            }
        }
    };

    let mut wants = Wants::none();
    wants.cpu = true;
    let mut sampler = Sampler::new();
    let mut results = Vec::new();
    let mut stop = spin(cores / 2);
    std::thread::sleep(Duration::from_millis(300));
    let _prime = sampler.sample(wants);
    let mut ticks_0 = busy_ticks();
    let mut cppc_0: Vec<_> = (0..cores).map(cppc).collect();
    for window in 0..4 {
        std::thread::sleep(Duration::from_millis(1500));
        let ours = sampler.sample(wants).cpu.expect("cpu");
        let ticks_1 = busy_ticks();
        let cppc_1: Vec<_> = (0..cores).map(cppc).collect();
        // Switch the load for the next window.
        stop();
        stop = spin(if window % 2 == 0 { 1 } else { cores / 2 });

        let (mut weight, mut sum) = (0.0, 0.0);
        for core in 0..cores {
            let (Some(a), Some(b)) = (cppc_0[core], cppc_1[core]) else { continue };
            let (t0, b0) = ticks_0[core];
            let (t1, b1) = ticks_1[core];
            if b.0 <= a.0 || t1 <= t0 {
                continue;
            }
            let busy = (b1 - b0) as f64 / (t1 - t0) as f64;
            let delivered = nominal_mhz * (b.1 - a.1) as f64 / (b.0 - a.0) as f64;
            weight += busy;
            sum += busy * delivered;
        }
        let truth = sum / weight;
        let busy_clock = ours.frequency_busy_mhz.expect("frequency_busy_mhz");
        results.push((window, busy_clock, truth, ours.frequency_busy_source.clone()));
        (ticks_0, cppc_0) = (busy_ticks(), (0..cores).map(cppc).collect());
    }
    stop();
    for (window, ours, truth, source) in &results {
        assert!(
            (ours - truth).abs() <= 150.0,
            "window {window}: busy clock {ours:.0} MHz ({source:?}) vs CPPC delivered {truth:.0} MHz over the same window: {results:?}"
        );
    }
}

/// The thread view (3.8): every thread `ps -L` lists, and a spinner
/// pinned to one CPU lands there with about a CPU's worth of time.
#[test]
fn threads_match_ps_and_a_pinned_thread_lands_on_its_cpu() {
    use sysmon_core::collect::threads::{ThreadSampler, cpu_count};
    let target = (cpu_count() - 1) as u32;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let spinner_stop = stop.clone();
    let (tid_tx, tid_rx) = std::sync::mpsc::channel();
    let spinner = std::thread::spawn(move || {
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_SET(target as usize, &mut set);
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
        }
        tid_tx.send(unsafe { libc::gettid() }).unwrap();
        while !spinner_stop.load(std::sync::atomic::Ordering::Relaxed) {
            std::hint::black_box(1u64.wrapping_mul(3));
        }
    });
    let spinner_tid = tid_rx.recv().unwrap();
    let pid = std::process::id() as i32;
    let mut sampler = ThreadSampler::new();
    sampler.sample(pid).expect("own threads");
    std::thread::sleep(Duration::from_millis(600));
    let threads = sampler.sample(pid).expect("own threads");
    let ps = shell(&format!("ps -L -o tid= -p {pid}"));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    spinner.join().unwrap();

    let mut ours: Vec<i32> = threads.threads.iter().map(|t| t.tid).collect();
    let mut theirs: Vec<i32> = ps.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    ours.sort();
    theirs.sort();
    // The test harness may start or end a thread between the two
    // reads; the spinner and the main thread must be in both.
    for tid in [pid, spinner_tid] {
        assert!(ours.contains(&tid) && theirs.contains(&tid), "tid {tid}: ours {ours:?} ps {theirs:?}");
    }
    let spinner = threads.threads.iter().find(|t| t.tid == spinner_tid).expect("spinner listed");
    assert_eq!(spinner.cpu, Some(target), "pinned to CPU {target}");
    assert!(spinner.cpu_percent >= 60.0, "a spinning thread is about one CPU: {}", spinner.cpu_percent);
    assert!(threads.per_cpu_percent[target as usize] >= 60.0, "its CPU carries the time: {:?}", threads.per_cpu_percent);
}
