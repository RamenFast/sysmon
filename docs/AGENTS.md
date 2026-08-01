# Driving SysMon as an agent

One binary, JSON everywhere, errors that tell you the way out, no
pixels needed. This is the worked guide; the always-current machine
map is `sysmon schema` — strict, generated from the binary, and
tested against it (`cargo test -p sysmon-app --test contract` runs
every jq path it prints). (Developing on the codebase instead?
`ARCHITECTURE.md` and `dev/EXTENDING.md`; before and after any
change, `scripts/conformance.sh`.)

## The contract

- Every one-shot emits **one envelope**:
  `{"status":"ok|error","tool":"sysmon","version":…,"ts":…,"result"|"error"+"fix"+"exit"}`.
- `ts` is **ISO-8601 with a UTC offset** (`2026-08-01T22:14:07+00:00`);
  `ts_epoch` carries the same instant as a number, for arithmetic.
- Errors **always** carry a `fix` you can act on, and an `exit`
  naming the code that failure means — so a socket client classifies
  a failure exactly as the CLI does.
- Exit codes: `0` ok · `2` unavailable (nothing running that could
  answer) · `3` bad arguments · `4` runtime failure.
- Output auto-switches to JSON when stdout is a pipe; `--json`
  forces it on a TTY.
- `tap` and `subscribe` are NDJSON: one raw snapshot per line (no
  envelope around stream lines), **every line carrying
  `event: "snapshot"`**.
- Every rate spans its snapshot's `interval_seconds`. A fresh
  sampler's first snapshot has zero rates — that first line is not a
  bug, it's the delta baseline.

## Read state

```bash
sysmon probe                       # everything, human on a TTY
sysmon probe network --json | jq .result.network.top_processes
sysmon probe cpu memory            # multiple sections, one snapshot
sysmon probe connections           # the full socket table
```

Works with or without a running instance: `result.via` says
`"socket"` (answered by the live GUI/serve — smoother, longer
windows) or `"direct"` (sampled in-process, twice, 250 ms apart —
real instantaneous rates). Sections: `all system cpu memory gpu
network disks processes sensors connections`.

The field to trust for per-process network is
**`network.process_source`**: `nethogs` (packet truth, all
protocols, all users) · `tcp_diag` (kernel TCP counters, own-UID
sockets, zero setup) · `none` (and `process_source_hint` says how to
upgrade). Don't compare rates across sources in one breath — they
measure at different layers.

## Stream

```bash
sysmon tap network --interval 2 | jq --unbuffered -c \
  '{down: .network.download_bps, up: .network.upload_bps}'
sysmon tap cpu -i 1                # any section set works
```

The intended desktop-bar diet. Each subscriber picks its own
cadence; rates are correct per line (per-collector windows). The
stream rides the live instance's socket when one exists, else
samples locally — either way the consumer code is identical.

## Drive the window

```bash
sysmon ctl status                  # who owns the socket: mode gui|serve, pid, version
sysmon ctl page processes          # overview | processes
sysmon ctl theme blossom_dark      # system blossom_dark blossom amoled light dark
                                   #   funky paper basalt amber chromacore
sysmon ctl palette sunset          # graph colors: mint aqua sunset forest mono
                                   #   blossom funky amber terminal
sysmon ctl popout gpu              # gpu memory cpu network disks sensors
sysmon ctl popin gpu
sysmon ctl compact on              # on|off
sysmon ctl units binary            # decimal|binary
sysmon ctl interval 2              # seconds, 0.2–60
sysmon ctl pause / resume
sysmon ctl raise                   # focus + deiconify
sysmon ctl shot [/path.png]        # BLOCKS until the PNG exists; path in result.path
sysmon ctl quit
```

`shot` uses a deferred reply — the file exists by the time you can
read `result.path` (default lands under
`$XDG_RUNTIME_DIR/sysmon/shots/`). It captures the **main viewport
only**; pop-out windows are separate OS windows (screenshot the X
root if you need the composition).

## Headless / background

```bash
sysmon serve                       # headless daemon; idle = zero sampling, ~6 MB
sysmon --background &              # full GUI on a private Xvfb — no window on the
                                   # user's screen, socket + ctl verbs + shot all live
```

Single-owner socket at `$XDG_RUNTIME_DIR/sysmon/ctl.sock`: a plain
`sysmon` launch while a **same-version** GUI runs raises it and
exits 0; if the running GUI is **older** (upgrade day: deb installed
while the old window was up) the new binary asks it to quit and
takes over, so a relaunch always shows the installed version; a
launching GUI asks a running `serve` to hand over; a second `serve`
exits 2 naming the owner.

Raw socket, no CLI:

```bash
echo '{"verb":"snapshot","sections":["network"]}' | socat - UNIX:$XDG_RUNTIME_DIR/sysmon/ctl.sock
# {"verb":"subscribe","sections":["network"],"interval":2} upgrades to an NDJSON stream
```

## Gotchas (paid for, so you don't have to)

- **Isolate test instances**: set both `XDG_RUNTIME_DIR` (socket)
  and `XDG_CONFIG_HOME` (settings) or you'll drive — and *rewrite
  the saved settings of* — the user's real instance. `ctl theme` in
  a test once silently repainted the user's config.
- `ctl` against nothing running exits 2 (the fix names your
  options); `probe`/`tap` never need a daemon.
- `probe connections` through a live GUI flips a sampling flag and
  waits up to ~2× the update interval for the next tick — budget a
  couple of seconds on first ask.
- The GUI process holds no inet sockets itself unless serving —
  don't be surprised by an empty connection list for sysmon's own
  pid; `connections` covers inet (tcp/udp) only, not unix sockets.
- Per-process rates for *other users'* processes need the nethogs
  source; `tcp_diag` can only resolve your own pids (that's /proc
  permissions, not a bug). Unresolved sockets show `pid: -1`.
- `--background` re-execs under `xvfb-run`; the `SYSMON_BACKGROUND`
  env guard stops recursion. Needs the `xvfb` package.
