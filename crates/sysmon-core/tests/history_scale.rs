// SPDX-License-Identifier: GPL-3.0-or-later
//! Scale receipts for the history store (ignored by default; run them
//! when the record shape or the store changes):
//!
//!   cargo test --release -p sysmon-core --test history_scale -- --ignored --nocapture
//!
//! `a_week_at_two_seconds_fits_and_summarizes_quickly`: a week at the
//! 2 s default through the real Recorder (sealing every hour, building
//! rollups), read back and summarized through the real reader.
//!
//! `long_ranges_stay_fast` (3.5): a simulated 30-day and 1-year store,
//! hot week plus packed archive, timed for `history gpu --since 30d`
//! (the summary the CLI and the copy buttons run) with rollups and
//! without. The records are synthetic (one real template with varying
//! readings), so the sizes say nothing about real compression; that is
//! measured on a copy of the real history (docs/dev/RECEIPTS.md). To
//! keep the year's setup short its archive is packed at zstd level 3
//! (decode speed barely depends on the level), at a 10 s cadence
//! (3.15 M records; the read path's cost is per record either way).

use std::time::Instant;

use sysmon_core::history::{
    self, GpuProcessRecord, HistoryRecord, Limits, PackScope, Recorder, Recording, summarize_dir,
};

fn template() -> HistoryRecord {
    // A real record from this machine as the template (24 temps, 8 fans,
    // 5 disks, 10 GPU programs), so the line is this machine's line.
    let mut sampler = sysmon_core::collect::Sampler::new();
    let wants = sysmon_core::snapshot::Wants::all();
    let _ = sampler.sample(wants);
    std::thread::sleep(std::time::Duration::from_millis(500));
    HistoryRecord::from_snapshot(&sampler.sample(wants))
}

fn vary(template: &HistoryRecord, ts: f64, i: u64) -> HistoryRecord {
    let mut record = template.clone();
    record.ts = (ts * 1000.0).round() / 1000.0;
    let phase = (i % 1_800) as f64 / 1_800.0;
    if let Some(gpu) = &mut record.gpu {
        gpu.busy_percent = (phase * 100.0) as f32;
        gpu.vram_used_bytes = 24_000_000_000 + (phase * 6e9) as u64;
        if i.is_multiple_of(3) {
            gpu.processes.push(GpuProcessRecord { pid: 99, name: "game".into(), busy_percent: 90.0, vram_bytes: 6_000_000_000, gtt_bytes: 0 });
        }
    }
    if let Some(cpu) = &mut record.cpu {
        cpu.busy_percent = ((i * 7919) % 1000) as f32 / 10.0;
    }
    record
}

/// Rollups change nothing a person can see: counts, min, max, p95,
/// latest and ceilings exactly; sums (avg, bytes moved, timeline means)
/// to 1e-9 relative, since adding hour subtotals rounds differently
/// from adding 1.3 M values one by one (measured: the 14th digit).
fn assert_same(a: &history::Summary, b: &history::Summary) {
    let close = |x: f64, y: f64| (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0);
    assert_eq!(a.samples, b.samples);
    assert_eq!(a.metrics.len(), b.metrics.len());
    for (m, p) in a.metrics.iter().zip(&b.metrics) {
        assert_eq!((&m.key, m.samples, m.min, m.max, m.p95, m.latest, m.ceiling), (&p.key, p.samples, p.min, p.max, p.p95, p.latest, p.ceiling));
        assert!(close(m.avg, p.avg), "{} avg {} vs {}", m.key, m.avg, p.avg);
        if let (Some(x), Some(y)) = (m.total_bytes, p.total_bytes) {
            assert!(close(x, y), "{} total", m.key);
        }
        for (x, y) in m.timeline.iter().zip(&p.timeline) {
            match (x, y) {
                (Some(x), Some(y)) => assert!(close(*x, *y), "{} timeline", m.key),
                _ => assert_eq!(x, y),
            }
        }
    }
    assert_eq!(a.gpu_processes.len(), b.gpu_processes.len());
    for (m, p) in a.gpu_processes.iter().zip(&b.gpu_processes) {
        assert_eq!((&m.name, m.samples, m.vram_max_bytes, m.vram_p95_bytes), (&p.name, p.samples, p.vram_max_bytes, p.vram_p95_bytes));
        assert!(close(m.vram_avg_bytes, p.vram_avg_bytes));
    }
}

#[test]
#[ignore]
fn a_week_at_two_seconds_fits_and_summarizes_quickly() {
    let dir = std::env::temp_dir().join(format!("sysmon-history-scale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let template = template();
    let raw_line = serde_json::to_string(&template).unwrap().len();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    let start = now - 7.0 * 86_400.0;
    let samples: u32 = 7 * 86_400 / 2;
    let written = Instant::now();
    let mut recorder = Recorder::open(&dir, Limits::new(8.0 * 86_400.0, None, None), "scale-test").unwrap();
    for i in 0..samples {
        recorder.record(&vary(&template, start + f64::from(i) * 2.0, u64::from(i))).unwrap();
    }
    recorder.maintain_blocking(PackScope::Due);
    let write_seconds = written.elapsed().as_secs_f64();
    drop(recorder);
    let info = Recording::info(&dir);

    // The range opens 1 s before the first record (real ranges never
    // line up with a record to the millisecond).
    let from = start - 1.0;
    let read = Instant::now();
    let summary = summarize_dir(&dir, "gpu", from, now, 48, true);
    let week_seconds = read.elapsed().as_secs_f64();
    let read = Instant::now();
    let plain = summarize_dir(&dir, "gpu", from, now, 48, false);
    let week_plain_seconds = read.elapsed().as_secs_f64();
    let read = Instant::now();
    let hour = summarize_dir(&dir, "gpu", now - 3_600.0, now, 48, true);
    let hour_seconds = read.elapsed().as_secs_f64();
    let _ = std::fs::remove_dir_all(&dir);
    println!(
        "week at 2 s: {samples} records, {raw_line} B a raw line, {} files, {} bytes on disk; write {write_seconds:.1} s; \
         summarize week {week_seconds:.2} s with rollups ({} hours merged), {week_plain_seconds:.2} s record by record; last hour {hour_seconds:.3} s",
        info.files, info.bytes, summary.rollup_hours,
    );
    assert_eq!(summary.samples, samples as usize);
    assert_same(&summary, &plain);
    assert!(hour.samples >= 1_799, "{}", hour.samples);
    assert!(week_seconds < 2.0, "a week must summarize at once, took {week_seconds:.1} s");
    assert!(hour_seconds < 0.5, "an hour must summarize at once, took {hour_seconds:.3} s");
    let game = summary.gpu_processes.iter().find(|p| p.name == "game").expect("the game");
    assert!((game.present_percent - 100.0 / 3.0).abs() < 0.1, "{}", game.present_percent);
}

/// Write `days` of history ending now at `cadence`, the last week hot
/// and the rest packed, straight into hourly gzip files and then
/// through the real packer.
fn build_store(dir: &std::path::Path, days: u32, cadence: f64, template: &HistoryRecord) -> (u64, f64) {
    use std::io::Write;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    let start = now - f64::from(days) * 86_400.0;
    std::fs::create_dir_all(dir).unwrap();
    let mut i = 0u64;
    let mut ts = start;
    let mut hour_start = (start / 3_600.0).floor() * 3_600.0;
    let mut buffer = Vec::new();
    let flush = |hour_start: f64, buffer: &mut Vec<u8>| {
        if buffer.is_empty() {
            return;
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(buffer).unwrap();
        let stamp = history::store::hour_stamp(hour_start);
        std::fs::write(dir.join(format!("{stamp}.jsonl.gz")), encoder.finish().unwrap()).unwrap();
        buffer.clear();
    };
    while ts < now - 60.0 {
        let this_hour = (ts / 3_600.0).floor() * 3_600.0;
        if this_hour != hour_start {
            flush(hour_start, &mut buffer);
            hour_start = this_hour;
        }
        let mut record = vary(template, ts, i);
        record.interval_seconds = cadence;
        serde_json::to_writer(&mut buffer, &record).unwrap();
        buffer.push(b'\n');
        i += 1;
        ts += cadence;
    }
    flush(hour_start, &mut buffer);
    (i, now)
}

#[test]
#[ignore]
fn long_ranges_stay_fast() {
    let template = template();
    for (label, days, cadence) in [("30-day store, 2 s", 30u32, 2.0f64), ("1-year store, 10 s", 365, 10.0)] {
        let dir = std::env::temp_dir().join(format!("sysmon-history-long-{days}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let built = Instant::now();
        let (records, now) = build_store(&dir, days, cadence, &template);
        let mut recorder = Recorder::open(&dir, Limits::new(7.0 * 86_400.0, None, None), "scale-test").unwrap();
        if days > 60 {
            recorder.set_archive_level(3);
        }
        let report = recorder.maintain_blocking(PackScope::Due);
        drop(recorder);
        let setup = built.elapsed().as_secs_f64();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        let info = Recording::info(&dir);
        let from = now - 30.0 * 86_400.0;
        let mut timings = Vec::new();
        for use_rollups in [true, false] {
            let read = Instant::now();
            let summary = summarize_dir(&dir, "gpu", from, now, 48, use_rollups);
            timings.push((read.elapsed().as_secs_f64(), summary));
        }
        // The slider at its last stop: everything on disk, 48 blocks.
        let oldest = info.oldest_ts.unwrap_or(now) - 1.0;
        let read = Instant::now();
        let whole = summarize_dir(&dir, "gpu", oldest, now, 48, true);
        let whole_seconds = read.elapsed().as_secs_f64();
        let read = Instant::now();
        let whole_plain = summarize_dir(&dir, "gpu", oldest, now, 48, false);
        let whole_plain_seconds = read.elapsed().as_secs_f64();
        println!(
            "{label}: {records} records, {} archive days ({:.1} MB) + {} hourly files ({:.1} MB), setup {setup:.0} s. \
             history gpu --since 30d: {:.3} s with rollups ({} of {} hours merged), {:.3} s record by record. \
             Whole store ({}, {} samples): {whole_seconds:.3} s with rollups ({} hours merged), {whole_plain_seconds:.2} s record by record",
            info.archive_days,
            info.archive_bytes as f64 / 1e6,
            info.hot_files,
            info.hot_bytes as f64 / 1e6,
            timings[0].0,
            timings[0].1.rollup_hours,
            30 * 24,
            timings[1].0,
            history::text::range_label(((now - oldest) / 86_400.0).round() * 86_400.0),
            whole.samples,
            whole.rollup_hours,
        );
        assert_same(&timings[0].1, &timings[1].1);
        assert_same(&whole, &whole_plain);
        assert_eq!(whole.samples as u64, records);
        assert!(timings[0].0 < 2.0, "30 days must summarize in about a second, took {:.2} s", timings[0].0);
        // SYSMON_SCALE_KEEP=1 leaves the store for timing the real CLI
        // (`XDG_STATE_HOME=<x> sysmon history gpu --since 30d`, with
        // <x>/sysmon/history linked to the printed dir).
        if std::env::var_os("SYSMON_SCALE_KEEP").is_some() {
            println!("kept: {}", dir.display());
            continue;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
