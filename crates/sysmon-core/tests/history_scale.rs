// SPDX-License-Identifier: GPL-3.0-or-later
//! Scale receipt for the history store: a week at the 2 s default,
//! written through the real Recorder (sealing every hour), then read
//! back and summarized through the real reader. The week repeats one
//! real record, so it compresses better than a real week (49 B a
//! sample here vs 162 B from a real recorded hour): this measures
//! speed and the fit under the cap, not the true size on disk. Ignored by default
//! (it writes ~1.2 GB of raw lines through the page cache and takes a
//! minute); run it when the record shape or the store changes:
//!
//!   cargo test --release -p sysmon-core --test history_scale -- --ignored --nocapture

use std::time::Instant;

use sysmon_core::history::{self, GpuProcessRecord, HistoryRecord, Limits, Recorder, Recording};

#[test]
#[ignore]
fn a_week_at_two_seconds_fits_and_summarizes_quickly() {
    let dir = std::env::temp_dir().join(format!("sysmon-history-scale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // A real record from this machine as the template (24 temps, 8 fans,
    // 5 disks, 10 GPU programs), so the size is this machine's size.
    let template: HistoryRecord = {
        let mut sampler = sysmon_core::collect::Sampler::new();
        let wants = sysmon_core::snapshot::Wants::all();
        let _ = sampler.sample(wants);
        std::thread::sleep(std::time::Duration::from_millis(500));
        HistoryRecord::from_snapshot(&sampler.sample(wants))
    };
    let raw_line = serde_json::to_string(&template).unwrap().len();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    let start = now - 7.0 * 86_400.0;
    let samples = 7 * 86_400 / 2;
    let written = Instant::now();
    let mut recorder = Recorder::open(&dir, Limits::new(8.0 * 86_400.0, 4_000_000_000), "scale-test").unwrap();
    for i in 0..samples {
        let mut record = template.clone();
        // Millisecond timestamps, as the real writer rounds them.
        record.ts = ((start + f64::from(i) * 2.0) * 1000.0).round() / 1000.0;
        let phase = f64::from(i % 1_800) / 1_800.0;
        if let Some(gpu) = &mut record.gpu {
            gpu.busy_percent = (phase * 100.0) as f32;
            gpu.vram_used_bytes = 24_000_000_000 + (phase * 6e9) as u64;
            if i % 3 == 0 {
                gpu.processes.push(GpuProcessRecord { pid: 99, name: "game".into(), busy_percent: 90.0, vram_bytes: 6_000_000_000, gtt_bytes: 0 });
            }
        }
        recorder.record(&record).unwrap();
    }
    let write_seconds = written.elapsed().as_secs_f64();
    drop(recorder);
    let info = Recording::info(&dir);

    // The range opens 1 s before the first record: serde_json's default
    // float parse can land 1 ULP below a value, so a record exactly on
    // the edge may fall out (it did, once in two runs). Real ranges never
    // line up with a record to the ULP.
    let from = start - 1.0;
    let read = Instant::now();
    let summary = history::summarize("gpu", from, now, 48, |sink| Recording::for_each(&dir, from, now, sink).skipped_lines);
    let week_seconds = read.elapsed().as_secs_f64();
    let read = Instant::now();
    let hour = history::summarize("gpu", now - 3_600.0, now, 48, |sink| {
        Recording::for_each(&dir, now - 3_600.0, now, sink).skipped_lines
    });
    let hour_seconds = read.elapsed().as_secs_f64();
    let mut on_disk = Vec::new();
    Recording::for_each(&dir, 0.0, f64::MAX, |r| on_disk.push(r.ts));
    let _ = std::fs::remove_dir_all(&dir);
    println!(
        "on disk: {} records, first {:?} (start {start}), last {:?}; summary first {:?} last {:?} skipped {}",
        on_disk.len(),
        on_disk.first(),
        on_disk.last(),
        summary.first_ts,
        summary.last_ts,
        summary.skipped_lines
    );

    println!(
        "week at 2 s: {samples} records, {raw_line} B a raw line, {} files, {} bytes on disk ({:.0} B a record sealed); \
         write {write_seconds:.1} s; summarize week {week_seconds:.2} s, last hour {hour_seconds:.3} s",
        info.files,
        info.bytes,
        info.bytes as f64 / f64::from(samples)
    );
    assert_eq!(summary.samples, samples as usize);
    assert!(hour.samples >= 1_799, "{}", hour.samples);
    assert!(info.bytes < 256_000_000, "a week must fit the default cap: {} bytes", info.bytes);
    assert!(week_seconds < 10.0, "a week must summarize in seconds, took {week_seconds:.1} s");
    assert!(hour_seconds < 0.5, "an hour must summarize at once, took {hour_seconds:.3} s");
    let game = summary.gpu_processes.iter().find(|p| p.name == "game").expect("the game");
    assert!((game.present_percent - 100.0 / 3.0).abs() < 0.1, "{}", game.present_percent);
}
