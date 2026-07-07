// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-process network attribution, two sources merged:
//!
//! 1. **tcp_diag (native, zero-setup)** — a hand-rolled netlink
//!    `sock_diag` dump. `INET_DIAG_INFO` carries each TCP socket's
//!    cumulative `tcpi_bytes_acked` (≈ sent) and
//!    `tcpi_bytes_received`; deltas between samples are real
//!    per-socket rates, no packet capture, no privileges. Socket
//!    inode → pid comes from /proc/<pid>/fd symlinks (own-UID
//!    processes; the dump itself sees *every* socket, so the
//!    connection table still lists other users' sockets, just
//!    unattributed).
//! 2. **nethogs merge** — when nethogs is installed with capture
//!    caps (v1's optional dependency), its trace output supplies
//!    packet-truth rates for all protocols (UDP/QUIC included) and
//!    all users; it takes over as the rate source, tcp_diag keeps
//!    feeding the connection table.
//!
//! Loopback-destination sockets are excluded from *rates* (they're
//! not network usage in the WAN sense — same philosophy as the
//! physical-interfaces-only headline) but stay visible in the
//! connection table.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::snapshot::{ConnectionRecord, ProcessNetRates, ProcessNetSource, ProcessNetTopEntry};

use super::read::SelfInterval;

// ---------------------------------------------------------------- netlink

const NETLINK_SOCK_DIAG: i32 = 4;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const NLM_F_REQUEST: u16 = 0x0001;
const NLM_F_DUMP: u16 = 0x0300;
const NLMSG_DONE: u16 = 3;
const NLMSG_ERROR: u16 = 2;
const INET_DIAG_INFO: u16 = 2;
/// idiag_ext bit requesting INET_DIAG_INFO: 1 << (INET_DIAG_INFO - 1).
const EXT_INFO_BIT: u8 = 1 << (INET_DIAG_INFO as u8 - 1);
/// tcp_info offsets of tcpi_bytes_acked / tcpi_bytes_received:
/// 8 bytes of u8 fields + 24 u32 fields + 2 u64s (pacing rates).
const TCPI_BYTES_ACKED_OFFSET: usize = 8 + 24 * 4 + 16;
const TCPI_BYTES_RECEIVED_OFFSET: usize = TCPI_BYTES_ACKED_OFFSET + 8;

/// One socket out of a diag dump.
#[derive(Clone, Debug)]
pub struct DiagSocket {
    pub protocol: &'static str,
    pub state: u8,
    pub local: String,
    pub remote: String,
    pub remote_is_loopback: bool,
    pub inode: u32,
    pub cookie: u64,
    /// (bytes_acked, bytes_received) — TCP with INET_DIAG_INFO only.
    pub tcp_bytes: Option<(u64, u64)>,
}

pub fn tcp_state_name(state: u8) -> &'static str {
    match state {
        1 => "established",
        2 => "syn-sent",
        3 => "syn-recv",
        4 => "fin-wait-1",
        5 => "fin-wait-2",
        6 => "time-wait",
        7 => "close",
        8 => "close-wait",
        9 => "last-ack",
        10 => "listen",
        11 => "closing",
        12 => "new-syn-recv",
        _ => "unknown",
    }
}

struct NetlinkSocket {
    fd: OwnedFd,
}

impl NetlinkSocket {
    fn open() -> std::io::Result<Self> {
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                NETLINK_SOCK_DIAG,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // Never hang a sampling tick on a wedged kernel reply.
        let timeout = libc::timeval {
            tv_sec: 0,
            tv_usec: 500_000,
        };
        unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &timeout as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );
        }
        Ok(NetlinkSocket { fd })
    }

    /// One SOCK_DIAG_BY_FAMILY dump request.
    fn request(&self, family: u8, protocol: u8) -> std::io::Result<()> {
        let mut packet = Vec::with_capacity(72);
        packet.extend_from_slice(&72u32.to_ne_bytes()); // nlmsg_len
        packet.extend_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
        packet.extend_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
        packet.extend_from_slice(&1u32.to_ne_bytes()); // seq
        packet.extend_from_slice(&0u32.to_ne_bytes()); // pid
        // inet_diag_req_v2
        packet.push(family);
        packet.push(protocol);
        packet.push(EXT_INFO_BIT);
        packet.push(0); // pad
        packet.extend_from_slice(&u32::MAX.to_ne_bytes()); // all states
        packet.extend_from_slice(&[0u8; 48]); // sockid: wildcard
        debug_assert_eq!(packet.len(), 72);

        let sent = unsafe {
            libc::send(
                self.fd.as_raw_fd(),
                packet.as_ptr() as *const libc::c_void,
                packet.len(),
                0,
            )
        };
        if sent != packet.len() as isize {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Read messages until NLMSG_DONE, handing each diag payload to
    /// the callback.
    fn drain(&self, mut on_message: impl FnMut(&[u8])) -> std::io::Result<()> {
        let mut buffer = vec![0u8; 128 * 1024];
        loop {
            let received = unsafe {
                libc::recv(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr() as *mut libc::c_void,
                    buffer.len(),
                    0,
                )
            };
            if received < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut offset = 0usize;
            let received = received as usize;
            while offset + 16 <= received {
                let read_u32 =
                    |at: usize| u32::from_ne_bytes(buffer[at..at + 4].try_into().unwrap());
                let read_u16 =
                    |at: usize| u16::from_ne_bytes(buffer[at..at + 2].try_into().unwrap());
                let message_length = read_u32(offset) as usize;
                let message_type = read_u16(offset + 4);
                if message_length < 16 || offset + message_length > received {
                    break;
                }
                match message_type {
                    NLMSG_DONE => return Ok(()),
                    NLMSG_ERROR => {
                        let errno = i32::from_ne_bytes(
                            buffer[offset + 16..offset + 20].try_into().unwrap(),
                        );
                        return Err(std::io::Error::from_raw_os_error(-errno));
                    }
                    _ => on_message(&buffer[offset + 16..offset + message_length]),
                }
                // NLMSG_ALIGN(4)
                offset += (message_length + 3) & !3;
            }
        }
    }
}

/// Parse one inet_diag_msg payload (+ rtattrs) into a DiagSocket.
fn parse_diag_message(payload: &[u8], protocol: &'static str) -> Option<DiagSocket> {
    // inet_diag_msg: family u8, state u8, timer u8, retrans u8,
    // sockid (48), expires u32, rqueue u32, wqueue u32, uid u32,
    // inode u32 → 72 bytes.
    if payload.len() < 72 {
        return None;
    }
    let family = payload[0];
    let state = payload[1];
    let sport = u16::from_be_bytes([payload[4], payload[5]]);
    let dport = u16::from_be_bytes([payload[6], payload[7]]);
    let src_raw = &payload[8..24];
    let dst_raw = &payload[24..40];
    // sockid: 4..6 sport, 6..8 dport, 8..24 src, 24..40 dst,
    // 40..44 if, 44..52 cookie.
    let cookie_low = u32::from_ne_bytes(payload[44..48].try_into().unwrap());
    let cookie_high = u32::from_ne_bytes(payload[48..52].try_into().unwrap());
    let cookie = (cookie_high as u64) << 32 | cookie_low as u64;
    let inode = u32::from_ne_bytes(payload[68..72].try_into().unwrap());

    let (local_ip, remote_ip, remote_is_loopback) = if family == libc::AF_INET as u8 {
        let local = Ipv4Addr::from(<[u8; 4]>::try_from(&src_raw[..4]).unwrap());
        let remote = Ipv4Addr::from(<[u8; 4]>::try_from(&dst_raw[..4]).unwrap());
        (
            local.to_string(),
            remote.to_string(),
            remote.is_loopback() || remote.is_unspecified(),
        )
    } else {
        let local = Ipv6Addr::from(<[u8; 16]>::try_from(src_raw).unwrap());
        let remote = Ipv6Addr::from(<[u8; 16]>::try_from(dst_raw).unwrap());
        let mapped_loopback = remote
            .to_ipv4_mapped()
            .map(|v4| v4.is_loopback())
            .unwrap_or(false);
        (
            format!("[{local}]"),
            format!("[{remote}]"),
            remote.is_loopback() || remote.is_unspecified() || mapped_loopback,
        )
    };

    // Walk rtattrs for INET_DIAG_INFO (tcp_info).
    let mut tcp_bytes = None;
    let mut cursor = 72usize;
    while cursor + 4 <= payload.len() {
        let attribute_length =
            u16::from_ne_bytes(payload[cursor..cursor + 2].try_into().unwrap()) as usize;
        let attribute_type =
            u16::from_ne_bytes(payload[cursor + 2..cursor + 4].try_into().unwrap());
        if attribute_length < 4 || cursor + attribute_length > payload.len() {
            break;
        }
        if attribute_type == INET_DIAG_INFO {
            let info = &payload[cursor + 4..cursor + attribute_length];
            if info.len() >= TCPI_BYTES_RECEIVED_OFFSET + 8 {
                let acked = u64::from_ne_bytes(
                    info[TCPI_BYTES_ACKED_OFFSET..TCPI_BYTES_ACKED_OFFSET + 8]
                        .try_into()
                        .unwrap(),
                );
                let received = u64::from_ne_bytes(
                    info[TCPI_BYTES_RECEIVED_OFFSET..TCPI_BYTES_RECEIVED_OFFSET + 8]
                        .try_into()
                        .unwrap(),
                );
                tcp_bytes = Some((acked, received));
            }
        }
        cursor += (attribute_length + 3) & !3;
    }

    Some(DiagSocket {
        protocol,
        state,
        local: format!("{local_ip}:{sport}"),
        remote: format!("{remote_ip}:{dport}"),
        remote_is_loopback,
        inode,
        cookie,
        tcp_bytes,
    })
}

/// Dump every TCP (and optionally UDP) socket in the namespace.
fn dump_sockets(include_udp: bool) -> std::io::Result<Vec<DiagSocket>> {
    let socket = NetlinkSocket::open()?;
    let mut sockets = Vec::new();
    let mut dumps: Vec<(u8, u8, &'static str)> = vec![
        (libc::AF_INET as u8, libc::IPPROTO_TCP as u8, "tcp"),
        (libc::AF_INET6 as u8, libc::IPPROTO_TCP as u8, "tcp6"),
    ];
    if include_udp {
        dumps.push((libc::AF_INET as u8, libc::IPPROTO_UDP as u8, "udp"));
        dumps.push((libc::AF_INET6 as u8, libc::IPPROTO_UDP as u8, "udp6"));
    }
    for (family, protocol, label) in dumps {
        socket.request(family, protocol)?;
        socket.drain(|payload| {
            if let Some(parsed) = parse_diag_message(payload, label) {
                sockets.push(parsed);
            }
        })?;
    }
    Ok(sockets)
}

// ------------------------------------------------------- inode → pid map

/// Resolve socket inodes to pids by reading /proc/<pid>/fd symlinks.
/// Two passes: pids already known to own sockets, then a full sweep
/// only if inodes remain unresolved.
struct InodeResolver {
    inode_to_pid: HashMap<u32, i32>,
    socket_pids: HashSet<i32>,
}

impl InodeResolver {
    fn new() -> Self {
        InodeResolver {
            inode_to_pid: HashMap::new(),
            socket_pids: HashSet::new(),
        }
    }

    fn resolve(&mut self, wanted: &HashSet<u32>) {
        // Drop mappings for inodes that no longer exist.
        self.inode_to_pid.retain(|inode, _| wanted.contains(inode));

        let mut unresolved: HashSet<u32> = wanted
            .iter()
            .filter(|inode| !self.inode_to_pid.contains_key(inode))
            .copied()
            .collect();
        if unresolved.is_empty() {
            return;
        }

        // Pass 1: previous socket owners (sockets churn within the
        // same handful of programs).
        let known: Vec<i32> = self.socket_pids.iter().copied().collect();
        for pid in known {
            if unresolved.is_empty() {
                break;
            }
            if !self.scan_pid(pid, &mut unresolved) {
                self.socket_pids.remove(&pid);
            }
        }
        if unresolved.is_empty() {
            return;
        }

        // Pass 2: full sweep.
        let Ok(entries) = fs::read_dir("/proc") else {
            return;
        };
        for entry in entries.flatten() {
            if unresolved.is_empty() {
                break;
            }
            let Some(pid) = entry
                .file_name()
                .to_str()
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<i32>().ok())
            else {
                continue;
            };
            self.scan_pid(pid, &mut unresolved);
        }
    }

    /// Scan one pid's fds; true if the pid was readable.
    fn scan_pid(&mut self, pid: i32, unresolved: &mut HashSet<u32>) -> bool {
        let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            return false;
        };
        let mut owned_any = false;
        for entry in entries.flatten() {
            let Ok(target) = fs::read_link(entry.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|rest| rest.strip_suffix(']'))
                .and_then(|digits| digits.parse::<u32>().ok())
            else {
                continue;
            };
            owned_any = true;
            self.inode_to_pid.insert(inode, pid);
            unresolved.remove(&inode);
        }
        if owned_any {
            self.socket_pids.insert(pid);
        }
        owned_any
    }
}

// ------------------------------------------------------------- nethogs

/// v1's optional nethogs trace reader: `nethogs -t` prints a
/// `Refreshing:` marker then `<path>/<pid>/<uid>\t<sent kB/s>\t<recv
/// kB/s>` lines. Runs on its own thread, keeps the latest complete
/// refresh.
struct NethogsSampler {
    child: Child,
    latest: Arc<Mutex<HashMap<i32, (f64, f64)>>>,
    got_first_refresh: Arc<Mutex<bool>>,
}

impl NethogsSampler {
    fn usable() -> Result<(), String> {
        let Some(path) = ["/usr/sbin/nethogs", "/usr/bin/nethogs"]
            .iter()
            .find(|p| std::path::Path::new(p).exists())
        else {
            return Err(
                "per-process rates cover TCP only right now — for UDP/QUIC and other users' \
                 processes: sudo apt install nethogs && sudo setcap \
                 'cap_net_admin,cap_net_raw+ep' $(which nethogs)"
                    .to_string(),
            );
        };
        if unsafe { libc::geteuid() } == 0 {
            return Ok(());
        }
        let getcap = Command::new("getcap").arg(path).output();
        let capabilities = getcap
            .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
            .unwrap_or_default();
        if capabilities.contains("cap_net_admin") && capabilities.contains("cap_net_raw") {
            Ok(())
        } else {
            Err(format!(
                "nethogs is installed but can't capture packets — grant it once with: \
                 sudo setcap 'cap_net_admin,cap_net_raw+ep' {path}"
            ))
        }
    }

    fn spawn(refresh_seconds: u32) -> std::io::Result<Self> {
        let mut child = Command::new("nethogs")
            .args(["-t", "-d", &refresh_seconds.max(1).to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let latest = Arc::new(Mutex::new(HashMap::new()));
        let got_first_refresh = Arc::new(Mutex::new(false));

        let latest_for_reader = latest.clone();
        let refreshed_for_reader = got_first_refresh.clone();
        std::thread::Builder::new()
            .name("nethogs-reader".to_string())
            .spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                let mut collecting: HashMap<i32, (f64, f64)> = HashMap::new();
                let mut line = String::new();
                loop {
                    line.clear();
                    match std::io::BufRead::read_line(&mut reader, &mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let trimmed = line.trim_end();
                    if trimmed.starts_with("Refreshing") {
                        *latest_for_reader.lock().unwrap() = std::mem::take(&mut collecting);
                        *refreshed_for_reader.lock().unwrap() = true;
                        continue;
                    }
                    let mut parts = trimmed.rsplitn(3, '\t');
                    let (Some(received), Some(sent), Some(identity)) =
                        (parts.next(), parts.next(), parts.next())
                    else {
                        continue;
                    };
                    let (Ok(received_kb), Ok(sent_kb)) =
                        (received.parse::<f64>(), sent.parse::<f64>())
                    else {
                        continue;
                    };
                    // identity = <program path>/<pid>/<uid>
                    let mut identity_parts = identity.rsplitn(3, '/');
                    let _uid = identity_parts.next();
                    let Some(pid) = identity_parts.next().and_then(|p| p.parse::<i32>().ok())
                    else {
                        continue;
                    };
                    if pid <= 0 {
                        continue; // unattributed traffic
                    }
                    let entry = collecting.entry(pid).or_insert((0.0, 0.0));
                    entry.0 += received_kb * 1024.0; // rx
                    entry.1 += sent_kb * 1024.0; // tx
                }
            })?;

        Ok(NethogsSampler {
            child,
            latest,
            got_first_refresh,
        })
    }

    fn rates(&mut self) -> Option<HashMap<i32, (f64, f64)>> {
        if let Ok(Some(_status)) = self.child.try_wait() {
            return None; // died — caller falls back
        }
        if !*self.got_first_refresh.lock().unwrap() {
            return Some(HashMap::new()); // alive, first window pending
        }
        Some(self.latest.lock().unwrap().clone())
    }
}

impl Drop for NethogsSampler {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ----------------------------------------------------------- the source

pub struct NetProcessSample {
    pub rates_by_pid: ProcessNetRates,
    pub connections: Vec<ConnectionRecord>,
    pub source: ProcessNetSource,
    pub hint: Option<String>,
    pub top: Vec<ProcessNetTopEntry>,
}

pub struct NetProcessCollector {
    window: SelfInterval,
    diag_works: bool,
    previous_socket_bytes: HashMap<u64, (u64, u64)>,
    resolver: InodeResolver,
    nethogs: Option<NethogsSampler>,
    nethogs_hint: Option<String>,
    nethogs_wanted: bool,
    nethogs_refresh_seconds: u32,
    comm_cache: HashMap<i32, String>,
}

impl NetProcessCollector {
    /// `enable_nethogs`: long-running callers (GUI, serve) keep a
    /// nethogs child for full-protocol coverage; one-shot probes
    /// shouldn't leave children behind and pass false.
    pub fn new(enable_nethogs: bool, refresh_seconds: u32) -> Self {
        let diag_works = dump_sockets(false).is_ok();
        NetProcessCollector {
            window: SelfInterval::default(),
            diag_works,
            previous_socket_bytes: HashMap::new(),
            resolver: InodeResolver::new(),
            nethogs: None,
            nethogs_hint: None,
            nethogs_wanted: enable_nethogs,
            nethogs_refresh_seconds: refresh_seconds,
            comm_cache: HashMap::new(),
        }
    }

    fn ensure_nethogs(&mut self) {
        if !self.nethogs_wanted || self.nethogs.is_some() || self.nethogs_hint.is_some() {
            return;
        }
        match NethogsSampler::usable() {
            Ok(()) => match NethogsSampler::spawn(self.nethogs_refresh_seconds) {
                Ok(sampler) => self.nethogs = Some(sampler),
                Err(error) => {
                    self.nethogs_hint = Some(format!(
                        "nethogs failed to start ({error}) — falling back to TCP socket counters"
                    ));
                }
            },
            Err(hint) => self.nethogs_hint = Some(hint),
        }
    }

    fn comm_for(&mut self, pid: i32) -> String {
        if let Some(cached) = self.comm_cache.get(&pid) {
            return cached.clone();
        }
        let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|c| c.trim().to_string())
            .unwrap_or_else(|_| format!("pid {pid}"));
        if self.comm_cache.len() >= 4096 {
            self.comm_cache.clear(); // pids recycle; cheap to rebuild
        }
        self.comm_cache.insert(pid, comm.clone());
        comm
    }

    pub fn collect(&mut self, now: std::time::Instant, want_connections: bool) -> NetProcessSample {
        let interval_seconds = self.window.tick(now);
        self.ensure_nethogs();

        let mut sample = NetProcessSample {
            rates_by_pid: ProcessNetRates::new(),
            connections: Vec::new(),
            source: ProcessNetSource::None,
            hint: None,
            top: Vec::new(),
        };

        // --- sock_diag: connection table + TCP rate deltas ---------
        let mut tcp_rates: ProcessNetRates = HashMap::new();
        if self.diag_works {
            match dump_sockets(want_connections) {
                Ok(sockets) => {
                    let wanted_inodes: HashSet<u32> =
                        sockets.iter().map(|s| s.inode).filter(|&i| i != 0).collect();
                    self.resolver.resolve(&wanted_inodes);

                    let mut current_bytes = HashMap::new();
                    for socket in &sockets {
                        let pid = self
                            .resolver
                            .inode_to_pid
                            .get(&socket.inode)
                            .copied()
                            .unwrap_or(-1);

                        let mut socket_rx = None;
                        let mut socket_tx = None;
                        if let Some((acked, received)) = socket.tcp_bytes {
                            current_bytes.insert(socket.cookie, (acked, received));
                            if interval_seconds > 0.0
                                && let Some((previous_acked, previous_received)) =
                                    self.previous_socket_bytes.get(&socket.cookie)
                            {
                                let tx =
                                    acked.saturating_sub(*previous_acked) as f64 / interval_seconds;
                                let rx = received.saturating_sub(*previous_received) as f64
                                    / interval_seconds;
                                socket_rx = Some(rx);
                                socket_tx = Some(tx);
                                if pid > 0 && !socket.remote_is_loopback {
                                    let entry = tcp_rates.entry(pid).or_insert((0.0, 0.0));
                                    entry.0 += rx;
                                    entry.1 += tx;
                                }
                            }
                        }

                        if want_connections {
                            sample.connections.push(ConnectionRecord {
                                pid,
                                process_name: (pid > 0).then(|| self.comm_for(pid)),
                                protocol: socket.protocol.to_string(),
                                local_address: socket.local.clone(),
                                remote_address: socket.remote.clone(),
                                state: if socket.protocol.starts_with("tcp") {
                                    tcp_state_name(socket.state).to_string()
                                } else if socket.state == 7 {
                                    // UDP reuses TCP_CLOSE for unconnected
                                    "unconnected".to_string()
                                } else {
                                    tcp_state_name(socket.state).to_string()
                                },
                                rx_bps: socket_rx,
                                tx_bps: socket_tx,
                            });
                        }
                    }
                    self.previous_socket_bytes = current_bytes;
                    sample.source = ProcessNetSource::TcpDiag;
                }
                Err(_) => {
                    self.diag_works = false;
                }
            }
        }

        // --- nethogs takes over rates when it has data -------------
        let mut used_nethogs = false;
        if let Some(nethogs) = &mut self.nethogs {
            match nethogs.rates() {
                Some(rates) if !rates.is_empty() => {
                    sample.rates_by_pid = rates;
                    sample.source = ProcessNetSource::Nethogs;
                    used_nethogs = true;
                }
                Some(_empty_first_window) => {}
                None => {
                    self.nethogs = None;
                    self.nethogs_hint =
                        Some("nethogs exited — rates fall back to TCP socket counters".to_string());
                }
            }
        }
        if !used_nethogs {
            sample.rates_by_pid = tcp_rates;
            if sample.source == ProcessNetSource::TcpDiag {
                sample.hint = self.nethogs_hint.clone();
            } else {
                sample.hint = Some(
                    "no per-process source available (netlink sock_diag denied and no nethogs)"
                        .to_string(),
                );
            }
        }

        // --- top list ----------------------------------------------
        let mut entries: Vec<(&i32, &(f64, f64))> = sample.rates_by_pid.iter().collect();
        entries.sort_by(|a, b| {
            (b.1.0 + b.1.1)
                .partial_cmp(&(a.1.0 + a.1.1))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sample.top = entries
            .into_iter()
            .filter(|(_, (rx, tx))| rx + tx >= 1.0)
            .take(10)
            .map(|(&pid, &(rx, tx))| ProcessNetTopEntry {
                pid,
                name: self.comm_for(pid),
                rx_bps: rx,
                tx_bps: tx,
            })
            .collect();

        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_state_names_cover_the_table() {
        assert_eq!(tcp_state_name(1), "established");
        assert_eq!(tcp_state_name(10), "listen");
        assert_eq!(tcp_state_name(99), "unknown");
    }

    #[test]
    fn tcp_info_offsets_match_uapi_layout() {
        // 8 u8-ish bytes, 24 u32s, then pacing_rate, max_pacing_rate,
        // bytes_acked, bytes_received.
        assert_eq!(TCPI_BYTES_ACKED_OFFSET, 120);
        assert_eq!(TCPI_BYTES_RECEIVED_OFFSET, 128);
    }

    /// Live: dump this machine's sockets and require that our own
    /// test process's listening/connected sockets resolve to us.
    #[test]
    fn live_dump_sees_own_socket() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();

        let sockets = dump_sockets(false).expect("sock_diag dump");
        let mine = sockets
            .iter()
            .find(|s| s.protocol == "tcp" && s.local.ends_with(&format!(":{port}")))
            .expect("own listener in dump");
        assert_eq!(tcp_state_name(mine.state), "listen");
        assert!(mine.inode > 0);

        let mut resolver = InodeResolver::new();
        let wanted: HashSet<u32> = [mine.inode].into_iter().collect();
        resolver.resolve(&wanted);
        assert_eq!(
            resolver.inode_to_pid.get(&mine.inode).copied(),
            Some(std::process::id() as i32),
            "listener inode must resolve to this test process"
        );
    }
}
