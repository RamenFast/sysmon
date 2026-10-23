# HANDOFF — SysMon

**Era: v2.2.2** (2026-07-07) — the "refix everything" release, fixes
by code where v2.2.1 had fixed by diagnosis: pin button's active
glyph stays ink (phosphor's rule — it no longer converges with the
accent-tinted face; that wash-out is why Ben hid it), scrollbar
handles wear text-grade tones (ink_2/ink/accent, 11 px), the GUI
names its render adapter and discloses a CPU-rasterizer fallback
loudly (toast + stderr + `status.renderer_hint` — never silently
slow), the sensors walk survives hwmon index gaps (k10temp Tccd1 was
silently dropped; new accuracy identity pins it) and grew voltage
rails / power vs cap / fan max / drive temps with the drive named,
and the icon is finally the round-1 ask: phosphor-language magnifying
glass over a traced screen. Before that: v2.2.0 the seven asks;
v2.1.0 ten rooms; the Rust rewrite. The Python/GTK3 v1 lives at tag
`v1.0.0`; nothing of it remains in the tree.

## What this is now

One binary (`crates/sysmon-app`, lib + bin) over one engine
(`crates/sysmon-core`): egui GUI as the default command, plus
`probe`/`tap`/`ctl`/`schema`/`serve`/`--background` on the phosphor
contract (JSON envelopes, errors carry `fix`, exit 0/2/3/4, control
socket at `$XDG_RUNTIME_DIR/sysmon/ctl.sock`, single owner, plain
relaunch raises a same-version GUI and replaces an older one —
upgrade day always shows the installed version). Ten palettes (Blossom Dark default; `amoled` is
v1's true-black look and v1 settings migrate to it; paper/basalt/
amber/chromacore are the distinct-room additions), stone-carved
primary controls, sharp corners everywhere.

## Load-bearing decisions

- **Per-process net**: netlink sock_diag TCP byte counters (hand-
  rolled client in `collect/net_process.rs`, offsets asserted against
  uapi) + inode→pid two-pass resolver; nethogs merges in as the rate
  source when cap-blessed. Sources are disclosed on the wire.
  Loopback excluded from rates, visible in `connections`.
- **Every collector owns its measurement window** (`SelfInterval`) —
  mixed-section socket clients can't skew each other's rates.
- **serve samples on demand only** — zero clients = zero work
  (receipt: 0.00% / 5.6 MB over 60 s).
- **Memory identity** is v1/psutil's (`used = total − available`);
  all identities + tolerances in `docs/dev/ACCURACY.md`. Accuracy
  suite failures mean fix the collector, never the tolerance.
- **Pop-outs are immediate viewports** fed from Arc'd shared state;
  the sampler pokes each open viewport so they update while the main
  window is minimized. Dock-on-drop needs X11 button state (x11rb);
  elsewhere the ⧉ toggle does it.
- **settings.json is v1-compatible** (era sniffed by
  `settings_version` absence; v1 `blossom` → `amoled`).

## How to verify anything

- `cargo test` — 41 green: parsers, accuracy cross-checks (live
  authorities), net attribution (needs network), icon chain, kittest
  a11y interactions.
- `scripts/e2e.sh out/ target/release/sysmon` — Xvfb+openbox live
  run: 12 theme×page screenshots, ctl-driven pop-out docked by drag,
  single-instance, contract checks, clean shutdown.
- `SYSMON_DEBUG_FPS=1` / `SYSMON_DEBUG_DRAG=1` env flags print frame
  cadence / drag-dock signals to stderr.

## Ship mechanics

**Every commit that lands on main is a release**: bump
`[workspace.package].version` on the feature branch, merge `--no-ff`,
then `scripts/release.sh <notes-file>` tags, builds deb + rpm +
source tarball + SHA256SUMS, publishes the GitHub release, and
prints the local install command. main never carries an unreleased
version. Version law: workspace == `--version` == filenames == tag.

## 2.2 load-bearing details

- **Selection colors ARE the graph palette's five series** — slot i
  of the multi-selection wears series color i everywhere (row tint,
  overview rows, combined window chips + share bars). Changing the
  graph palette recolors the selection; that identity is the point.
- **Menus use custom rows** (`cards.rs::menu_option_row/chip/
  check_row`), not egui radios — egui radios paint no hover at all
  (Ben's complaint). Hover glow everywhere else comes from
  `hovered.weak_bg_fill` (theme.rs), which egui menu buttons use.
- **Scrollbars are solid, ink-handled**: handle = `widgets.*.bg_fill`
  (ink_2 / ink / accent-while-dragged, 11 px), rail =
  `extreme_bg_color`. Stone handles (v2.2.0) blended into every room
  — the ink tones hold text-grade contrast by construction. egui has
  no handle-stroke channel; contrast is fill + width only.
- **Glyph buttons: the glyph never chases the accent** (phosphor's
  bevel_toggle rule). Active = face eased toward accent (0.22 mix,
  cap 0.32) + accent border + inset bevel; the glyph stays ink. The
  v2.2.0 port recolored the active glyph toward the accent too — it
  converged with the face and washed out in every room; that's the
  bug Ben hid the pin button over.
- **Never silently slow**: `SysMonApp::new` reads the wgpu adapter;
  `DeviceType::Cpu` (llvmpipe) ⇒ stderr with fix + toast +
  `status.renderer`/`renderer_hint`. Xvfb/e2e always runs in this
  mode — the toast is IN the first ~6 s of screenshots by design.
- **hwmon channel indices gap** (k10temp: temp1 + temp3, no temp2) —
  the sensors walk enumerates directory entries, never counts up
  from 1; `accuracy.rs::sensor_channels_match_sysfs_files` pins it.
  Drive chips resolve their block device + model (`device/block/*`,
  `device/model`), so four `drivetemp` chips read as sda…sdd.
- **The table lives in a horizontal ScrollArea** — inside it,
  `Column::remainder()` would see unbounded width, so the Command
  column takes the viewport-derived spare width at build time.

## Known edges & next-session ideas

- **3.8 Threads & CPUs** (`collect/threads.rs`, `gui/threads_view.rs`):
  `ThreadSampler` reads one pid's `/proc/<pid>/task/*/stat` (each view
  owns one: Inspector, each details window, each Threads window,
  `probe threads`). The Threads window (`details::threads_window`,
  `AppAction::OpenThreads`, `ctl threads <pid>`) is what the shared
  `process_context_menu` opens, so every process row gets it — new
  process rows should call that menu, not grow their own;
  `OccupancySampler` walks every task for `cpu.per_core_top` only when
  `Wants::cpu_occupants` (window: while a CPU card shows). Attribution
  is last-CPU (stat field 39), disclosed as `cpu_attribution`; threads
  born inside a window count all their ticks (`window_ticks`, against
  /proc/uptime). Accuracy: `threads_match_ps_and_a_pinned_thread_lands_on_its_cpu`.
  Present mode is `AutoNoVsync` (`gui/mod.rs`): with FIFO a SysMon window
  the compositor isn't showing blocked the shared frame and froze every
  window. Hardware lanes are "CPU n" in visible text; AccessKit labels
  keep "CPU thread n" (tests read them).

- **3.7.1 pop-outs fit their card** (`app.rs::PopoutFit`): a pop-out
  asks for its card's size (width from `CardContext::want_width`, which
  the header and `stat_grid` report while drawing; height from the
  scroll content) once the first full sample is in, re-asks if Sway
  drops a request, and stops for good once the user resizes it.
  `stat_grid` drops to one pair a row when two would squeeze (the CPU
  card's "Avg clock2.71 GHz" at 380 px). Wayland reports no
  `inner_rect`: compare against `ctx.content_rect()`. Debug:
  `SYSMON_DEBUG_FIT=1`.

- **3.7 the NVIDIA card** (section `nvidia`, `collect/nvidia.rs`): the
  RTX 3070 beside the R9700 is compute-only (no nvidia-drm, no DRM
  node), so NVML is the only source; opened by dlopen on the first
  sample that wants it. Overview: the card rides in the AMD card's slot,
  under it (`app.rs::overview_slots`), so a machine without one lays
  out as before and no other card changes column; no menu row without a
  card. History records it (`nvidia` record, `nvidia.*` metrics,
  `nvidia_programs` in rollups, serde-defaulted, so 3.6 rollups and the
  archive read unchanged). The spec sheet now walks PCI display class
  too (it said "every PCI display device" and missed the 3070), with the
  slot's PCIe generation from NVML (an idle card drops to Gen 1).
  Accuracy: `nvidia_matches_nvidia_smi`. Edges: the Processes table's
  GPU % / VRAM columns are still the AMD card's; the NVIDIA card has no
  "more" link for that reason. Only NVML index 0 is read.

- **3.6 one Overview** (`docs/dev/OVERVIEW-VIEW.md`, revision 3): the
  Performance page is gone; its graphs became the cards' graphs and
  its boxes card stats. The CPU card switches total · threads · grid
  (threads, a coloured line per thread, is the default; colours from
  `theme::thread_colors`). Settings migrate (`combined` → `total`,
  `auto`/`per_thread` → `threads`, start page `performance` →
  `overview`); `ctl page performance` exits 3 with the fix. Process
  rows hover with `Palette::row_hover()` and a 2 px accent edge; the
  table body scopes the row tokens because the theme's
  `widgets.hovered.bg_fill` is the ink (scrollbar handles). Top-3 and
  Inspector-children rows take a pid-keyed click id (P5), and each
  card has its own id scope (`push_id(section)`), because a child Ui's
  stable id is the same for siblings. Every viewport carries app_id
  `sysmon`; the greenhouse Sway config floats them. Edges: the process
  table's own rows are still positional ids inside egui_extras (a
  re-sort between paint and click can retarget a click; widening the
  hit target to a pid would need our own table body).
  Testing trap found on the way: a shell inside Ben's Sway session
  inherits `LD_LIBRARY_PATH=/opt/swayfx-nexus/lib/...` (the sway-nexus
  launcher exports it). An X11 SysMon launched with it mixes SwayFX's
  libxkbcommon with the system libxkbcommon-x11 and segfaults on quit
  or on a keymap reload (`xdotool type`), 3.5.0 and 3.6.0 alike; with
  it unset both exit 0. e2e already unsets it; do the same in any
  ad-hoc X11 harness.
- **3.5 long-term history** (`sysmon_core::history`): hourly gzip
  files for 7 days, then whole days packed losslessly into
  `archive/<day>.jsonl.zst` (zstd-19 frame per hour, index + rollups
  in skippable frames, verified before sources go). Nothing is deleted
  by default; keep (1 y / 90 d) and size cap (1/4/16 GB) are opt-in,
  and past 1 GB with no cap the status warns. Each sealed hour has a
  rollup (mergeable stats + sketch), so a year summarizes in ~0.7 s
  with numbers equal to a record-by-record read (`--raw` checks).
  Menu: size per tier + growth per month/year, Export… (.tar.zst),
  Clear… (confirm). CLI: `history --info | --export | --clear --yes |
  --pack`; a recorder holding the lock does clear/pack itself via the
  socket `history` verb. `serve --record` now stands by while a window
  records and resumes when it closes (Ben's `sysmon-record` user
  service depends on it). Ideas: a range picker on the graphs that
  draws from the disk history (rollups make it cheap); a `ctl` verb to
  bookmark a moment ("game started"); per-process CPU and RAM in the
  record (only GPU programs today); an optional per-minute downsample
  only if a multi-year view ever needs it (lossless is enough today).
- **Per-process VRAM** reads `drm-total-vram − amd-evicted-vram`.
  If a future kernel fixes `drm-resident-vram`, prefer it only after
  the accuracy check (Σ unshared ≤ card) proves it.
- **The Cinnamon bar applet** — the whole reason `tap network`
  exists. Diet: `sysmon tap network -i 2`; click-through:
  `probe network` top_processes + `probe connections`. See
  docs/API.md "For the future Cinnamon bar".
- Intel GPU cards (honest empty state today); NVIDIA landed in 3.7.
- Wayland drag-dock (needs a compositor-side story; toggle works).
- RSS trim: worth trying eframe's glow backend behind a feature flag
  and re-measuring (wgpu costs ~40 MB of the 148 MB llvmpipe RSS).
- Real-desktop idle numbers (vulkan) recorded in the README after
  install day; llvmpipe numbers live in docs/dev/RECEIPTS.md.
- `probe` human rendering could grow sensors/connections sections.

## Receipts of record

`docs/dev/RECEIPTS.md` (tests, e2e, resource table),
`docs/dev/ACCURACY.md` (identities + tolerances),
`docs/dev/PLAN.md` (the approved era plan).

## Documentation map (written for zero-context agents)

`CLAUDE.md` (laws + commands, auto-loaded) → `docs/AGENTS.md`
(driving) · `docs/ARCHITECTURE.md` (the developer map) ·
`docs/dev/EXTENDING.md` (touch-point checklists for every common
change) · `docs/dev/TESTING.md` (test layers + GUI-testing gotchas).
Keep them true: the three user-facing mirrors (schema, manpage,
docs/API.md) must agree after any surface change — EXTENDING.md
says so per recipe.
