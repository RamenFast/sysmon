# SysMon

A compact, theme-aware system monitor for Linux Mint / Cinnamon, built with
GTK 3 so it picks up your Mint-Y theme natively.

![overview](docs/screenshot-overview.png)

## Launching

```bash
python3 sysmon.py
```

No installation needed — the only dependencies are `python3-gi` and
`python3-psutil`, both already present on Mint. It shows up as `sysmon` in
process lists, has its own icon, and a menu entry is already installed at
`~/.local/share/applications/sysmon.desktop` (re-copy `sysmon.desktop` there
if you ever move the project).

## What it shows (Overview tab)

| Section | Readings | Graph |
|---|---|---|
| **GPU** | busy %, VRAM (level bar), core/VRAM clocks, power draw vs cap, edge + hot-spot temps, fan RPM, top-3 GPU processes | line |
| **Memory** | used %, used/available/cached, swap, top-3 by RSS | bars |
| **CPU** | overall %, per-core bars, frequency, load averages, task count, top-3 by CPU | filled area |
| **Network** | live ↓/↑ speeds, totals, top-3 processes by combined traffic | dual line |
| **Disks** | every mounted real drive with live read/write speed, label, mount point, and usage | none (by design) |

GPU data comes straight from the `amdgpu` driver's sysfs interface, and
per-process GPU usage from DRM fdinfo — the same source `nvtop` and
`amdgpu_top` use.

### Process control

Right-click any process — in the top-3 lists or the Processes tab — to end
it, force-kill it, or set its priority. Actions that need elevation (raising
priority, other users' processes) fall back to a `pkexec` authentication
dialog.

### Processes tab

The full table: icon, name, PID, user, CPU %, memory, GPU %, VRAM, disk
read/write rates, network rate, priority, and command line. Click any header
to sort; type in the filter bar to search by name, command, or PID.

### Pop-out windows

The small ⧉ toggle in any section header pops that card out into its own
window (pinned on top by default — its pin button toggles that). It keeps
updating even with the main window minimized, and it's still the same single
application. Drag a pop-out from **anywhere in its body**, not just the
title bar. Three ways to bring a card home: close the pop-out, untoggle
the ⧉ button, or just **drop the pop-out onto the main window** and it
docks back into place. Popped-out sections are remembered across launches.

## Display options (☰ menu)

* **Appearance** — follow the system theme, force light/dark, or two looks
  of the house: **Blossom (AMOLED)**, true-black surfaces with soft-pink
  titles and gold numbers, and **Funky Pink**, a bubblegum pop-art look with
  a hot-pink-to-violet header, wonky card corners, and offset shadows. Both
  auto-select their matching graph palette. Light/dark switching swaps to
  the matching variant of your current Mint theme (app-only; the rest of
  the desktop is untouched).
* **Graph colours** — seven palettes: Mint, Aqua, Sunset, Forest, Mono,
  Blossom, Funky.
* **Units** — decimal (GB, MB/s — what drive stickers and ISPs quote) or
  binary (GiB, MiB/s — what htop and GNOME System Monitor show).
* **Update interval** — 1 / 2 / 3 / 5 seconds.
* **Always on top** and **Show pin button** — the header-bar pin toggles
  always-on-top in one click.
* **Compact mode** — shrinks the graphs and hides detail rows (disks drop to
  one line per drive) so the window tucks neatly into a screen corner.
* **Overview sections** — hide any section you don't care about.

Preferences persist in `~/.config/sysmon/settings.json`.

## Per-process network (optional)

Linux doesn't expose per-process bandwidth in /proc, so this one reading
needs `nethogs` with packet-capture permission:

```bash
sudo apt install nethogs
sudo setcap 'cap_net_admin,cap_net_raw+ep' $(which nethogs)
```

SysMon detects it automatically on the next launch — no configuration. Until
then the Network section shows everything except the per-process top-3, and
the Net/s column in the Processes tab stays empty.

## Layout

```
sysmon.py                  entry point
sysmon_app/
  application.py           Gtk.Application shell (single instance)
  settings.py              persisted preferences
  theming.py               dark/light switching, graph palette, CSS
  formatting.py            human-readable units
  monitors/                one module per subsystem + the sampler thread
  widgets/                 Cairo graphs, top-3 lists, process actions
  ui/                      window, Overview page, Processes page
```

Sampling runs on a background thread; the UI only ever receives finished
snapshots, so the window stays responsive regardless of system load.

## License

GPLv3 — see [LICENSE](LICENSE).
