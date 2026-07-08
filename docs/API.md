# The SysMon API

Everything the window knows, one command away. Three doors, one
contract; `sysmon schema` is the always-current machine map of all of
it.

## The contract

Every one-shot reply is a single JSON envelope:

```json
{"status":"ok","tool":"sysmon","version":"2.0.0","ts":1751900000.0,"result":{…}}
{"status":"error","tool":"sysmon","version":"2.0.0","ts":…,"error":"…","fix":"…"}
```

- errors **always** carry a `fix` — the way out, not just the wall
- exit codes: `0` ok · `2` unavailable (nothing running that could
  answer) · `3` bad arguments · `4` runtime failure
- output is JSON automatically when stdout is piped; `--json` forces
  it on a terminal
- every rate spans the snapshot's `interval_seconds`, reported per
  snapshot; a fresh sampler's first snapshot has zero rates

## Door 1 — `sysmon probe` (one-shot)

```bash
sysmon probe network --json | jq .result.network.download_bps
sysmon probe cpu memory        # multiple sections in one snapshot
sysmon probe connections       # the full socket table
```

Works with or without anything running: a live instance answers over
its socket (`result.via == "socket"`); otherwise probe samples
in-process, twice, 250 ms apart, so rates are real
(`result.via == "direct"`).

Sections: `all system cpu memory gpu network disks processes sensors
connections`.

## Door 2 — `sysmon tap` (stream)

One snapshot per line, NDJSON, at your cadence — the desktop-bar
diet:

```bash
sysmon tap network --interval 2 | jq --unbuffered -c \
  '{down: .network.download_bps, up: .network.upload_bps}'
```

With a live instance the stream rides its socket; without one, tap
samples locally. Multiple subscribers each pick their own cadence;
every line's rates are correct for its own window.

## Door 3 — `sysmon ctl` (drive the window)

```bash
sysmon ctl status            # who's serving, which mode
sysmon ctl page processes    # switch pages
sysmon ctl theme blossom_dark
sysmon ctl popout gpu        # pop the GPU card into its own window
sysmon ctl shot /tmp/s.png   # PNG of the window, path in the reply
sysmon ctl quit
```

`raise page theme palette popout popin shot compact units` need the
GUI; `status snapshot subscribe pause resume interval quit` work on
either the GUI or `sysmon serve`. With nothing running, ctl exits 2
and the fix names your options.

A GUI's `status` also names the adapter the window renders on
(`renderer`); if it had to fall back to a CPU rasterizer (no usable
Vulkan driver), `renderer_hint` appears and names the fix — degraded
modes are disclosed, never silent.

## The socket

`$XDG_RUNTIME_DIR/sysmon/ctl.sock` — one JSON object per line in,
one envelope per line out:

```bash
echo '{"verb":"snapshot","sections":["network"]}' | socat - UNIX:$XDG_RUNTIME_DIR/sysmon/ctl.sock
```

`{"verb":"subscribe","sections":["network"],"interval":2}` upgrades
the connection to a raw NDJSON snapshot stream. Single owner: the GUI
or `serve`, whoever binds first; a launching GUI asks a running serve
to hand over.

## Per-process network, honestly

`network.process_source` names what fed the per-process figures:

| source | coverage | needs |
|---|---|---|
| `nethogs` | packet truth: all protocols (UDP/QUIC), all users | `nethogs` with `cap_net_admin,cap_net_raw` |
| `tcp_diag` | TCP socket byte counters, your own processes | nothing — netlink sock_diag, zero setup |
| `none` | (hint says how to fix) | — |

`network.top_processes` answers "who is using the network" without a
process scan; `probe connections` lists every socket (proto,
addresses, state, per-socket TCP rates), pid-resolved where /proc
allows. Loopback chatter is excluded from rates, visible in the
table.

## For the future Cinnamon bar

Poll: `sysmon tap network --interval 2` (or keep `sysmon serve`
running — it costs nothing idle). On click:
`sysmon probe network --json | jq .result.top_processes` and
`sysmon probe connections` for the drill-down. `sysmon` (the GUI)
raises itself if launched again — one command for "open the full
monitor".
