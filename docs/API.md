# The SysMon API

Everything the window knows, one command away. Five doors, one
contract; `sysmon schema` is the always-current machine map of all of
it.

## The contract

Every one-shot reply is a single JSON envelope:

```json
{"status":"ok","tool":"sysmon","version":"3.0.3","ts":"2026-08-01T22:14:07+00:00","ts_epoch":1785622447.0,"result":{…}}
{"status":"error","tool":"sysmon","version":"3.0.3","ts":…,"error":"…","fix":"…","exit":3}
```

- `ts` is ISO-8601 with a UTC offset; `ts_epoch` is the same instant
  as a number, for arithmetic
- errors **always** carry a `fix` — the way out, not just the wall —
  and an `exit` naming its failure code
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

One snapshot per line, NDJSON, at your cadence, every line carrying
`event: "snapshot"` — the desktop-bar diet:

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
sysmon ctl page processes    # switch pages: overview | processes
sysmon ctl cpugraph threads  # the Overview's CPU card: total | threads | grid
sysmon ctl theme blossom_dark
sysmon ctl popout gpu        # pop the GPU card into its own window
sysmon ctl shot /tmp/s.png   # PNG of the window, path in the reply
sysmon ctl quit
```

`status snapshot subscribe quit` work against either the GUI or
`sysmon serve`. Everything else — `raise page theme palette popout
popin shot compact units`, and also `pause resume interval` — needs
the GUI: `serve` samples on demand, so it has nothing to pause and no
fixed interval to set (drive cadence from the client instead, with
`tap --interval N`). Against `serve` those verbs exit 2 and say so;
with nothing running at all, ctl exits 2 and the fix names your
options. `sysmon schema` is authoritative and generated from the binary.

A GUI's `status` also names its render adapter
(`renderer`); if it had to fall back to a CPU rasterizer (no usable
Vulkan driver), `renderer_hint` appears and names the fix — degraded
modes are disclosed, never silent.

## Door 4 — `sysmon specs` (what the machine is)

```bash
sysmon specs                 # the shareable spec sheet, plain text
sysmon specs graphics        # one section: system cpu memory graphics storage network audio
sysmon specs --json | jq -j .result.text   # byte-for-byte what the top bar's copy button copies
```

`probe` answers *how busy*; `specs` answers *which hardware, which
firmware, which driver stack*: CPU topology, base and max clocks,
caches and instruction sets; every memory module with maker, part
number and rank; each GPU with its architecture, compute units, the
PCIe link it actually gets (the slowest hop to the CPU, beside what
the card supports), power cap, Vulkan/OpenGL driver and compute stack;
the displays at their running mode; every physical drive, mounted or
not, with its link and filesystems; network interfaces; the audio
server and devices.

It is read on demand, needs no running instance, and costs ~0.1–0.2 s.
sysfs and udev come first; a few optional helpers (`vulkaninfo`,
`glxinfo`, `swaymsg`, `pactl`) fill in what sysfs can't say, each
under a 3 s deadline. A missing helper leaves its line out. A whole
section that can't be read is listed in `result.unavailable` with the
reason, and the sheet prints it as `Missing`.

The JSON carries everything, including the hostname and mount points,
for local scripts. `result.text` is the sheet meant for sharing: it
leaves out the hostname, serials, addresses and private mount paths (a
drive says what it holds, such as system, home, EFI or data, not where
it is mounted).

The top bar's copy button copies the whole sheet. Each section's copy
button (beside a pop-out's pin, and in a docked card's header) copies
that section's specs and then its usage over the range the pop-out's
slider picks: `sysmon history <section> --specs` prints the same text.

## Door 5 — `sysmon history` (what the machine did)

```bash
sysmon history gpu --since 2h            # GPU over the last 2 h: min/avg/p95/max, timeline, programs
sysmon history --since 7d --json | jq '.result.metrics[] | {key, max}'
sysmon history gpu --since 1y            # as far back as it goes: the archive reads like the rest
sysmon history gpu --specs --json | jq -j .result.text   # byte-for-byte a GPU pop-out's copy
sysmon history gpu --since 30m --records | jq -c '{ts, busy: .gpu.busy_percent}'
sysmon history --info                    # size per tier, how far back, growth per month and year
sysmon history --export ~/backup/        # the whole history as one .tar.zst
sysmon history --clear --yes             # delete it all (refused without --yes)
```

`probe` is now; `history` is the past. SysMon records one line per
sample while its window runs (Display options → Usage history), or
while `sysmon serve --record` runs headless: CPU (busy, kernel, iowait,
busy clock, temperature, load), memory (used, cache, swap, commit), GPU
(busy, VRAM, GTT, power, clocks, temperatures, fan, and the programs
holding it), network rates, per-disk rates and busy share, and every
plausible temperature and fan.

Per reading the summary gives min, avg, p95, max, the latest value, and
a timeline (`--blocks`, default 48) whose empty blocks are `null`:
SysMon was not recording then, and no gap is ever filled with a zero.
`covered_seconds` below `range_seconds` says the same for the range as
a whole. Rates add `total_bytes`, the bytes moved over the range. min,
avg and max are exact; p95 comes from a log-bucket sketch, within 0.5%
of the exact value. The GPU summary also lists every program that held
the card: peak, p95 and average VRAM, average and peak busy share, and
how much of the range it was present for. That answers "how much did
the game use while the model stayed loaded".

Storage (3.5): two tiers under `$XDG_STATE_HOME/sysmon/history/`
(default `~/.local/state/sysmon/history/`), read as one.

- **Hourly**: one file per UTC hour, gzipped when the hour ends, plus a
  small `<hour>.rollup.zst` (the hour condensed, see below). Hours
  stay hourly for 7 days (☰ → Usage history → *hourly for*).
- **Archive**: once a whole UTC day is older than that, its hours are
  repacked, losslessly, into `archive/<day>.jsonl.zst`: the same JSON
  lines, oldest first, one zstd frame per hour at level 19, an index
  and the hours' rollups in zstd skippable frames. `zstd -dc
  archive/2026-10-03.jsonl.zst` prints the day's lines with no SysMon
  needed. The day file is decoded back and checked line for line
  before any hourly file is removed.

**Nothing is deleted by age or size by default.** *keep* (forever, 1 y,
90 d) and *size cap* (none, 1, 4, 16 GB) are opt-in; past 1 GB with no
cap `status.history.warning` (and the menu) says so instead.

Measured on this machine's real recording (24 temperatures, 8 fans, 5
disks, 10 GPU programs; 2,781 samples over 4.6 hours, idle and
gaming): a raw line is 3.4 KB; the gzip-sealed hours averaged 141 B a
sample; the packed day file 103 B a sample (hour frames 94 B, rollups
8.4 B, index 0.3 B), 33× smaller than the raw lines and 27% smaller
than gzip. At the 5 s cadence Ben runs that is ~54 MB a month, ~650 MB
a year; at the 2 s default ~135 MB a month, ~1.6 GB a year. The hot
week adds ~18 MB at 5 s. `sysmon history --info`
prints the projection from the newest archived day (or, before one
exists, an estimate from the newest sealed hour).

Rollups make long ranges fast: each sealed hour's readings are kept
condensed (exact count, sum, min, max, latest, bytes moved, and the
p95 sketch, plus the GPU programs and the sample gaps), and a summary
merges an hour whole when it lies inside the range and inside one
timeline block. The numbers are identical to reading every record
(`--raw` does that, as a check; `rollup_hours` says how many hours were
merged). Rollups are derived data, ~4% of the archive, rebuilt from
the raw lines whenever one is missing or stale.

One process writes at a time, through a lock file that names it
(`result.recorder`); packing runs on that writer's own background
thread at nice 19. Readers need no lock and skip a torn last line,
counted in `skipped_lines`. `--clear` and `--pack` take the lock when
it is free; when a recorder holds it they ask it over the socket
(`{"verb":"history","op":"clear","yes":true}` or `"op":"pack"`), and it
acts between two samples. Clearing removes only the store's own files
(hourly files, rollups, archive days, its temp files); anything else
in the directory is left and counted, and nothing outside it is
touched. An export is laid out to restore with
`tar --zstd -xf FILE -C ~/.local/state/sysmon` (stop the recorder
first).

## Per-process GPU memory

`gpu.processes[].vram_bytes` is the VRAM a process holds now: for each
of its DRM clients (counted once, however many file descriptors point
at it), `drm-total-vram − amd-evicted-vram` from its fdinfo.
`drm-memory-vram` is never used where `drm-total-vram` exists: on
kernel 7.2 it drifted to 1.9 TB for sway and quickshell on a 32 GB
card (3.4.0 shipped that number). A buffer two processes share, such
as a window surface the compositor maps, counts in both and is reported
as `vram_shared_bytes`, so the sum over processes can exceed the card's
`vram_used_bytes` by the shared part. `vram_evicted_bytes` are buffers
that want VRAM but were moved to system RAM: the sign of VRAM pressure.

## The socket

`$XDG_RUNTIME_DIR/sysmon/ctl.sock` — one JSON object per line in,
one envelope per line out:

```bash
# nc ships with the base system; socat is a fine substitute if you have it
echo '{"verb":"snapshot","sections":["network"]}' | nc -U $XDG_RUNTIME_DIR/sysmon/ctl.sock
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
`sysmon probe network --json | jq .result.network.top_processes` and
`sysmon probe connections` for the drill-down. `sysmon` (the GUI)
raises itself if launched again — one command for "open the full
monitor".
