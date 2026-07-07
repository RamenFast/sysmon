# Accuracy — what each number means and who vouches for it

Every figure SysMon v2 reports is cross-checked live by
`crates/sysmon-core/tests/accuracy.rs` against an independent
authority. The suite runs in `cargo test` on the machine that ships
the build. **A failure is a collector bug; tolerances are the
contract and never get widened to make a red test green.**

| Reading | SysMon's definition | Authority | Tolerance (why) |
|---|---|---|---|
| Memory total | `MemTotal` | `free -b` | exact (static) |
| Memory used | `MemTotal − MemAvailable` (v1/psutil identity — **not** htop's total−free−buff−cache) | `free -b` available | 3% of RAM (allocations move between reads) |
| Memory cached | `Cached + SReclaimable`; buffers separate | `free -b` buff/cache = ours cached+buffers | 3% of RAM |
| Disk size | statvfs `f_blocks × f_frsize` | `df -B1` size | exact (static) |
| Disk used | statvfs `(f_blocks − f_bfree) × f_frsize` (df's "used") | `df -B1` used | 0.5% + 32 MiB (writes land between reads) |
| CPU busy % | per-core `1 − Δ(idle+iowait)/Δtotal` from /proc/stat; guest excluded from total (already in user) | pinned-spinner experiment + per-core mean | spinner ≥ 60% of one core's share; mean−overall < 1pp |
| Process RSS | stat rss pages × page size | `ps -o rss=` | 10% / 8 MiB (test allocates while running) |
| Process census | /proc numeric dirs, one record each | `ps -e \| wc -l` | ±50 (spawn/die churn) |
| Net totals | Σ physical interfaces (a `device` symlink in /sys/class/net = physical) | independent /proc/net/dev parse | 16 MiB (traffic flows between reads) |
| VRAM total | `mem_info_vram_total` | sysfs re-read | exact (static) |
| VRAM used | `mem_info_vram_used` | sysfs re-read | 256 MiB (allocations move) |
| GPU edge temp | hwmon `temp1_input` | sysfs re-read | 10 °C (thermal drift between reads) |
| Load 1m | /proc/loadavg field 1 | /proc re-read | 0.5 (5 s kernel recompute tick) |
| Uptime / boot | /proc/uptime; btime from /proc/stat | `btime + uptime ≈ now` | 3–5 s |

## Deliberate identity choices

- **used memory**: `total − available` answers "how much RAM is
  spoken for" and matches v1, psutil, and GNOME System Monitor.
  htop's figure is smaller (it excludes reclaimable cache from used);
  both are correct answers to different questions. Ours is stated in
  the schema description.
- **network headline**: physical interfaces only. Container/VPN
  virtual interfaces are listed individually but do not inflate the
  WAN figure by double-counting.
- **per-process CPU**: can exceed 100% for multi-threaded processes
  (clamped at cores×100), matching v1/htop convention.
- **process CPU on first sight**: 0% until a second sample exists —
  never a guess.

## Results, this machine (2026-07-07, kernel 6.17.0-35)

`cargo test -p sysmon-core --test accuracy` — 8/8 green in four
consecutive runs (one under a 100%-pinned core, by design of the CPU
test). Census cross-check at run time: sysmon 2741 vs `ps -e` 2740
(the delta is the test process itself).
