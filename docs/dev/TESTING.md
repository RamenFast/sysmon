# Testing SysMon

Five layers, cheapest first. The machine itself is the fixture for
the live layers — that's deliberate (a monitor that only passes on
synthetic data proves nothing).

| layer | command | needs |
|---|---|---|
| parser units + fixtures | `cargo test -p sysmon-core --lib` | nothing (pure) |
| settings migration | `cargo test -p sysmon-app --lib` | nothing |
| accuracy cross-checks | `cargo test -p sysmon-core --test accuracy` | a live Linux box |
| live probes (smoke, icons, net attribution) | `cargo test -p sysmon-core --test live_smoke --test live_icons --test live_net_attribution` | live box; net test wants WAN + skips offline |
| UI interactions | `cargo test -p sysmon-app --test ui_kittest` | GPU or lavapipe (wgpu) |
| full e2e | `scripts/e2e.sh out/ target/release/sysmon` | Xvfb, openbox, xdotool, imagemagick, jq |

`cargo test` runs everything but the e2e script. All 41 green is the
resting state.

## The accuracy suite (`crates/sysmon-core/tests/accuracy.rs`)

Each check compares a collector against an independent authority
(`free -b`, `df -B1`, `ps`, /proc//sys re-reads) under a justified
tolerance. **The law: a red check means fix the collector. Never
widen a tolerance to pass.** Identities and the tolerance table:
`docs/dev/ACCURACY.md`. The CPU check pins a spinner thread — brief
100%-of-one-core during the run is the test working.

## The kittest suite (`crates/sysmon-app/tests/ui_kittest.rs`)

Drives the real `SysMonApp` (real sampler, live /proc) through
AccessKit — clicks and typing land the way assistive tech would.
Patterns to keep:

- Construct with `Harness::builder().build_eframe(…)`, scratch
  `XDG_CONFIG_HOME` set first (the app saves settings on changes).
- Query by label (`get_by_label("Processes")`), by
  `Role::TextInput` for the filter, `get_by_label_contains` for
  count labels. Custom-painted widgets are findable only because
  `glyph_button` registers `WidgetInfo::labeled` — do the same for
  any new hand-painted control.
- Assert on `harness.state()` (the app struct) *and* on the UI
  where possible (e.g. panel_fill color after a theme switch).
- One `#[test]` fn, sequential — harnesses mutate process-global
  env; parallel harnesses race.

## The e2e script (`scripts/e2e.sh`)

One command, full live receipts: boots Xvfb + openbox, launches the
GUI with scratch config/socket, screenshots all 6 themes × 2 pages
through `ctl shot`, opens a pop-out and **docks it by drag-drop**,
checks single-instance raise, probe-via-socket, tap streaming, exit
codes, clean socket removal. Screenshots land in the output dir —
they're also how README images regenerate (fresh-from-this-build is
the law for release screenshots).

## ⚠ GUI-testing gotchas (each one cost a debugging loop)

- **Bare Xvfb has no window manager**: no keyboard-focus semantics
  (typed keys vanish — `xdotool windowfocus --sync` or run
  `openbox`), and windows cannot move at all.
- **Synthetic XTEST titlebar drags do not drive openbox's
  interactive move.** To emulate a user drag for dock testing:
  `xdotool mousedown 1` + step the window with `xdotool windowmove`
  + `mouseup 1` — the same signal triad (motion + button held +
  release position) the app's dock logic reads.
- **Never blind-click a row of a live-sorted process table** to test
  kill flows — rows re-sort between your coordinate lookup and the
  click. Spawn a sacrificial process (`sleep 3000 &`) and filter by
  its exact PID first; the confirm modal must name the sacrifice
  before you click confirm.
- **Scratch env or you drive the real thing**: `XDG_CONFIG_HOME`
  (settings writes!) and `XDG_RUNTIME_DIR` (socket ownership!) both.
  A ctl theme test once silently rewrote the user's saved theme.
- **`pkill -f <pattern>` kills your own test shell** when the
  pattern appears in its command text (exit 144, script dies
  mid-run). Match `readlink /proc/<pid>/exe` instead.
- **Xvfb renders via llvmpipe** (software): CPU figures measured
  there are the worst case, not the real-GPU truth. Label them; the
  resource table in RECEIPTS.md does.
- `ctl shot` captures the main viewport only; compose pop-outs with
  `import -window root` on the Xvfb display.

## Resource measurement recipe

```bash
# own CPU% over a window, no tools needed:
clk=$(getconf CLK_TCK)
t0=$(awk '{print $14+$15}' /proc/<pid>/stat); sleep 60
t1=$(awk '{print $14+$15}' /proc/<pid>/stat)
echo "$(( (t1-t0) ))" # ticks; % of one core = ticks/clk/60*100
awk '/VmRSS/{print $2}' /proc/<pid>/status   # RSS KiB
```

Let the GUI settle ~8 s first (icon index + first paints). Baseline
numbers to beat are in `docs/dev/RECEIPTS.md`.
