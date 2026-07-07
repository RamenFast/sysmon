# SysMon v2.0.0 — Rust engine + egui rewrite, agent API, ship

## Context

SysMon v1 (current repo, installed as deb 1.0.0) is a Python/GTK3 theme-aware system monitor:
Overview page with GPU/Memory/CPU/Network/Disks cards (graphs, stat grids, top-3 lists, pop-out
windows with drag-to-dock), a sortable/filterable process table with icons and end/kill/renice
actions, 5 appearance modes, 7 graph palettes, compact mode, units toggle. Ben's brain dump asks
for: a Rust engine rewrite with the same-or-improved UI ("don't base it off gtk4, do what Phosphor
did"), minimal resource usage, a simple efficient programmatic API for ANY system state (networking
first — a desktop-bar integration comes later: up/down at a glance, click → which processes are
using what), richer data fields, real app icons that look nice, extensive testing (interactions,
screenshots, pop-outs), accuracy-discrepancy testing with full fixes, a polish pass, install on his
machine, and a GitHub update. No subagents; fully autonomous; sudo via `sudoplz`.

"What Phosphor did" (verified in ~/Dev/ClaudeWorkspace/phosphor @4.0.1): one Rust binary,
subcommand-first with the GUI as the default command; egui 0.33 + wgpu 27 + winit 0.30 (NOT GTK);
self-drawn chrome from a token-driven `Palette` table (Blossom Dark default, sharp corners, carved
stone controls); an agent surface — `probe`/`tap`/`ctl`/`schema` — over a Unix-domain NDJSON
control socket in `$XDG_RUNTIME_DIR`, one JSON envelope per one-shot (`{"status","tool","version",
"ts",…}`, errors always carry `fix`), exit codes 0/2/3/4, JSON auto-on when piped; `--background`
re-execs under `xvfb-run` (env-guard against recursion) for a headless drivable GUI; deb+rpm+source
packaging with hand-rolled build scripts.

## Locked decisions

- **Language**: Rust only (brain dump says "and/or"; zig isn't installed, one toolchain is the
  minimal-resource + maintainable choice). Edition 2024, workspace like phosphor's.
- **UI**: egui/eframe 0.33 (wgpu backend; glow kept feature-switchable if RSS receipts disappoint).
  Same information priority & layout as v1 (GPU → Memory → CPU → Network → Disks, narrow vertical
  card stack, ~430×780 default), restyled to Ben's house design language: sharp corners, 1px
  hairline frames, monospace data, dimensional/carved important controls, flat lower tiers.
- **Themes** (Palette table, phosphor theme.rs pattern): `blossom_dark` (wine-plum house tokens —
  fresh-install default), `blossom` (petal-paper light), `light`, `dark`, `amoled` ("Blossom
  AMOLED" — v1's true-black/pink/gold look Ben actively uses), `funky` (bubblegum pop-art, kept),
  plus a `system` mode that reads the Cinnamon GTK theme name via gsettings and maps to the nearest
  palette family (Blossom→blossom family, "*dark*"→dark). Migration: Ben's existing
  `~/.config/sysmon/settings.json` has `theme_mode:"blossom"` (the AMOLED look) → maps to `amoled`
  so his daily look survives the upgrade. 7 graph palettes carried over verbatim from v1
  (mint/aqua/sunset/forest/mono/blossom/funky, light+dark variants each).
- **Per-process network**: native, zero-setup primary = netlink `sock_diag` (INET_DIAG_INFO →
  per-TCP-socket `bytes_acked`/`bytes_received` deltas; kernel ≥4.1, no privileges for own
  sockets) + socket-inode→PID map from `/proc/*/fd`. UDP/QUIC + other-user coverage: merge nethogs
  trace rates when usable (Ben's machine already has nethogs with cap_net_admin,cap_net_raw — v1
  pattern kept). Source is disclosed in UI/API (`"source":"nethogs"|"tcp_diag"|"none"` + fix hint).
  Per-process connection table (proto, laddr→raddr, state, per-socket rate) always available for
  own processes — this is the "click → what's using what" story for the future bar.
- **Old Python tree**: deleted in the rewrite (phosphor 4.0.0 purge precedent). Tag the current
  commit `v1.0.0` first so history stays findable. `dist/sysmon_1.0.0_all.deb` leaves git
  (packaging/dist gitignored; release assets belong on GitHub releases).
- **Versioning**: 2.0.0 everywhere (workspace, `--version`, deb/rpm filenames, tag) — the version
  law.
- **License**: GPLv3 (unchanged). SPDX headers like phosphor.

## Architecture

```
sysmon/                          (repo root — README, LICENSE, HANDOFF.md, Cargo.toml, crates/,
                                  assets/, packaging/, docs/, tests fixtures live in crates)
  Cargo.toml                     workspace; [workspace.package] version = "2.0.0"
  crates/
    sysmon-core/                 lib: collectors + snapshot model + wire types. No UI deps.
      src/
        lib.rs, snapshot.rs      SystemSnapshot { ts, system, cpu, memory, gpu, network, disks,
                                 processes, sensors } — serde Serialize == the wire contract
        units.rs                 decimal/binary formatting (v1 formatting.py semantics)
        collect/
          mod.rs                 Sampler: owns per-collector delta state; sample() -> SystemSnapshot
          cpu.rs                 /proc/stat (per-core %, overall — delta-based like htop),
                                 /proc/loadavg, /sys/…/cpufreq (cur/min/max), uptime, task counts
          memory.rs              /proc/meminfo: total/used(total-available)/available/cached/
                                 buffers/dirty/swap (+zswap if present) — psutil-compatible
                                 definitions so numbers match v1
          gpu.rs                 amdgpu sysfs (busy %, VRAM used/total, GTT, edge+junction temp,
                                 power avg/cap, core+mem clocks, fan rpm) + per-process DRM fdinfo
                                 (engine-time deltas, max-across-engines; negative-cache like v1)
          disk.rs                /proc/self/mountinfo + statvfs (real block devices only, dedup by
                                 realpath) + /proc/diskstats deltas (read/write B/s, util%)
          net.rs                 /proc/net/dev per-interface counters + rates (virtual ifaces
                                 excluded but listed separately), interface addrs (getifaddrs)
          net_process.rs         sock_diag netlink client (hand-rolled ~250-line INET_DIAG
                                 request/parse; no heavy netlink dep) + inode→pid cache +
                                 nethogs -t merge (v1 reader logic ported)
          process.rs             /proc scan: stat (state, ppid, nice, threads, utime/stime deltas
                                 → CPU%, starttime), statm (RSS), status (uid→user via /etc/passwd
                                 cache), cmdline, io (read/write deltas; EACCES→None), comm
          sensors.rs             hwmon walk: k10temp (CPU Tctl/Tdie), nvme composite, others
                                 labeled; battery via /sys/class/power_supply when present
        apps.rs                  desktop-entry index: parse /usr/share/applications,
                                 ~/.local/share/applications, flatpak exports; map executable
                                 basename / StartupWMClass / desktop-id → icon name; freedesktop
                                 icon lookup honoring gsettings icon theme (Blossom →
                                 Papirus-Dark → Mint-Y → hicolor inherit chain), returns icon
                                 file path (png or svg)
      tests/
        fixtures/                frozen /proc & /sys captures from THIS machine
        parsers.rs               golden parser tests
        accuracy.rs              live cross-checks vs free -b / df -B1 / ps / direct /proc reads
                                 with tolerances (the accuracy-discrepancy suite)
    sysmon-app/                  bin `sysmon`: CLI + GUI + socket server
      src/
        main.rs                  subcommand dispatch (phosphor style, hand-rolled args):
                                 GUI default | probe | tap | ctl | schema | serve | --background |
                                 --version | help. Exit codes 0/2/3/4.
        agent.rs                 probe/tap/ctl/schema client side; envelope
                                 {"status","tool":"sysmon","version","ts",…}; errors carry "fix";
                                 JSON auto-on when piped, --json forces
        control.rs               Unix NDJSON server @ $XDG_RUNTIME_DIR/sysmon/ctl.sock
                                 (mirrors phosphor control.rs: shared StatusSnapshot + request
                                 queue + egui context wake). Verbs: status, snapshot(list of
                                 sections), subscribe [sections] [interval], ctl raise|quit|
                                 page <overview|processes>|theme <id>|palette <id>|interval <s>|
                                 pause|resume|popout <sect>|popin <sect>|shot [--path] (PNG via
                                 wgpu readback, deferred reply → result.path)
        serve.rs                 headless daemon: sampler + same socket, no window (for the bar
                                 when the GUI isn't up). Single socket owner; second instance
                                 exits 2 naming the owner pid + fix.
        gui/
          mod.rs, app.rs         eframe App: sampler thread → channel → request_repaint;
                                 pause, always-on-top, compact, section visibility, settings
                                 (v1-compatible settings.json — reads old keys, adds new)
          theme.rs               Palette table (7 entries above) + carved-stone widget helpers
                                 (phosphor carve pattern), semantic per-section colors
          cards/{gpu,memory,cpu,network,disks,sensors}.rs
                                 each card = one render fn reused by main page AND popout
                                 viewport; graphs + stat grid + top-3 (with app icons)
          graphs.rs              line/area/bars history graphs + per-core bars, hover readouts,
                                 sharp-corner hairline frames
          processes.rs           egui_extras Table: icon, name, PID, user, CPU%, memory, GPU%,
                                 VRAM, read/s, write/s, net ↓/s, net ↑/s, threads, state, nice,
                                 started, command; click-to-sort, filter box (Ctrl+F), context
                                 menu (End/Kill/Priority presets/Copy PID/Details) with pkexec
                                 fallback exactly like v1
          details.rs             per-process detail popout (click-to-dismiss): full cmdline, cwd,
                                 user, threads, started, io, gpu, memory + live connection list
                                 (proto laddr→raddr state rate) — the "what is this app doing on
                                 the network" drill-down
          popout.rs              section pop-outs as deferred viewports: pinned-on-top default
                                 with pin toggle, drag-anywhere (ViewportCommand::StartDrag),
                                 drag-onto-main-window-to-dock (outer-rect overlap + X11 pointer
                                 button check via x11rb; settle-timeout fallback on non-X11),
                                 popped-out set persisted across launches (v1 behavior)
          icons.rs               icon rasterizer: png via image, svg via resvg, texture cache
      tests/
        ui_kittest.rs            egui_kittest harness w/ injected synthetic snapshots: page
                                 switch, menu, theme switch, filter, sort, context menu, popout
                                 state; wgpu snapshot PNGs as receipts
  assets/sysmon.svg              kept (existing identity) + rendered 256px png for hicolor
  packaging/build-deb.sh         phosphor-style: cargo build --release, strip, staged tree
                                 (bin, .desktop, icons, scdoc manpage sysmon.1, copyright),
                                 minimal Depends, Recommends: nethogs; → packaging/dist/
  packaging/build-rpm.sh         cargo-generate-rpm with asset table in sysmon-app's metadata
  docs/API.md                    the agent/bar contract: every verb, envelope, exit codes, jq
                                 examples incl. `sysmon tap network` for the future applet
  docs/dev/                      this plan, receipts ledger, accuracy results (planning-era docs
                                 out of the public root)
```

**Efficiency contract** (why this hits "minimal resource usage"): adaptive sampling — the GUI
samples at the user interval (default 2s); `serve` samples only while a subscriber/one-shot is
connected (else sleeps on accept); probe with no daemon does a direct two-sample read (~200ms)
in-process. Collectors read only needed /proc files with reused buffers; fdinfo scans keep v1's
negative-cache; process scan is one pass. Receipts: own-CPU% and RSS measured over 60s idle,
compared against v1 (python) in the README.

## Implementation waves (each ends with its verify receipt)

1. **Scaffold + tag**: `git tag v1.0.0` on e9bac8a. Branch `v2-rust`. Workspace, two crates,
   SPDX headers, .gitignore (+packaging/dist, /target), rust-toolchain pin. `cargo build` green.
2. **sysmon-core collectors** (cpu, memory, disk, net, process, gpu, sensors, apps) + snapshot
   model + fixtures + golden tests. Verify: `cargo test -p sysmon-core`; spot-check live values
   against `free -b`, `df -B1`, `/proc/stat` by hand.
3. **Accuracy suite**: `tests/accuracy.rs` cross-checks (memory vs free, disks vs df, cpu vs
   /proc/stat recompute, RSS-of-self vs ps, net totals vs /proc/net/dev, gpu vs sysfs re-read,
   load vs /proc/loadavg) with justified tolerances. Fix every discrepancy found (that's the
   "full fixes" ask). Verify: suite green on live machine; results table → docs/dev/ACCURACY.md.
4. **sock_diag per-process network**: netlink client, inode→pid cache, nethogs merge, per-process
   ↓/↑ split. Verify: generate known traffic (curl a big file), assert sysmon attributes it to
   curl within tolerance vs nethogs' own figure.
5. **Agent surface**: envelope, probe (direct mode), schema, serve + control socket, tap
   subscribe, ctl verbs (except GUI-needing ones). Verify from a clean shell with jq: envelope
   shape, exit codes (0/2/3), `sysmon probe network --json | jq`, `sysmon tap network` streams,
   second `serve` exits 2 with fix.
6. **GUI**: theme table + chrome, five cards + sensors card, graphs, top-3 with icons, processes
   table + actions + details popout, settings load/save + v1 migration, pause/pin/compact/units/
   interval/sections, keyboard shortcuts. Verify: launch under Xvfb (background-gui skill),
   `sysmon ctl shot` PNGs of every page × every theme, eyeball against v1 screenshots for layout
   parity; kittest interaction tests green.
7. **Pop-outs + single instance + --background + ctl GUI verbs** (raise/page/theme/popout/shot).
   Verify: Xvfb e2e script — popout gpu via ctl, shot shows the viewport, xdotool drag-dock test,
   plain second launch forwards to running instance (exit 0, raises).
8. **Self-resource receipts**: idle CPU%/RSS of GUI + serve, vs v1 python app, 60s windows →
   README table. If wgpu RSS is embarrassing, flip eframe to glow and re-measure (both wired).
9. **Polish pass** (explicit ask): spacing/empty states ("no AMD GPU", "per-process net: TCP
   only — fix: …") in gentle-coding tone, 80–200ms eased reveals only, reduced-motion respect,
   focus-visible, --help + manpage + schema completeness, README rewrite with fresh screenshots
   from THIS build (every theme, processes page, popout, details), docs/API.md.
10. **Ship**: hygiene pass (python tree removed, root minimal), receipts-bearing commit
    narrative, `--no-ff` merge to main, tag v2.0.0, build deb+rpm+`git archive` tarball+
    SHA256SUMS, `gh release create --notes-file` (honest ledger: "Removed, honestly" — GTK-native
    theme inheritance replaced by palette system + system-mapping; anything else discovered),
    push, `gh repo edit` description refresh.
11. **Install on Ben's machine**: `sudoplz sudo apt install ./packaging/dist/sysmon_2.0.0_amd64.deb`
    (upgrades the 1.0.0 'all' package), verify `sysmon --version` == 2.0.0, launch GUI once on
    the real display briefly (or --background if Ben is active — background-gui skill), probe
    answers, desktop entry present. Update memory (project state, decisions) + HANDOFF.md.

## Verification (end-to-end definition of done)

- `cargo test` (all crates) green including accuracy suite on the live machine.
- Agent contract receipts: probe/tap/ctl/schema transcripts with jq, exit codes checked.
- Screenshot receipts of every page/theme/popout from the shipped build (in README/docs).
- Traffic-attribution receipt (curl test) and self-resource table (v2 vs v1).
- `sysmon --version`, deb filename, rpm filename, tag all read 2.0.0; install verified on the
  machine; release live with 4 assets; every README command was actually run verbatim.

## Out of scope (fenced)

- The Cinnamon desktop-bar applet itself ("later" — the API + docs/API.md serve it).
- Non-AMD GPU support (NVIDIA/Intel) — honest "no AMD GPU" state + ledger issue, like v1.
- Wayland-specific popout drag-dock polish (Ben runs X11; non-X11 gets the settle-timeout path).
