# The Performance page

*Spec for SysMon 3.2's startup page. Ratified by Ben 2026-10-01 (plan
approved in conversation). The code in `crates/sysmon-app/src/gui/performance.rs`
and the new pieces in `graphs.rs` are compiled from this file: when
they disagree, this file wins and the code is regenerated.*

## Lineage

The Windows XP Task Manager's Performance tab: an LED meter beside a
scrolling scope graph for each resource, etched group boxes of totals
below, a status bar along the bottom. Everything on one screen, no
scrolling to find a number. SysMon keeps that discipline and dresses
it in its own language: sharp corners, hairline frames, carved stone
for the few controls that matter, one colour per concept, monospace
numbers, a dark instrument field under every trace.

## Layout

Two breakpoints, decided by the central panel's width each frame.

### Narrow (< 760 px, the 430 px default window)

```
┌ CPU ──────────────────────── [combined│per thread] ┐
│ ▮▮▮▮▮▮▮▮░░ 4%  │ scope graph (2 lines)              │
├ Memory ───────────────────────────────────────────┤
│ ▮▮░░░░░░░░ 12% │ scope graph (used + cache)         │
├ GPU ──────────────────────────────────────────────┤
│ ▮░░░░░░░░░ 3%  │ scope graph (busy + VRAM)          │
├ Disk ─────────────────────────────────────────────┤
│ ▮░░░░░░░░░ 2%  │ scope graph (read + write)         │
├ Network ──────────────────────────────────────────┤
│ 1.2 MB/s ↓     │ scope graph (down + up)            │
├ Thermals ─────────────────────────────────────────┤
│ 160°F · 71°C   │ scope graph (CPU °C + GPU °C,      │
│                │  busy clock as a faint third)       │
├ Totals ───────────┬ Physical memory ───────────────┤
│ Processes    636  │ Installed        137.4 GB      │
│ Threads     3214  │ Usable           135.0 GB      │
│ Ctx/s      13106  │ Available        119.4 GB      │
│ Uptime  3d 04:12  │ Cache             17.9 GB      │
├ Commit charge ────┼ Kernel memory ─────────────────┤
│ Total     30.1 GB │ Slab               1.2 GB      │
│ Limit     67.5 GB │ Page tables      210.4 MB      │
│ Peak*     31.0 GB │ Stacks            48.6 MB      │
└───────────────────┴────────────────────────────────┘
 Processes 636 │ CPU 4% │ Commit 30.1 / 67.5 GB │ GPU 3% │ 160°F · 71°C
```

The page scrolls vertically only when the window is shorter than the
content (a 430×780 window shows everything at the default interval).

*Peak is the highest commit charge seen since SysMon started (an
in-process high-water mark, labelled "since launch" on hover). It is
not the kernel's figure, and the label says so.

### Wide (≥ 760 px)

Two columns of resource rows (CPU, Memory, GPU on the left; Disk,
Network, Thermals on the right), the four group boxes in one row of
four beneath, the status bar last. The per-thread CPU grid spans the
full width of its column.

### The CPU graph toggle

A two-position carved stone switch in the CPU box's title row:
`combined` | `per thread`. It is the one dimensional control on the
page, which is how the eye finds it.

- **combined:** one scope graph, two lines: total busy % (CPU colour)
  and kernel time % (its own colour, dashed in greyscale). The kernel
  line is drawn on top because it is always the smaller.
- **per thread:** a grid of mini scope graphs, one per thread in
  `/proc/stat` order (cpu0 top-left, row-major). Columns follow the
  width: `clamp(floor(width / 96), 2, 8)`. Each mini graph is at least
  28 px tall and carries its thread number in the top-left corner in
  the muted ink. The meter beside the grid still shows the total.
- The choice persists in `settings.cpu_graph_mode`. Until the user
  picks, it is `auto`: per thread when the column is ≥ 420 px wide,
  combined below that. Picking either side writes it down; the
  settings menu has a "Decide by width" row to return to `auto`.

## Pieces

All painted straight onto egui in `graphs.rs`. No new crates.

### Scope graph

- The **field**: `Palette::field()`, a dark surface on every theme.
  Dark themes: the plane darkened. Light themes: the ink (deep plum
  on Blossom, deep slate on Light). The field is always darker than
  every line drawn on it.
- A **graticule** of hairlines at 25/50/75 % plus vertical lines
  every 15 samples that scroll left as samples arrive, so time
  visibly passes. Drawn in the accent at low alpha. With reduced
  motion on, the vertical lines are fixed.
- Up to **two series**, each one `Shape::line`, newest at the right,
  150 samples wide (`HISTORY_LENGTH`), the same scale for both. The
  first series gets a soft fill under it (one trapezoid per segment,
  as today); the second is a bare line. In the greyscale theme the
  second series is dashed.
- **Scale**: a percentage graph is fixed 0..100. A rate graph
  autoscales to `observed_max × 1.15`, floored at `minimum_autoscale`,
  and the current ceiling is written in the top-right corner of the
  field in the muted ink (`↑ 48 MB/s`) so a flat line is never read as
  "nothing" when it is "small against a big spike".
- **Hover** reads out every series at the cursor's sample, through the
  same `hover_formatter` as today.

### LED meter

- A vertical bar of 20 segments, 3 px tall each with 1 px gaps, lit
  from the bottom in the concept colour; unlit segments are the
  field colour at low alpha. Width 18 px. The value is written under
  it in monospace (`4%`, `1.2 MB/s`). The meter's reading and the
  newest graph sample come from the same snapshot field, in the same
  frame, so they never disagree.
- The meter and its label are one click target: it opens the matching
  Overview card (`AppAction::ShowOverview`). The graph beside it opens
  Processes sorted by that resource (`AppAction::ShowProcessesBy`).
  Hover text says where each click goes.

### Etched group box

- A hairline frame in `palette.line`, with the title set into the top
  edge: the frame line breaks around the title text, which sits in
  the title colour. A 1 px highlight in `stone_hi` just inside the top
  and left edges and `stone_lo` inside the bottom and right edges,
  so the box reads as carved into the surface (depth encodes
  importance; the group boxes are the lower tier, so the carving is
  faint).
- Contents: label/value rows in the existing stat-grid style.

### Status bar

A bottom panel in `surface_2`, monospace 11 px, segments separated by
`│` in the muted ink. Its numbers are the same snapshot fields as the
boxes above it.

## Colours by concept

Stable across the page, pulled from the active graph palette's five
series plus two derived tints:

| concept | colour |
|---|---|
| CPU busy | `GRAPH_SERIES_CPU` |
| CPU kernel time | `GRAPH_SERIES_CPU` mixed 45 % toward the ink (a quieter sibling) |
| memory used | `GRAPH_SERIES_MEMORY` |
| memory cache | `GRAPH_SERIES_MEMORY` at 55 % alpha, stacked beneath used |
| GPU busy | `GRAPH_SERIES_GPU` |
| VRAM | `GRAPH_SERIES_GPU` mixed 45 % toward the ink |
| disk read | `GRAPH_SERIES_NET_DOWN` |
| disk write | `GRAPH_SERIES_NET_UP` |
| network down | `GRAPH_SERIES_NET_DOWN` |
| network up | `GRAPH_SERIES_NET_UP` |
| CPU temperature | `palette.value` (gold) |
| GPU temperature | `palette.title` (rose) |
| busy clock | `palette.muted`, faint, right-hand scale |

Every graph's legend names the colour and the series next to each
other in the box title row (`■ busy  ■ kernel`), so the meaning is
stated where the colour is used.

## Data

| shown | source | new in 3.2 |
|---|---|---|
| CPU busy % | `cpu.overall_percent` | |
| CPU kernel % | `cpu.kernel_percent` (system + irq + softirq ticks over the window) | yes, audited C7 |
| per thread % | `cpu.per_core_percent` | |
| memory used / cache | `memory.used_bytes`, `memory.cached_bytes` over `total_bytes` | |
| commit charge / limit | `memory.committed_bytes`, `memory.commit_limit_bytes` (Committed_AS, CommitLimit) | yes, audited M7 |
| kernel memory | `memory.slab_bytes`, `page_tables_bytes`, `kernel_stack_bytes` (Slab, PageTables, KernelStack) | yes, audited M8 |
| GPU busy / VRAM | `gpu.busy_percent`, `gpu.vram_used_bytes / vram_total_bytes` | |
| disk read / write | sum of `disks[].read_bps`, `write_bps` | |
| network | `network.download_bps`, `upload_bps` | |
| temperatures | `cpu.temperature_celsius`, `gpu.temperature_edge_celsius` | |
| busy clock | `cpu.frequency_busy_mhz` | |
| threads | sum of `processes[].threads` | |
| uptime | `system.uptime_seconds` | |

Histories for every graphed value are kept by the sampler thread in
`Histories` (150 samples each: 32 threads + 12 scalars ≈ 53 KB).

## What this page never does

- It never warns about temperature. Ben's CPU cooler is known to be
  underpowered; a high Tctl is expected. The Thermals graph puts
  temperature next to the busy clock so the effect is visible as
  data, with no colour change and no words.
- It never shows a number the audit cannot check. Peak commit charge
  is labelled as SysMon's own high-water mark.
- It never hides a spike by autoscaling without writing the ceiling.

## Verification

Listed in `FAILURE-MODES.md` rows V1–V9 and checked by:

- `ui_kittest`: meter labels equal snapshot values; per-thread graph
  count equals `core_count` in order; the toggle switches modes and
  the choice survives a restart; status bar text matches the
  snapshot; clicking a meter lands on the right card; startup lands on
  Performance; no clipping at 430 px and 1240 px in both modes.
- `scripts/accuracy-audit.sh`: rows C7, M7, M8.
- `scripts/perf.sh`: the page on both modes costs no more than the
  Overview did in 3.1.0.
- `ctl shot` on a private Xvfb: every theme × both widths × both modes.
