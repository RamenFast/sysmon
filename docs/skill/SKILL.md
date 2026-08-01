---
name: sysmon
description: "Drive SysMon (Ben's Rust + egui system monitor, a station node) without pixels — one-shot machine state for CPU/GPU/memory/network/disks/sensors/processes/connections (probe), NDJSON streams for a desktop bar (tap), drive the running window (ctl: page/theme/palette/popout/shot/interval), run it headless (serve) or on a private Xvfb (--background). Use whenever a task needs live system numbers on this machine, per-process network attribution, a screenshot of the monitor, or control of the running instance. Carries the isolation law that has bitten before (XDG_RUNTIME_DIR *and* XDG_CONFIG_HOME) and the honest limits of each per-process data source."
---

> **Submits to [[ben-context-standards]].** That skill is the authority on Ben's coding
> spirit and on every word that enters a model's attention. Read it first. Where this
> document and that one disagree, that one wins and this one is the defect to fix.

# Driving SysMon

SysMon ≥ 3.0 (`/usr/bin/sysmon`) is fully agent-drivable: one binary,
JSON everywhere, errors that carry the way out, no pixels needed. The
authoritative machine map is **`sysmon schema`** — always generated
from the binary, never hand-maintained. The worked repo guide is
`docs/AGENTS.md` in `~/Dev/ClaudeWorkspace/sysmon`.

## ⛔ The isolation law (this one has already bitten)

Any test, experiment, or throwaway instance **must** set *both*:

```bash
export XDG_RUNTIME_DIR="$(mktemp -d)"   # the control socket
export XDG_CONFIG_HOME="$(mktemp -d)"   # the saved settings
```

Set only the first and you will drive Ben's real window. Set neither
and `ctl theme …` **silently rewrites his saved settings** — that
exact accident happened once already. `scripts/conformance.sh` and
`scripts/e2e.sh` both do this by construction; copy them, don't
improvise.

## The contract

Every one-shot emits one envelope:

```json
{"status":"ok","tool":"sysmon","version":"3.0.0","ts":"2026-08-01T22:14:07+00:00","result":{…}}
{"status":"error",…,"error":"…","fix":"…","exit":3}
```

- `ts` is **ISO-8601 with a UTC offset**; `ts_epoch` rides along for arithmetic.
- Errors **always** carry a `fix`, and an `exit` naming the code the failure means.
- Exit codes: `0` ok · `2` unavailable (nothing running that could answer) ·
  `3` bad arguments · `4` runtime failure.
- JSON is automatic when stdout is a pipe; `--json` forces it on a TTY.
- Streams are NDJSON, no envelope per line, every line carrying `event: "snapshot"`.
- Every rate spans its snapshot's `interval_seconds`. **A fresh sampler's first
  snapshot has zero rates** — that first line is the delta baseline, not a bug.

## Read state

```bash
sysmon probe                        # everything, prose on a TTY
sysmon probe cpu memory             # several sections, one snapshot
sysmon probe network --json | jq .result.network.top_processes
sysmon probe connections            # the full socket table
```

Sections: `all system cpu memory gpu network disks processes sensors
connections`.

Works with or without a running instance. `result.via` says which:
`socket` (the live GUI/serve answered — longer, smoother window) or
`direct` (sampled in-process, twice, 250 ms apart, so rates are real).

## Stream

```bash
sysmon tap network --interval 2 | jq --unbuffered -c \
  '{down: .network.download_bps, up: .network.upload_bps}'
sysmon tap cpu -i 1
```

The desktop-bar diet. Each subscriber picks its own cadence and every
line's rates are correct for its own window. Rides the live instance's
socket when one exists, samples locally otherwise — the consumer code
is identical either way.

## Drive the window

```bash
sysmon ctl status              # who owns the socket: mode gui|serve, pid, version, renderer
sysmon ctl page processes      # overview | processes
sysmon ctl theme blossom_dark  # system blossom_dark blossom amoled light dark
                               #   funky paper basalt amber chromacore greyscale
                               # greyscale is the a11y floor; `system` picks it
                               # automatically under desktop high contrast
sysmon ctl palette sunset      # mint aqua sunset forest mono blossom funky amber terminal
sysmon ctl popout gpu          # gpu memory cpu network disks sensors
sysmon ctl popin gpu
sysmon ctl compact on          # on|off
sysmon ctl units binary        # decimal|binary
sysmon ctl interval 2          # seconds, 0.2–60
sysmon ctl pause / resume / raise / quit
sysmon ctl shot [/path.png]    # BLOCKS until the PNG exists; path in result.path
```

`shot` uses a deferred reply — the file exists by the time you can read
`result.path` (default under `$XDG_RUNTIME_DIR/sysmon/shots/`). It
captures the **main viewport only**; pop-outs are separate OS windows.

Against `sysmon serve`, only `status snapshot subscribe quit` work.
Everything else answers exit **2** with a fix — including `pause`,
`resume` and `interval`, because serve samples on demand (drive
cadence with `tap --interval N` instead). A bad enum value is exit
**3**; a bad `--interval` is refused rather than clamped.

## Headless / background

```bash
sysmon serve          # headless daemon; idle = zero sampling, ~6 MB
sysmon --background & # full GUI on a private Xvfb — no window on Ben's
                      # screen, socket + ctl + shot all live. Game-safe.
```

Single-owner socket at `$XDG_RUNTIME_DIR/sysmon/ctl.sock`. A plain
`sysmon` launch while a same-version GUI runs raises it and exits 0; an
*older* running GUI is asked to quit so a relaunch always shows the
installed version; a launching GUI asks a running `serve` to hand over;
a second `serve` exits 2 naming the owner.

Raw socket, no CLI:

```bash
# nc ships with the base system; socat is a fine substitute if you have it
echo '{"verb":"snapshot","sections":["network"]}' | nc -U $XDG_RUNTIME_DIR/sysmon/ctl.sock
# {"verb":"subscribe","sections":["network"],"interval":2} upgrades to an NDJSON stream
```

## Per-process network, honestly

`network.process_source` names what fed the figures, and the layers do
not compare:

| source | coverage | needs |
|---|---|---|
| `nethogs` | packet truth: all protocols (UDP/QUIC), all users | `nethogs` with `cap_net_admin,cap_net_raw` |
| `tcp_diag` | TCP socket byte counters, **your own** processes | nothing |
| `none` | — | `process_source_hint` names the upgrade |

Unresolved sockets show `pid: -1` (that's /proc permissions, not a bug).

## Gotchas (paid for, so you don't have to)

- `ctl` against nothing running exits 2 and the fix names your options;
  `probe`/`tap` never need a daemon.
- `probe connections` through a live GUI flips a sampling flag and waits
  up to ~2× the update interval — budget a couple of seconds on first ask.
- The GUI holds no inet sockets itself unless serving; `connections`
  covers inet (tcp/udp) only, not unix sockets.
- `--background` re-execs under `xvfb-run` (`SYSMON_BACKGROUND` guards
  recursion) and needs the `xvfb` package.
- Drive temperatures need `drivetemp` (`modprobe drivetemp`); the card
  says so in-window when they're missing.

## Changing sysmon's code

`scripts/conformance.sh` is the standard made executable — thirteen
numbered checks against a real binary. **Run it before and after any
change**, and never let the number go down:

```bash
scripts/conformance.sh target/release/sysmon        # ledger on a TTY
scripts/conformance.sh target/release/sysmon --json # one envelope
```

Then `cargo test --release` (unit + kittest UI + live accuracy) and
`scripts/e2e.sh` (private Xvfb: every theme and page, pop-out
drag-dock, single-instance forward). `docs/SERIOUS-TODOS.md` is the
honest-uncertainty ledger and `FEEDBACK.md` is the session ledger Ben
mines — append, never rewrite.
