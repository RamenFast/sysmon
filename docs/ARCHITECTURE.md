# Architecture

The developer map. File references use `path :: symbol` (symbols
survive refactors better than line numbers; `grep -n "fn symbol"`
finds the line). Assume zero context: everything you need to hold in
your head is written here.

## The one-sentence version

A `Sampler` turns /proc and /sys into a serde `SystemSnapshot`;
everything else — the egui window, the CLI, the control socket — is
a client of that snapshot.

## Crates

```
crates/sysmon-core     the engine (no UI dependencies, no egui)
crates/sysmon-app      lib `sysmon_app` + bin `sysmon`
```

`sysmon-app` is a lib + thin `main.rs` so integration tests can
reach every module (`lib.rs :: run_cli` is the whole dispatch).

## sysmon-core

```
src/snapshot.rs        SystemSnapshot and every section struct — THE WIRE
                       CONTRACT. serde field names carry units (_bytes,
                       _bps, _percent…). `Wants` picks sections per sample;
                       Wants::from_section_name maps CLI section names and
                       encodes dependencies (cpu⇒sensors for the package
                       temp, processes⇒gpu for the GPU% column,
                       network⇒per_process_net).
src/units.rs           decimal/binary human formatting (v1 semantics)
src/apps.rs            AppIndex: .desktop entries indexed by exe basename /
                       comm / StartupWMClass / desktop-id → Icon name, plus
                       a freedesktop icon-theme resolver that walks the real
                       Inherits chain. Returns paths; rasterizing is the
                       GUI's job.
src/collect/mod.rs     Sampler: owns every collector + sample(Wants).
                       SamplerOptions{enable_nethogs} — one-shots pass
                       false (never leave child processes behind).
src/collect/read.rs    read helpers + SelfInterval (see Invariants)
src/collect/cpu.rs     /proc/stat deltas (busy = total − idle − iowait,
                       guest excluded from total), loadavg, cpufreq sysfs
src/collect/memory.rs  /proc/meminfo; used = MemTotal − MemAvailable
src/collect/disk.rs    mountinfo + statvfs + /proc/diskstats deltas (rates,
                       util%); partition borrows parent-disk counters
src/collect/net.rs     /proc/net/dev deltas; physical = has a device symlink
                       in /sys/class/net (never name prefixes); getifaddrs
src/collect/net_process.rs  per-process attribution — see below
src/collect/process.rs one pass over /proc/<pid>: stat, cmdline, io, exe.
                       Delta state keyed by (pid, starttime) → pid reuse
                       can't fake CPU%. uid from dir metadata; /etc/passwd
                       cache with mtime refresh.
src/collect/gpu.rs     amdgpu sysfs card metrics + per-process DRM fdinfo
                       (engine-time deltas, max across engines per client,
                       negative cache so non-GPU pids are re-checked every
                       5th sample)
src/collect/sensors.rs hwmon walk + /sys/class/power_supply battery
tests/                 parsers (fixtures), accuracy (live cross-checks),
                       live_smoke, live_icons, live_net_attribution
```

### Per-process network (`collect/net_process.rs`)

Two sources, merged, always disclosed:

1. **tcp_diag** — a hand-rolled netlink `sock_diag` client (~250
   lines, no netlink crate). Sends `SOCK_DIAG_BY_FAMILY` dumps for
   tcp/tcp6 (+udp/udp6 when the connection table is wanted) with the
   `INET_DIAG_INFO` ext bit; parses `inet_diag_msg` + rtattrs; reads
   `tcpi_bytes_acked`/`tcpi_bytes_received` at byte offsets 120/128
   of `tcp_info` (offsets asserted by a unit test against the uapi
   layout: 8 u8-ish bytes + 24 u32 + 2 u64 pacing rates). Deltas
   keyed by socket **cookie**. Socket inode → pid via
   `InodeResolver`: two passes over `/proc/<pid>/fd` (known
   socket-owners first, full sweep only for stragglers).
2. **nethogs merge** — when nethogs exists with
   `cap_net_admin,cap_net_raw` (checked via getcap), a `nethogs -t`
   child is kept by *long-lived* samplers; its per-refresh rates
   replace tcp_diag's as `rates_by_pid` (packet truth, UDP/QUIC, all
   users). Child death ⇒ fall back + hint.

Loopback-remote sockets are excluded from **rates** (matching the
physical-interfaces-only headline) but stay in `connections`.

### Invariants

- **Every rate-producing collector owns a `SelfInterval`**
  (`collect/read.rs`): its denominators span *its own* previous
  collection. Two socket clients sampling different sections
  therefore can't corrupt each other's rates. Never pass a shared
  interval into a collector.
- Collectors never panic on unreadable files — absence is a normal
  procfs Tuesday; fields go `None`/default and the section survives.
- `boot_ts` comes from /proc/stat `btime` (stable), not
  now−uptime (drifts).

## sysmon-app

```
src/lib.rs             run_cli(): the whole command dispatch
src/main.rs            one line over run_cli + the --background re-exec
                       (xvfb-run, SYSMON_BACKGROUND env guard)
src/envelope.rs        the reply envelope + exit codes + json_wanted()
src/agent.rs           probe/tap/ctl/schema client side + human probe render
src/control.rs         the socket server: Backend trait, bind/negotiate,
                       per-connection NDJSON loop, verb dispatch,
                       subscribe streaming. socket_path() =
                       $XDG_RUNTIME_DIR/sysmon/ctl.sock
src/serve.rs           ServeBackend: Mutex<Sampler>, samples ON DEMAND —
                       zero clients = zero work. pause/interval verbs
                       answer honest errors (nothing to pause).
src/gui/mod.rs         run(): settings → SharedUi → GuiBackend → socket
                       negotiation (raise a gui / take over a serve) →
                       eframe::run_native
src/gui/app.rs         SysMonApp (eframe::App) — the main-thread owner of
                       ALL UI state. SharedUi (Arc): latest snapshot,
                       histories, paused, interval, connections_wanted,
                       open_viewports, main_window_rect. spawn_sampler():
                       the sampling thread. process_commands(): drains ctl
                       verbs. collect_screenshot(): ViewportCommand::
                       Screenshot round-trip. popout_viewports(): immediate
                       viewports + drag-dock. x11_button1_down(): x11rb
                       pointer poll.
src/gui/backend.rs     GuiBackend (Backend impl): reads answer from
                       SharedUi without waking the window; UI verbs queue
                       GuiCommand{verb,value,path,reply} + request_repaint;
                       shot waits ≤10 s on the reply channel.
src/gui/theme.rs       Palette table (10) + system-theme mapping via
                       gsettings + apply() (sharp corners, tokens→egui
                       style) + GraphPalette table (9) + companions
src/gui/settings.rs    v1-compatible settings.json (era sniffed by absent
                       settings_version; v1 "blossom" → "amoled")
src/gui/cards.rs       the six overview cards + card chrome (glyph_button
                       hand-painted icons + stone carve, stat_grid,
                       top_process_row, process_context_menu) + AppAction
src/gui/graphs.rs      History ring buffers + line/area/bars painters +
                       per-core bars + level_bar (fills = per-segment
                       trapezoids; one concave polygon renders as stripes)
src/gui/processes.rs   the table: filter, stable sort, 16 columns,
                       virtualized rows
src/gui/details.rs     per-process viewport incl. live connections
src/gui/icons.rs       IconCache: AppIndex path → png (image crate) / svg
                       (resvg) → egui texture; letter-tile fallback;
                       kernel threads stay bare
src/gui/actions.rs     end/kill/renice with the pkexec ladder + confirm
                       modal + toasts
tests/ui_kittest.rs    the AccessKit interaction suite
```

### Threading model

```
main thread            eframe event loop; ALL UI state mutation; drains
                       GuiCommand queue; immediate pop-out/detail viewports
sysmon-sampler         loops at the user interval; Sampler::sample →
                       SharedUi.latest/histories; ctx.request_repaint() +
                       request_repaint_of(each open viewport)
sysmon-ctl-accept      control.rs accept loop → per-connection threads
sysmon-ctl-conn (×N)   read verb line → dispatch → write envelope;
                       subscribe loops sample-write-sleep
nethogs-reader         parses `nethogs -t` output into the latest rate map
pkexec runners         one-shot threads for privileged fallbacks
```

Rules: socket threads never touch UI state directly — UI verbs go
through the `GuiCommand` mpsc + one-shot reply channel, drained by
`process_commands` on the main thread. Reads (`status`, `snapshot`,
`subscribe`) answer straight from `SharedUi` and never wake the
window. Pop-outs render as **immediate viewports** but keep updating
while the main window is minimized because the sampler thread
requests repaints of each open viewport id.

### The socket, end to end

```
sysmon ctl theme funky
  agent.rs::run_ctl        builds {"verb":"theme","value":"funky"}
  control.rs::request      connect + one line each way
  control.rs::dispatch     "theme" → backend.gui_verb
  backend.rs::gui_verb     queue GuiCommand + reply_rx.recv_timeout(5s)
  app.rs::process_commands (main thread, next frame) applies + saves
                           settings, sends Ok back through the channel
  control.rs               wraps in envelope, writes the line
```

`shot` is the same, except process_commands stashes the reply sender
in `pending_screenshot`, sends `ViewportCommand::Screenshot`, and
`collect_screenshot` answers when the `egui::Event::Screenshot`
frame arrives (PNG encoded via the image crate).

### Single-owner socket negotiation (`gui/mod.rs :: negotiate_socket`)

bind → if `AlreadyRunning{mode:"gui"}` → send `raise`, print, exit
0. If `mode:"serve"` → send `quit`, wait 400 ms, retry once. Io
error → run the GUI **without** a socket, saying so (degraded, never
fatal).

## Design system

Hard rules live in `gui/theme.rs` header and CLAUDE.md. Palettes are
data (ten token blocks); `apply()` maps tokens onto egui's style
with `CornerRadius::ZERO` everywhere. `title`/`value` are the two
signature text colors (v1's pink titles / gold numbers). The stone
triple (`stone`, `stone_hi`, `stone_lo`) is worn only by
`glyph_button` — depth encodes importance, shape never changes.
Graph palettes: v1's seven verbatim plus amber and terminal, all with
light+dark variants;
`companion_graph_palette` re-creates v1's theme-brings-its-palette
behavior.

## Packaging

`packaging/build-deb.sh` (staged tree, stripped binary, scdoc
manpage, minimal Depends) and `build-rpm.sh` (cargo-generate-rpm;
asset table + `name = "sysmon"` override in
`crates/sysmon-app/Cargo.toml`). Both read the version from the
built binary so filenames can't drift. Output: `packaging/dist/`
(gitignored — assets belong on the GitHub release).
