# Accuracy — what each number means and who vouches for it

Two layers keep every figure honest, and both run on real hardware:

- **`crates/sysmon-core/tests/accuracy.rs`** (14 tests, in `cargo test`):
  each reading cross-checked live against an independent authority.
- **`scripts/accuracy-audit.sh`** (56 checks): every number the app
  shows next to the tool a skeptical human would open beside it
  (`free`, `turbostat`, `mpstat`, `sensors -j`, `lspci`, `df`, `findmnt`,
  `ps`, `iostat`, `pidstat`, udev DMI, `/sys/firmware/memmap`), idle,
  under `stress-ng` load, and under a known `fio` writer. Writes `report.json` + `report.md`.

**A failure is a collector bug.** Tolerances are the contract and are
never widened to turn a red test green. The ways each number could lie
are listed first, in [`FAILURE-MODES.md`](FAILURE-MODES.md); check ids
here match ids there.

## The definitions

| Reading | SysMon's definition | Authority | Tolerance (why) |
|---|---|---|---|
| Memory usable | `MemTotal` | `free -b` | exact (static) |
| Memory installed | Σ DMI memory devices (`/run/udev/data/+dmi:id`) | same, and `usable ≤ firmware System RAM ≤ installed` via `/sys/firmware/memmap` | exact |
| Memory used | `MemTotal − MemAvailable` (psutil identity — **not** htop's total−free−buff−cache) | `free -b` available | 3% of RAM (allocations move between reads) |
| Memory cached | `Cached + SReclaimable`; buffers separate | `free -b` buff/cache = cached+buffers | 3% of RAM |
| RAM speed | DMI `CONFIGURED_SPEED_MTS` (what it runs at), rated beside it | udev DMI | exact |
| Disk size / used | statvfs (df's "size"/"used") | `df -B1` | exact / 0.5% + 32 MiB |
| CPU busy % | per-core `1 − Δ(idle+iowait)/Δtotal` from /proc/stat; guest folded into user | `mpstat` 100 − idle − iowait, same window | ±8 pp (two 2 s windows, not one) |
| CPU iowait % | Δiowait/Δtotal — reported, never counted as busy | `mpstat %iowait` | ±8 pp |
| CPU busy clock | each core's **delivered** clock over the window (ACPI CPPC delivered/reference × nominal — the APERF/MPERF ratio turbostat reads), weighted by its busy share | `turbostat Bzy_MHz`, same window; test: own CPPC read | ±700 MHz audit / ±150 MHz test |
| CPU temperature | k10temp `Tctl` (Zen 3: Tctl = Tdie) / coretemp Package | `sensors -j` | next read |
| GPU clocks | mean of `gpu_metrics` reads across the window (20/s) | independent 100 Hz poll, same window | RMS error ≤ 0.6 σ over six windows |
| GPU temps / power / fan / VRAM | hwmon by **label** (channel order isn't ABI); `mem_info_vram_*` | `sensors -j`, sysfs | next read |
| GPU name | `lspci -mm` (pci.ids) → amdgpu.ids → "AMD GPU 1002:xxxx", never "Device 7551" | `lspci` | exact |
| Process RSS | `statm` field 2 (the kernel's exact per-CPU sum — `stat` runs MiB low) | `ps -o rss=` | 10% / 8 MiB |
| Process CPU % | Δ(utime+stime) ticks / window; `probe` uses a 1 s window so one tick is 1% | contract test: window ≥ 1 s. `pidstat` compared by hand in the 3.1 audit (not yet scripted: SERIOUS-TODOS) | — |
| Process name | `display_name`: a generic thread comm (MainThread) → its executable; `python3` → its script | jq: none left generic | 0 |
| Process census | /proc numeric dirs | `ps -e` | ±50 (churn) |
| Net totals | Σ physical interfaces (a `device` symlink in /sys/class/net) | /sys/class/net/*/statistics | 16 MiB |
| Sensor census | one reading per readable hwmon channel; index gaps honored | `sensors -j temp*_input` count | exact |
| Implausible inputs | −62 °C or exactly 0 °C with no limits → `plausible: false` (unconnected thermistors, PCH stubs) | jq over probe | 0 unmarked |

## Deliberate identity choices

- **Usable vs installed.** 128 GiB of sticks shows as *Installed
  137.4 GB*; the kernel can use *135.0 GB* after firmware and GPU
  reservations; the gap is *Reserved*. Sizes are decimal (GB)
  everywhere by default. RAM is sold in binary units, which the hover
  note says out loud.
- **Used memory** is `total − available`: "how much RAM is spoken
  for", matching psutil and GNOME System Monitor. htop's smaller figure
  answers a different question.
- **Two CPU clocks.** *Busy clock* is what the working cores ran at
  (the headline). *Avg clock* is the mean of all 32 threads' requested
  clocks, parked ones included, which mostly measures idling.
- **Network headline** counts physical interfaces only, so VPN and
  container interfaces don't double-count WAN traffic.
- **Per-process CPU** can exceed 100% for multi-threaded processes
  (clamped at cores × 100), the htop convention.
- **Every reading's first sample** has no window to diff against. A
  one-shot probe and a fresh `sysmon serve` take a real window first
  rather than answer with zeros.

## Results, this machine (2026-10-01, 3.1, kernel 7.0.0-28)

Ryzen 9 5950X · Radeon AI PRO R9700 (Navi 48) · 4 × 32 GB DDR4 ·
ASRock X570 Phantom Gaming 4 (nct6798) · Mint 22.3.

`scripts/accuracy-audit.sh`: **56 pass, 0 fail** (idle, `stress-ng`
load with row L1 proving the load was still running, and a 64 MB/s
`fio` direct-I/O writer). Selected rows:

| id | reading | SysMon | authority |
|---|---|---|---|
| M2 | installed RAM | 137 438 953 472 | DMI: 137 438 953 472 |
| M4 | usable RAM | 134 979 108 864 | `free -b`: 134 979 108 864 |
| C1 | busy clock (load) | 3908 MHz | turbostat Bzy_MHz: 3969 |
| C2 | CPU busy % (load) | 33.0 | mpstat: 32.3 |
| C2b | iowait % (load) | 12.2 | mpstat: 11.0 |
| C3 | CPU temp (load) | 90.5 °C | sensors: 90.5 °C |
| G3 | VRAM used | 938 074 112 | sysfs: 938 074 112 |
| S0 | temperature channels | 28 | sensors -j: 28 |
| D3 | partition write rate | 67.3 MB/s | iostat: 67.1 MB/s |
| D5 | partition util % | 15.4 | iostat: 16.2 |
| P6b | writer's disk write rate | 67.3 MB/s | pidstat -d: 67.1 MB/s |

The 3.0.3 baseline on the same machine was 34 pass / 16 fail. What
moved, each with a red test first, is in the 3.1 release notes and
`git log 0238481~1..`. The audit report itself is reproducible:
`scripts/accuracy-audit.sh target/release/sysmon audit-out/<name>`.
