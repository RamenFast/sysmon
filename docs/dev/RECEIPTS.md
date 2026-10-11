# v2 build receipts (2026-07-07, this machine)

## Test suite
`cargo test --release`: 41 tests green, 0 failures —
23 core unit (parsers, fixtures) · 8 accuracy cross-checks (free/df/
ps//proc/sysfs authorities) · 4 net-attribution live (curl pinned at
2 MB/s: tcp_diag read 2.2 MB/s, nethogs takeover 1.8 MB/s) · icon
resolution through the real Blossom→Papirus-Dark chain · live smoke ·
2 settings-migration · 1 kittest UI interaction sequence (AccessKit:
page switch, filter narrows to "1 of N", theme switch applies the
palette + companion, pause glyph flips).

## scripts/e2e.sh (Xvfb + openbox, scratch config/socket)
All checks green in one run: gui owns socket → 12 theme×page ctl
screenshots → pop-out opened by ctl, docked by drag-and-drop over
the main window (button-held windowmove + release, the real WM-drag
signal set) → second launch raised the instance exit 0 → probe rode
the socket → tap streamed → bad section exit 3 → quit unlinked the
socket.

## Self-resource, 60 s idle at the default 2 s interval
Same virtual display (llvmpipe SOFTWARE rendering — v2's worst case;
the GUI pays CPU for every repaint that real hardware pays on the
GPU):

| build | idle CPU (one core) | RSS |
|---|---|---|
| v1 python/GTK3 (shipped 1.0.0, clean config) | 13.0% | 118.8 MB |
| **v2 GUI (release)** | **6.0%** | 148.3 MB |
| **v2 serve, no clients** | **0.00%** | **5.6 MB** |

v2 idle frame rate: ~1.5 fps (sampler tick + safety repaint; no
repaint loops). Real-desktop (vulkan) figures to be captured at
install and recorded in the README.

Notes: v1 crashes on startup if settings.json carries a v2 theme id
(KeyError — no fallback); v2's loader degrades unknown ids gently.
The serve figure is the design receipt: zero clients = zero sampling.

## v2.2.2 addenda (2026-07-07)

- **Never-silently-slow**: on Xvfb (llvmpipe) the fresh GUI printed
  the CPU-rasterizer disclosure + fix to stderr, toasted in-window,
  and `ctl status` carried `renderer: "llvmpipe (LLVM 20.1.2, 256
  bits) · Vulkan"` + `renderer_hint`. On real hardware the fields
  name the actual GPU and the hint is absent.
- **Idle self-resource re-measured** (30 s window, 2 s interval,
  main window only, llvmpipe): 7.9% of one core / 162.9 MB RSS —
  same class as the v2.2.0 row (this run carried the sensors card's
  four new drive rows + a fuller sensor walk). With a sensors
  pop-out open: 9.7%. Repaints stay event-driven: zero between
  sampler ticks.
- **Sensor census identity added**
  (`sensor_channels_match_sysfs_files`): reading counts equal
  readable sysfs channel files, exactly — this machine: k10temp
  Tctl+Tccd1 (temp2 is a GAP), amdgpu 3 temps + fan (630/3300 rpm)
  + vddgfx 0.800 V + PPT 8 W/cap 200 W, drivetemp × 4 with drives
  resolved (sda/sdb KINGSTON, sdc Hitachi — idles at 61 °C, worth
  eyes — sdd SAMSUNG). Cross-checked against `sensors` output 1:1.
- **Icon chain**: guard-band scan clean (no colored pixel on the
  8/248 frame); `app_icon_renders` pins SVG→resvg→128 px RGBA;
  titlebar icon verified live on the Xvfb instance.

## v3.5.0 addenda (2026-10-11): the long-term history

**Compression, on a copy of the real history** (this machine: 24
temperatures, 8 fans, 5 disks, 10 GPU programs a sample; 5 s cadence;
2,781 samples over 4.6 h, idle and Arc Raiders):

| form | bytes a sample | vs raw |
|---|---|---|
| raw JSON line (the open hour) | 3,398 | 1× |
| gzip -6 sealed hours (hot tier) | 141 | 24× |
| packed day file, `archive/<day>.jsonl.zst` | **103** | **33×** |
| · of which hour frames (zstd -19) | 94.3 | |
| · of which rollups / index | 8.4 / 0.3 | |

Codec choice, one real hour (716 samples, 2.43 MB raw): gzip -6 84 KB,
gzip -9 80 KB, zstd -12 61 KB, zstd -19 55 KB, xz -6 59 KB, xz -9e 52 KB.
zstd -19 with window/chain/hash 22/22/21 gives the same 55 KB at 32 MB
peak memory instead of 57 MB. xz -9e is 6–8% smaller but decodes ~3×
slower (two real hours, 30 decodes: zstd 0.05 s, xz 0.12 s), and zstd's
skippable frames carry the index and rollups inside the day file while
`zstd -dc` still prints only the lines. Repack of the copy: 5 hours packed in
1.9 s wall at nice 19, 47 MB peak; every record byte-identical after
(md5 of `history all --records`), the summary identical with rollups
and with `--raw`; `zstd -dc` on the day file prints its 2,781 lines.

**Growth** (from the measured 103 B): 5 s → 2.2 MB a day, ~54 MB a
month, ~650 MB a year; 2 s → ~135 MB a month, ~1.6 GB a year. The hot
week adds a fixed ~18 MB at 5 s. Defaults: keep forever, no cap; the
status warns past 1 GB (≈1.5 years at 5 s).

**Query speed** (`history_scale::long_ranges_stay_fast`, synthetic
records from a real template, release build, nice 19, while a game
ran on the same machine):

| store | `history gpu --since 30d` with rollups | record by record | whole store |
|---|---|---|---|
| 30 days at 2 s (1.30 M records; 23 archive days 11.3 MB + 175 hourly 48 MB) | **0.92 s** (672/720 hours merged) | 13.1 s | 0.92 s |
| 1 year at 10 s (3.15 M records; 358 archive days 54 MB) | **0.21 s** | 2.6 s | **0.74 s** (vs 33.7 s) |
| 1 week at 2 s (302 k records) | 0.91 s for the week | 3.0 s | |

The same two stores kept (`SYSMON_SCALE_KEEP=1`) and timed through the
real binary, `XDG_STATE_HOME=<scratch> sysmon history gpu --since 30d`,
three runs each, nice 19, a game running: 30-day store **1.01 s** (13.2 s
with `--raw`); 1-year store **0.26 s** (2.7 s with `--raw`).

Rollups never change a number a person sees: counts, min, max, p95,
latest and ceilings are exact; sums agree to 1e-9 relative (hour
subtotals round differently from 1.3 M single adds, in the 14th digit).
The unit test `merged_rollups_summarize_exactly_like_the_records`
checks every section over ranges that cut hours, align with them and
span a recording gap.

**Recorder cost** (`serve --record --interval 5`, nice 10, 60 s, scratch
copy, first pass building 4 hour rollups): 0.58% of one core, 8.7 MB
RSS (peak 10.9 MB).

