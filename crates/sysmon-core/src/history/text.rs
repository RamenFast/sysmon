// SPDX-License-Identifier: GPL-3.0-or-later
//! A summarized range as plain text: what a pop-out's copy button
//! copies under the section's specs, and what `sysmon history` prints.
//!
//! ```text
//! Usage · last 1 h · 2026-10-10 20:02–21:02 PDT
//! 1,800 samples, one every 2 s, covering all of it
//!
//!                       min      avg      p95      max
//! GPU busy               0%      41%      99%     100%
//! VRAM used         24.6 GB  29.8 GB  30.9 GB  31.0 GB
//!
//! Timeline, one block per 75 s, scaled to the ceiling (▁ low, █ full)
//! GPU busy   ▁▁▁▂▆███████████████▇██████████▇▆▁▁▁▁▁▁▁▁▁▁▁▁▁
//!
//! On the GPU        peak VRAM   avg VRAM   busy avg / max   present
//! strata              24.6 GB    24.6 GB         0% / 3%       100%
//! ```
//!
//! Like the spec sheet it names programs, devices and sensors, never
//! the hostname, addresses or mount paths. A block with no samples is
//! a space: SysMon was not recording then, and the text never fills a
//! gap with a zero.

use crate::units::{
    TemperatureScale, Units, format_frequency_mhz, format_rate, format_size, format_temperature_in,
};

use super::summary::{GpuProcessSummary, MetricSummary, Summary, Unit};

/// How the text formats numbers (the GUI passes the user's choices).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStyle {
    pub units: Units,
    pub temperature: TemperatureScale,
}

const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const MAX_PROGRAMS: usize = 8;
const MAX_SENSOR_TIMELINES: usize = 6;

/// "5 min", "1 h", "24 h", "7 d".
pub fn range_label(seconds: f64) -> String {
    let seconds = seconds.round();
    if seconds >= 2.0 * 86_400.0 && (seconds % 86_400.0) == 0.0 {
        format!("{:.0} d", seconds / 86_400.0)
    } else if seconds >= 3_600.0 && (seconds % 3_600.0) == 0.0 {
        format!("{:.0} h", seconds / 3_600.0)
    } else if seconds >= 3_600.0 {
        format!("{:.1} h", seconds / 3_600.0)
    } else if seconds >= 120.0 || (seconds >= 60.0 && seconds % 60.0 == 0.0) {
        format!("{:.0} min", seconds / 60.0)
    } else {
        format!("{seconds:.0} s")
    }
}

/// Local (date, "HH:MM", zone) for a unix time.
fn local(ts: f64) -> (String, String, String) {
    let seconds = ts as libc::time_t;
    // SAFETY: localtime_r writes only into the struct we own; tm_zone
    // points at static tz data that outlives this call.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null() {
        return (String::new(), format!("{ts:.0}"), "unix".to_string());
    }
    let zone = if tm.tm_zone.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(tm.tm_zone) }.to_string_lossy().to_string()
    };
    (
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday),
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min),
        zone,
    )
}

/// "2026-10-10 20:02–21:02 PDT", or both dates when the range crosses
/// midnight.
pub fn local_range(from_ts: f64, to_ts: f64) -> String {
    let (from_date, from_time, _) = local(from_ts);
    let (to_date, to_time, zone) = local(to_ts);
    let zone = if zone.is_empty() { String::new() } else { format!(" {zone}") };
    if from_date == to_date {
        format!("{from_date} {from_time}–{to_time}{zone}")
    } else {
        format!("{from_date} {from_time} – {to_date} {to_time}{zone}")
    }
}

fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// One value in its unit.
pub fn format_value(value: f64, unit: Unit, style: TextStyle) -> String {
    match unit {
        Unit::Percent if value.abs() < 9.95 => format!("{value:.1}%"),
        Unit::Percent => format!("{value:.0}%"),
        Unit::Bytes => format_size(value.max(0.0).round() as u64, style.units),
        Unit::BytesPerSecond => format_rate(value, style.units),
        Unit::Celsius => format_temperature_in(value as f32, style.temperature),
        Unit::Watts => format!("{value:.0} W"),
        Unit::Mhz => format_frequency_mhz(value),
        Unit::Rpm => format!("{value:.0} rpm"),
        Unit::Load => format!("{value:.2}"),
    }
}

/// The timeline as block glyphs. A reading with a full scale (a
/// percent, RAM, VRAM) is drawn from 0 to that scale, so "half the
/// card" looks like half. Any other reading (a temperature, a clock, a
/// rate) is drawn from the range's min to its max, so its movement
/// shows: scaled to its own max, 68–72 °C would be a flat bar. A
/// reading that never moved is a row of `▄`. A block with no samples
/// is a space.
pub fn sparkline(metric: &MetricSummary) -> String {
    let (bottom, top) = match metric.ceiling {
        Some(ceiling) => (0.0, ceiling.max(metric.max)),
        None => (metric.min, metric.max),
    };
    let span = top - bottom;
    let flat = span <= f64::EPSILON * top.abs().max(1.0);
    metric
        .timeline
        .iter()
        .map(|block| match block {
            None => ' ',
            Some(_) if flat && metric.ceiling.is_none() && metric.max != 0.0 => BLOCKS[3],
            Some(_) if flat => BLOCKS[0],
            Some(value) => {
                let level = ((value - bottom) / span * BLOCKS.len() as f64).ceil() as usize;
                BLOCKS[level.clamp(1, BLOCKS.len()) - 1]
            }
        })
        .collect()
}

/// The metrics drawn as timelines: each section's headline readings
/// that did something (a machine with no swap gets no swap timeline).
fn timeline_metrics(summary: &Summary) -> Vec<&MetricSummary> {
    let headline = [
        "cpu.busy_percent",
        "cpu.busy_clock_mhz",
        "cpu.temperature_celsius",
        "memory.used_bytes",
        "memory.swap_used_bytes",
        "gpu.busy_percent",
        "gpu.vram_used_bytes",
        "gpu.power_watts",
        "gpu.junction_celsius",
        "network.download_bps",
        "network.upload_bps",
        "disks.read_bps",
        "disks.write_bps",
    ];
    let mut chosen: Vec<&MetricSummary> =
        headline.iter().filter_map(|key| summary.metric(key)).filter(|m| m.max != 0.0 || m.min != 0.0).collect();
    // Sensors: the hottest few, then any fan that turned.
    let mut temps: Vec<&MetricSummary> =
        summary.metrics.iter().filter(|m| m.key.starts_with("sensors.") && m.unit == Unit::Celsius).collect();
    temps.sort_by(|a, b| b.max.total_cmp(&a.max));
    chosen.extend(temps.into_iter().take(MAX_SENSOR_TIMELINES));
    chosen.extend(
        summary.metrics.iter().filter(|m| m.key.starts_with("sensors.") && m.unit == Unit::Rpm && m.max > 0.0).take(4),
    );
    chosen
}

fn pad_right(text: &str, width: usize) -> String {
    let count = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(count)))
}

fn pad_left(text: &str, width: usize) -> String {
    let count = text.chars().count();
    format!("{}{text}", " ".repeat(width.saturating_sub(count)))
}

/// A right-aligned table under a left label column.
fn table(header: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let columns = header.len();
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate().take(columns) {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let line = |cells: Vec<String>| -> String {
        let mut out = pad_right(&cells[0], widths[0]);
        for (index, cell) in cells.iter().enumerate().skip(1) {
            out.push_str("  ");
            out.push_str(&pad_left(cell, widths[index]));
        }
        out.trim_end().to_string()
    };
    let mut lines = vec![line(header.iter().map(|h| h.to_string()).collect())];
    lines.extend(rows.iter().map(|row| line(row.clone())));
    lines
}

fn program_rows(programs: &[GpuProcessSummary], style: TextStyle) -> Vec<Vec<String>> {
    programs
        .iter()
        .filter(|p| p.vram_max_bytes >= 1_000_000.0 || p.busy_max_percent >= 1.0)
        .take(MAX_PROGRAMS)
        .map(|p| {
            vec![
                p.name.clone(),
                format_value(p.vram_max_bytes, Unit::Bytes, style),
                format_value(p.vram_avg_bytes, Unit::Bytes, style),
                format!(
                    "{} / {}",
                    format_value(p.busy_avg_percent, Unit::Percent, style),
                    format_value(p.busy_max_percent, Unit::Percent, style)
                ),
                format!("{:.0}%", p.present_percent),
            ]
        })
        .collect()
}

/// The usage block for one summarized range.
pub fn summary_text(summary: &Summary, style: TextStyle) -> String {
    let span = summary.to_ts - summary.from_ts;
    let mut lines = vec![format!(
        "Usage · last {} · {}",
        range_label(span),
        local_range(summary.from_ts, summary.to_ts)
    )];
    if summary.samples == 0 || summary.metrics.is_empty() {
        lines.push(
            "No history recorded in this range. SysMon records while its window or \
             `sysmon serve --record` runs (Display options → History)."
                .to_string(),
        );
        return lines.join("\n") + "\n";
    }
    let cadence = summary.cadence_seconds.map(|c| format!(", one every {}", range_label(c))).unwrap_or_default();
    let coverage = summary.covered_seconds / span.max(1e-9);
    let covering = if coverage >= 0.98 {
        "covering all of it".to_string()
    } else {
        format!("covering {} of it", range_label(summary.covered_seconds))
    };
    lines.push(format!("{} samples{cadence}, {covering}", thousands(summary.samples)));
    if summary.skipped_lines > 0 {
        lines.push(format!("({} unreadable lines skipped)", summary.skipped_lines));
    }

    lines.push(String::new());
    let rows: Vec<Vec<String>> = summary
        .metrics
        .iter()
        .filter(|m| m.samples > 0)
        .map(|m| {
            let mut row = vec![m.label.clone()];
            for value in [m.min, m.avg, m.p95, m.max] {
                row.push(format_value(value, m.unit, style));
            }
            row
        })
        .collect();
    lines.extend(table(&["", "min", "avg", "p95", "max"], &rows));

    let moved: Vec<String> = summary
        .metrics
        .iter()
        .filter_map(|m| m.total_bytes.filter(|t| *t >= 1.0).map(|t| format!("{} {}", m.label, format_size(t as u64, style.units))))
        .collect();
    if !moved.is_empty() {
        lines.push(format!("Moved: {}", moved.join(" · ")));
    }

    let timelines = timeline_metrics(summary);
    if !timelines.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Timeline, one block per {} (% and memory: ▁ empty … █ full; the rest: ▁ its min … █ its max; blank = not recorded)",
            range_label(summary.timeline_block_seconds)
        ));
        let width = timelines.iter().map(|m| m.label.chars().count()).max().unwrap_or(0);
        for metric in timelines {
            lines.push(format!("{}  {}", pad_right(&metric.label, width), sparkline(metric)).trim_end().to_string());
        }
    }

    let programs = program_rows(&summary.gpu_processes, style);
    if !programs.is_empty() {
        lines.push(String::new());
        lines.extend(table(&["On the GPU", "peak VRAM", "avg VRAM", "busy avg / max", "present"], &programs));
        lines.push("(a buffer two programs share, such as a window the compositor draws, counts in both)".to_string());
    }
    lines.join("\n") + "\n"
}

/// A pop-out's copy: a title, the section's specs (when it has some),
/// then the usage block.
pub fn section_report(title: &str, specs_text: Option<&str>, summary: &Summary, style: TextStyle) -> String {
    let mut text = format!("SysMon {} · {title}\n\n", crate::VERSION);
    if let Some(specs) = specs_text.map(str::trim_end).filter(|s| !s.is_empty()) {
        text.push_str(specs);
        text.push_str("\n\n");
    }
    text.push_str(&summary_text(summary, style));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::summary::summarize;
    use crate::history::{GpuProcessRecord, GpuRecord, HistoryRecord};

    fn gpu_hour() -> Summary {
        let records: Vec<HistoryRecord> = (0..1800)
            .map(|i| {
                let game_on = (600..1200).contains(&i);
                HistoryRecord {
                    ts: 1_791_680_000.0 + f64::from(i) * 2.0,
                    interval_seconds: 2.0,
                    gpu: Some(GpuRecord {
                        busy_percent: if game_on { 97.0 } else { 2.0 },
                        vram_used_bytes: if game_on { 30_900_000_000 } else { 24_600_000_000 },
                        vram_total_bytes: 34_208_743_424,
                        power_watts: Some(if game_on { 280.0 } else { 30.0 }),
                        processes: [
                            Some(GpuProcessRecord { pid: 1, name: "strata".into(), vram_bytes: 24_500_000_000, ..Default::default() }),
                            game_on.then(|| GpuProcessRecord {
                                pid: 2,
                                name: "ArcRaiders".into(),
                                busy_percent: 95.0,
                                vram_bytes: 6_200_000_000,
                                ..Default::default()
                            }),
                        ]
                        .into_iter()
                        .flatten()
                        .collect(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }
            })
            .collect();
        summarize("gpu", 1_791_680_000.0, 1_791_683_600.0, 48, move |sink| {
            records.into_iter().for_each(sink);
            0
        })
    }

    #[test]
    fn the_usage_block_is_organized_and_honest() {
        let text = summary_text(&gpu_hour(), TextStyle::default());
        assert!(text.starts_with("Usage · last 1 h · "), "{text}");
        assert!(text.contains("1,800 samples, one every 2 s, covering all of it"), "{text}");
        let busy = text.lines().find(|l| l.starts_with("GPU busy ")).expect("a busy row");
        // min 2.0%, avg 33.7%, p95 97%, max 97%.
        assert!(busy.contains("2.0%") && busy.contains("34%") && busy.ends_with("97%"), "{busy}");
        let vram = text.lines().find(|l| l.starts_with("VRAM used ")).expect("a VRAM row");
        assert!(vram.contains("24.6 GB") && vram.ends_with("30.9 GB"), "{vram}");
        let game = text.lines().find(|l| l.starts_with("ArcRaiders")).expect("the game row");
        assert!(game.contains("6.2 GB") && game.contains("95% / 95%") && game.ends_with("33%"), "{game}");
        let strata = text.lines().position(|l| l.starts_with("strata")).unwrap();
        assert!(strata < text.lines().position(|l| l.starts_with("ArcRaiders")).unwrap(), "peak VRAM first");
        let timeline = text.lines().find(|l| l.starts_with("GPU busy  ") && l.contains('█')).expect("a timeline");
        assert_eq!(timeline.chars().filter(|c| BLOCKS.contains(c)).count(), 48);
    }

    #[test]
    fn an_unrecorded_range_says_so_instead_of_printing_zeroes() {
        let empty = summarize("gpu", 0.0, 3_600.0, 48, |_| 0);
        let text = summary_text(&empty, TextStyle::default());
        assert!(text.contains("No history recorded in this range"), "{text}");
        assert!(!text.contains("0%"), "{text}");
    }

    #[test]
    fn a_reading_without_a_scale_shows_its_movement() {
        let metric = MetricSummary {
            key: "cpu.temperature_celsius".into(),
            label: "Temperature".into(),
            unit: Unit::Celsius,
            samples: 4,
            min: 68.0,
            avg: 70.0,
            p95: 72.0,
            max: 72.0,
            latest: 72.0,
            total_bytes: None,
            ceiling: None,
            timeline: vec![Some(68.0), Some(70.0), None, Some(72.0)],
        };
        assert_eq!(sparkline(&metric), "▁▄ █", "min to max, not a flat bar against 72");
        let steady = MetricSummary { min: 70.0, max: 70.0, timeline: vec![Some(70.0); 3], ..metric.clone() };
        assert_eq!(sparkline(&steady), "▄▄▄", "steady: mid-height, it never moved");
        let nothing = MetricSummary { min: 0.0, max: 0.0, timeline: vec![Some(0.0); 3], ..metric };
        assert_eq!(sparkline(&nothing), "▁▁▁", "always zero reads as empty, not half full");
    }

    #[test]
    fn a_gap_is_blank_in_the_timeline() {
        let mut summary = gpu_hour();
        let busy = summary.metrics.iter_mut().find(|m| m.key == "gpu.busy_percent").unwrap();
        busy.timeline[3] = None;
        let line = sparkline(busy);
        assert_eq!(line.chars().nth(3), Some(' '));
        assert_eq!(line.chars().count(), 48);
    }

    #[test]
    fn range_labels_read_like_a_person_wrote_them() {
        assert_eq!(range_label(300.0), "5 min");
        assert_eq!(range_label(3_600.0), "1 h");
        assert_eq!(range_label(86_400.0), "24 h");
        assert_eq!(range_label(604_800.0), "7 d");
        assert_eq!(range_label(75.0), "75 s", "a timeline block is not rounded to a minute");
        assert_eq!(range_label(2.0), "2 s");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
