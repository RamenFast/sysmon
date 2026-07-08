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
