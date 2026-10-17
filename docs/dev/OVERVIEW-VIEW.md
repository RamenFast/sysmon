# The Overview

*Spec for SysMon's one front page. The code in
`crates/sysmon-app/src/gui/overview.rs`, the cards in `cards.rs` and
the painters in `graphs.rs` are compiled from this file: when they
disagree, this file wins and the code is regenerated.*

*Revision 3 (2026-10-16, SysMon 3.6): the Performance page is folded
into the Overview. Ben: "The performance and overview tabs seem
redundant, could we consolidate any unique information into the
overview tab and release/delete the performance overview one? I really
like how other task managers do it where there are different colored
lines for each cpu thread and it's displayed in one window view … A
button to switch views for anything you determine to be ideal would be
swell." Creative and technical direction delegated for the round.*

*Revisions 1–2 (2026-10-01, SysMon 3.2) specified the separate
Performance page: LED meters, etched group boxes, a two-way CPU
toggle. That text is in git history as `docs/dev/PERFORMANCE-VIEW.md`;
what survived is below.*

## Why one page

Two pages showed the same six resources twice: the Overview as cards
with numbers and top processes, the Performance page as meters and
graphs. Neither had everything. The fold keeps one card per resource
and gives each card the Performance page's best part, its graph on a
dark instrument field, plus every number only the Performance page had.
The LED meters go (the card headline already states the value), and so
do the group boxes (their numbers are card stats now).

## Layout

The central panel's width decides, each frame.

- **One column** (< 880 px, the 430 px default window): the cards top to
  bottom in the user's card order, scrolling.
- **Two columns** (≥ 880 px, `TWO_COLUMN_WIDTH`): the cards alternate
  left and right (default order: GPU · CPU · Disks on the left, Memory ·
  Network · Sensors on the right), so both columns end near the same
  height.
- **Status bar** along the bottom while the Overview is showing (below).

Pop-outs, drag-to-dock, card order and "scroll to card" behave as
before; a popped-out card draws the same graphs.

## The CPU card's three views

A three-position carved stone switch sits right of the CPU card's
legend row: `total` | `threads` | `grid`. It is the one carved control
on the card, which is how the eye finds it. The choice persists in
`settings.cpu_graph_mode`; `sysmon ctl cpugraph <view>` sets it.

- **threads** (the default): every thread as its own coloured line on
  one field, 1.6 × a normal graph's height so thirty-two lines can be
  told apart. Thread *i* wears colour *i* (below) in the lines, the
  bars under the graph and the grid. Pointing at the field picks the
  line nearest the pointer at that sample: it is drawn on top at full
  strength, the others dim to 22 %, and the readout names it ("thread
  7: 41%", then the busiest thread and the average of all). Pointing at
  a bar in the row underneath singles out that thread the same way: the
  bars are the lines' legend.
- **total**: one scope graph, busy % in the CPU colour with kernel time
  (system + irq + softirq) as a quieter second trace; bars underneath
  in the CPU colour.
- **grid**: a small graph per thread in `/proc/stat` order (cpu0
  top-left, row-major), columns `clamp(floor(width / 96), 2, 8)`, each
  cell ≥ 28 px tall with its thread number in the corner, each trace in
  its thread's colour. No bars: the grid already shows every thread.

The three replace 3.2's `combined` / `per_thread` / `auto`. Settings
migrate on load: `combined` → `total`; `auto`, `per_thread` and
anything unknown → `threads`. `ctl cpugraph` with an old word exits 3
and its `fix` names the new ones.

### Thread colours

`theme::thread_colors(graph_palette, count)`: hues stepped by the
golden angle so neighbouring threads never sit next to each other on
the wheel, alternating two brightness steps. The graph palette keeps
its character: Amber stays within warm hues, Terminal within green to
cyan, Mono is grey and carries identity in brightness alone. Every
thread colour holds ≥ 3:1 against the dark field in every room (unit
test `thread_colours_read_on_the_field_and_mono_stays_grey`).

## The graphs, card by card

Every graph is a scope graph on `Palette::field()` with a legend row
above it that names each colour where it is used (`■ busy  ■ VRAM`).

| card | graph | series |
|---|---|---|
| GPU | GPU history | busy %, VRAM % |
| Memory | Memory history | used and cache, stacked |
| CPU | the three views | above |
| Network | network rate | down, up; autoscaled with the ceiling floored at 20 kB/s and written in the corner |
| Disks | Disk history | read, write (summed across drives); autoscaled with the ceiling written; "busiest N%" (the busiest drive's util) right of the legend |
| Sensors | Thermal history | CPU °C, GPU edge °C; 0..100 °C until a reading passes 100, then the ceiling rises and is written; the busy clock as a number right of the legend, not a third trace |

### Scope graph

- The **field** is dark in every room: dark rooms darken the plane,
  light rooms use the ink (deep plum on Blossom, deep slate on Light).
  The field is always darker than every line drawn on it.
- A **graticule** of hairlines at 25/50/75 % plus vertical lines every
  15 samples that scroll left one step per sample, so time visibly
  passes; fixed with reduced motion on.
- Newest at the right, 150 samples (`HISTORY_LENGTH`). The first series
  gets a soft fill (one trapezoid per segment); later ones are bare
  lines, dashed in greyscale.
- Percentages are fixed 0..100. Rates autoscale to `observed_max ×
  1.15` above a floor, with the ceiling written top-right in the muted
  ink (`↑ 48 MB/s`), so a flat line is never read as "nothing" when it
  is "small against a big spike".
- **Hover** reads every series at the pointer's sample. The accessible
  label is the graph's name plus the newest readout ("GPU history: busy
  3%  VRAM 3%"), which is what the tests check against the snapshot.

### Mini graph

A grid cell is a scope graph in its lean form: the midline only,
vertical graticule every 30 samples, no fill, a 1 px stroke, the thread
number as its tag. Thirty-two draw every frame; the lean field is what
keeps the grid cheap (V8, V10).

## Colours by concept

Stable across the page, pulled from the active graph palette's five
series plus derived tints:

| concept | colour |
|---|---|
| CPU busy | `GRAPH_SERIES_CPU` |
| CPU kernel time | `GRAPH_SERIES_CPU` mixed 45 % toward the ink (a quieter sibling) |
| CPU thread *i* | `thread_colors(...)[i]` |
| memory used | `GRAPH_SERIES_MEMORY` |
| memory cache | `GRAPH_SERIES_MEMORY` at 55 % alpha; used+cache filled, then used filled on top, so the band between them is the cache |
| GPU busy | `GRAPH_SERIES_GPU` |
| VRAM | `GRAPH_SERIES_GPU` mixed 45 % toward the ink |
| disk read, network down | `GRAPH_SERIES_NET_DOWN` |
| disk write, network up | `GRAPH_SERIES_NET_UP` |
| CPU temperature | `palette.value` (gold) |
| GPU temperature | `palette.title` (rose) |

On light rooms the sibling tints and the thermal pair are chosen to read
on the dark field (≥ 3:1 against `field()`), not on the page.

## Numbers folded in from the Performance page

| was | now |
|---|---|
| Totals: processes, threads, ctx/s | CPU stats: Process count, **Threads** (`cpu.tasks_total`), Ctx/s, plus **Kernel %** |
| Totals: uptime | status bar (wide) |
| Commit charge: total, limit, Peak* | Memory stats: **Commit** "x / limit", **Peak\*** |
| Kernel memory: slab, page tables, stacks | Memory stats: **Slab**, **Page tables**, **Kernel stacks** |
| Physical memory box | already on the Memory card (installed / usable / reserved) |
| CPU kernel-time trace | the `total` view's second trace |
| VRAM, cache, disk, thermal graphs | the cards' graphs above |

*Peak\* is the highest commit charge seen since SysMon started, an
in-process high-water mark. The asterisk is in the label and the hover
says so; it is never presented as a kernel figure.

| shown | source |
|---|---|
| CPU busy % / kernel % | `cpu.overall_percent`, `cpu.kernel_percent` (audited C7) |
| per thread % | `cpu.per_core_percent` |
| memory used / cache | `memory.used_bytes`, `memory.cached_bytes` over `total_bytes` |
| commit charge / limit | `memory.committed_bytes`, `memory.commit_limit_bytes` (audited M7) |
| kernel memory | `memory.slab_bytes`, `page_tables_bytes`, `kernel_stack_bytes` (audited M8) |
| GPU busy / VRAM | `gpu.busy_percent`, `gpu.vram_used_bytes / vram_total_bytes` |
| disk read / write | sum of `disks[].read_bps`, `write_bps` |
| network | `network.download_bps`, `upload_bps` |
| temperatures | `cpu.temperature_celsius`, `gpu.temperature_edge_celsius` |
| busy clock | `cpu.frequency_busy_mhz` |
| threads | `cpu.tasks_total` |
| uptime | `system.uptime_seconds` |

Histories for every graphed value are kept by the sampler thread in
`Histories` (150 samples each).

## Status bar

A bottom panel in `surface_2`, monospace 11 px, segments separated by
`│`, most important first so a narrow window loses the tail (the label
truncates and shows everything on hover):

```
636 proc │ CPU 4% │ GPU 3% │ 160°F · 71°C │ Commit 30.1 GB / 67.5 GB │ up 3d 04:12
```

The commit limit and the uptime appear from 560 px
(`WIDE_STATUS_WIDTH`). Every number is the same snapshot field the cards
show.

## Rows under the pointer

A process row under the pointer, in the Processes table, a card's top-3
or the Inspector's children, wears the room's soft tint
(`Palette::row_hover()`, the accent at 12 %) and a 2 px accent edge on
the left. A selected row is a step firmer (`row_selected()`, 20 %) and
keeps its own text colours. Ben, 3.6: "upon hover over a process the
text is impossible to read … make the highlight on hover a little less
strong on the eyes while still remaining legible."

The old table hover was the ink itself: egui_extras paints a hovered
row with `widgets.hovered.bg_fill`, which the theme keeps at text grade
for scrollbar handles. The table body now scopes the row tokens to
itself, and the scrollbar, drawn outside the body, keeps its handle.
Readability is a test, not a judgement: primary and secondary text hold
≥ 4.5:1 on both states in every room, and muted text keeps most of its
resting contrast or stays at AA.

Click targets on the card and Inspector rows are keyed by pid, not by
slot, so a re-rank landing between the paint and the click still acts
on the process that was clicked (P5).

## Windows

Every SysMon window (the main window, pop-outs, details and combined
details) carries the Wayland app_id / X11 class `sysmon`, so a
compositor can recognise all of them with one rule. On Ben's SwayFX the
greenhouse setup floats them all
(`for_window [app_id="^sysmon$"] floating enable`).

## What this page never does

- It never warns about temperature. A high Tctl is expected on Ben's
  machine; the thermal graph puts temperature next to the busy clock so
  the cost is visible as data, with no colour change and no words.
- It never shows a number the audit cannot check. Peak commit charge is
  labelled as SysMon's own high-water mark.
- It never hides a spike by autoscaling without writing the ceiling.

## Verification

`FAILURE-MODES.md` rows V1–V12, checked by:

- `ui_overview`: startup lands on the Overview and no Performance tab
  is left; each graph's readout is the snapshot; the switch steps total
  · threads · grid and the choice persists; grid cells in order with
  their values, ≥ 28 px at 430 and 1240 px; nothing clips at 430 px;
  every folded-in number is on a card; the status bar is the snapshot;
  1240 px lays out two columns.
- `ui_row_hover`: the real table, the pointer on a row, pixels read
  back: the soft tint, never the ink, in a dark and a light room.
- `ui_bridge`: P5 on the CPU card's top-3.
- unit tests: thread colours on the field, row-state contrast, nearest
  line under the pointer, graticule step, scope ceiling, grid columns,
  settings migration from 3.5.
- `scripts/accuracy-audit.sh`: rows C7, M7, M8.
- `scripts/perf.sh`: the Overview in its default `threads` view.
- `scripts/e2e.sh`: every theme × Overview (threads, total, grid) and
  Processes, on a private Xvfb.
