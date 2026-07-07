// SPDX-License-Identifier: GPL-3.0-or-later
//! Human-readable formatting, shared by the GUI and the CLI's friendly
//! output. Two unit systems, the user's choice (v1 semantics):
//!
//!   * decimal (default) — GB, MB/s: what drive stickers and ISPs quote
//!   * binary            — GiB, MiB/s: what htop and GNOME SM show

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Units {
    #[default]
    Decimal,
    Binary,
}

const DECIMAL_SIZE: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
const BINARY_SIZE: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
const DECIMAL_RATE: [&str; 5] = ["B/s", "kB/s", "MB/s", "GB/s", "TB/s"];
const BINARY_RATE: [&str; 5] = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"];

fn format_scaled(raw: f64, names: &[&str], step: f64) -> String {
    let mut value = raw;
    for (index, unit) in names.iter().enumerate() {
        if value < step || index == names.len() - 1 {
            return if index == 0 {
                format!("{} {unit}", value as u64)
            } else {
                format!("{value:.1} {unit}")
            };
        }
        value /= step;
    }
    unreachable!()
}

/// "9.8 GB" / "9.1 GiB".
pub fn format_size(bytes: u64, units: Units) -> String {
    match units {
        Units::Decimal => format_scaled(bytes as f64, &DECIMAL_SIZE, 1000.0),
        Units::Binary => format_scaled(bytes as f64, &BINARY_SIZE, 1024.0),
    }
}

/// "12.3 MB/s" / "11.7 MiB/s".
pub fn format_rate(bytes_per_second: f64, units: Units) -> String {
    let clamped = bytes_per_second.max(0.0);
    match units {
        Units::Decimal => format_scaled(clamped, &DECIMAL_RATE, 1000.0),
        Units::Binary => format_scaled(clamped, &BINARY_RATE, 1024.0),
    }
}

/// "29%" — whole numbers; headline values don't dance.
pub fn format_percent(percent: f32) -> String {
    format!("{percent:.0}%")
}

/// "3.69 GHz" above 1000 MHz, "503 MHz" below.
pub fn format_frequency_mhz(megahertz: f64) -> String {
    if megahertz >= 1000.0 {
        format!("{:.2} GHz", megahertz / 1000.0)
    } else {
        format!("{megahertz:.0} MHz")
    }
}

pub fn format_temperature(celsius: f32) -> String {
    format!("{celsius:.0}°C")
}

pub fn format_power(watts: f32) -> String {
    format!("{watts:.0} W")
}

/// "2.1 h" / "34 min" / "12 s" — process ages, uptimes.
pub fn format_duration_seconds(seconds: f64) -> String {
    if seconds >= 86_400.0 {
        format!("{:.1} d", seconds / 86_400.0)
    } else if seconds >= 3_600.0 {
        format!("{:.1} h", seconds / 3_600.0)
    } else if seconds >= 60.0 {
        format!("{:.0} min", seconds / 60.0)
    } else {
        format!("{seconds:.0} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_v1_formatting() {
        assert_eq!(format_size(0, Units::Decimal), "0 B");
        assert_eq!(format_size(999, Units::Decimal), "999 B");
        assert_eq!(format_size(9_800_000_000, Units::Decimal), "9.8 GB");
        assert_eq!(format_size(9_800_000_000, Units::Binary), "9.1 GiB");
        assert_eq!(format_size(1024, Units::Binary), "1.0 KiB");
        assert_eq!(format_size(1000, Units::Decimal), "1.0 kB");
    }

    #[test]
    fn rates_clamp_negative_noise() {
        assert_eq!(format_rate(-5.0, Units::Decimal), "0 B/s");
        assert_eq!(format_rate(12_300_000.0, Units::Decimal), "12.3 MB/s");
    }

    #[test]
    fn frequencies_switch_units_at_a_gigahertz() {
        assert_eq!(format_frequency_mhz(503.0), "503 MHz");
        assert_eq!(format_frequency_mhz(3690.0), "3.69 GHz");
    }
}
