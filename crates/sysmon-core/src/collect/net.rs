// SPDX-License-Identifier: GPL-3.0-or-later
//! Network interfaces: /proc/net/dev counter deltas, classification
//! via /sys/class/net (a device symlink = real hardware — sturdier
//! than name-prefix guessing), addresses via getifaddrs.
//!
//! The headline download/upload rate and the session totals count
//! physical interfaces only (v1 semantics: what the WAN sees, not
//! docker's chatter with itself). Everything is still listed
//! per-interface for the API.

use std::collections::HashMap;
use std::ffi::CStr;
use std::fs;
use std::path::Path;

use crate::snapshot::{InterfaceKind, InterfaceSnapshot, NetworkSnapshot};

use super::read::read_trimmed;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InterfaceCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// /proc/net/dev: two header lines, then
/// `  iface: rx_bytes rx_packets … (8 fields) tx_bytes …`.
pub fn parse_net_dev(content: &str) -> HashMap<String, InterfaceCounters> {
    let mut map = HashMap::new();
    for line in content.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<u64> = rest
            .split_ascii_whitespace()
            .map(|f| f.parse().unwrap_or(0))
            .collect();
        if fields.len() < 16 {
            continue;
        }
        map.insert(
            name.trim().to_string(),
            InterfaceCounters {
                rx_bytes: fields[0],
                tx_bytes: fields[8],
            },
        );
    }
    map
}

fn classify(name: &str) -> InterfaceKind {
    if name == "lo" {
        return InterfaceKind::Loopback;
    }
    // Real hardware has a bus device behind it; veth/bridges/tun don't.
    if Path::new(&format!("/sys/class/net/{name}/device")).exists() {
        InterfaceKind::Physical
    } else {
        InterfaceKind::Virtual
    }
}

/// name → (ipv4 list, ipv6 list) via getifaddrs(3).
fn addresses() -> HashMap<String, (Vec<String>, Vec<String>)> {
    let mut map: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return map;
    }
    let mut cursor = ifap;
    while !cursor.is_null() {
        let entry = unsafe { &*cursor };
        cursor = entry.ifa_next;
        if entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
            continue;
        }
        let name = unsafe { CStr::from_ptr(entry.ifa_name) }
            .to_string_lossy()
            .to_string();
        let family = unsafe { (*entry.ifa_addr).sa_family } as i32;
        let slot = map.entry(name).or_default();
        match family {
            libc::AF_INET => {
                let address = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
                let octets = address.sin_addr.s_addr.to_ne_bytes();
                slot.0
                    .push(format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]));
            }
            libc::AF_INET6 => {
                let address = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in6) };
                let segments: Vec<String> = address
                    .sin6_addr
                    .s6_addr
                    .chunks(2)
                    .map(|pair| format!("{:x}", u16::from_be_bytes([pair[0], pair[1]])))
                    .collect();
                slot.1.push(segments.join(":"));
            }
            _ => {}
        }
    }
    unsafe { libc::freeifaddrs(ifap) };
    map
}

pub struct NetCollector {
    previous: HashMap<String, InterfaceCounters>,
}

impl NetCollector {
    pub fn new() -> Self {
        NetCollector {
            previous: HashMap::new(),
        }
    }

    pub fn collect(&mut self, interval_seconds: f64) -> NetworkSnapshot {
        let counters = fs::read_to_string("/proc/net/dev")
            .map(|content| parse_net_dev(&content))
            .unwrap_or_default();
        let addresses = addresses();

        let mut snapshot = NetworkSnapshot::default();
        let mut names: Vec<&String> = counters.keys().collect();
        names.sort();

        for name in names {
            let current = counters[name];
            let kind = classify(name);
            let (rx_bps, tx_bps) = match self.previous.get(name) {
                Some(previous) if interval_seconds > 0.0 => (
                    current.rx_bytes.saturating_sub(previous.rx_bytes) as f64 / interval_seconds,
                    current.tx_bytes.saturating_sub(previous.tx_bytes) as f64 / interval_seconds,
                ),
                _ => (0.0, 0.0),
            };

            if kind == InterfaceKind::Physical {
                snapshot.download_bps += rx_bps;
                snapshot.upload_bps += tx_bps;
                snapshot.total_received_bytes += current.rx_bytes;
                snapshot.total_sent_bytes += current.tx_bytes;
            }

            let (ipv4, ipv6) = addresses.get(name).cloned().unwrap_or_default();
            let speed_mbps = read_trimmed(format!("/sys/class/net/{name}/speed"))
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|&speed| speed > 0)
                .map(|speed| speed as u32);
            snapshot.interfaces.push(InterfaceSnapshot {
                name: name.clone(),
                kind,
                is_up: read_trimmed(format!("/sys/class/net/{name}/operstate")).as_deref()
                    == Some("up"),
                ipv4,
                ipv6,
                rx_bps,
                tx_bps,
                rx_total_bytes: current.rx_bytes,
                tx_total_bytes: current.tx_bytes,
                speed_mbps,
            });
        }

        self.previous = counters;
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NET_DEV_FIXTURE: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1000000    5000    0    0    0     0          0         0  1000000    5000    0    0    0     0       0          0
enp34s0: 987654321  654321    0    0    0     0          0      1234 123456789  234567    0    0    0     0       0          0
";

    #[test]
    fn net_dev_reads_rx_and_tx_bytes() {
        let map = parse_net_dev(NET_DEV_FIXTURE);
        assert_eq!(map["lo"].rx_bytes, 1_000_000);
        assert_eq!(map["enp34s0"].rx_bytes, 987_654_321);
        assert_eq!(map["enp34s0"].tx_bytes, 123_456_789);
    }
}
