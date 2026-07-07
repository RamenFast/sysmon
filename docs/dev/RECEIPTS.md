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
