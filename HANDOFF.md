# HANDOFF — SysMon

**Era: v2.2.0** (2026-07-07) — Ben's first-feedback release: fully
sortable table (any column, ▲▼ affordance, persisted), horizontal
table scroll, phosphor's pin button (animated carve + `P`),
theme-carved scrollbars, hover-lit menu rows, "Open in process
viewer" from anywhere, and Ctrl+click multi-select (≤5) feeding a
combined-details window with per-metric share bars. Before that:
v2.1.0 ten-room theme set; the Rust rewrite. The Python/GTK3 v1
lives at tag `v1.0.0`; nothing of it remains in the tree.

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
- **Scrollbars are solid + carved**: handle = `widgets.*.bg_fill`
  (stone / stone_hi / accent), rail = `extreme_bg_color`. Floating
  overlay bars are gone on purpose.
- **The table lives in a horizontal ScrollArea** — inside it,
  `Column::remainder()` would see unbounded width, so the Command
  column takes the viewport-derived spare width at build time.

## Known edges & next-session ideas

- **The Cinnamon bar applet** — the whole reason `tap network`
  exists. Diet: `sysmon tap network -i 2`; click-through:
  `probe network` top_processes + `probe connections`. See
  docs/API.md "For the future Cinnamon bar".
- NVIDIA/Intel GPU cards (honest empty state today).
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
