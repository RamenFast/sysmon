# ASKS — every unique thing Ben asked for

*The intent ledger (AGENTS.md §6). One line per unique ask, newest
era first, with where it landed. Bugs rediscover themselves here.*

## 2026-07-07 · v2.2.0 feedback round

| # | ask (Ben's words, condensed) | landed |
|---|---|---|
| 1 | "No horizontal scrollbar in processes list" | table wrapped in a horizontal ScrollArea; solid always-visible bar (`gui/processes.rs`) |
| 2 | "Sort by any factor/variable (VRAM, READ/S, NET, etc.), up/down arrow" | every column incl. Command sorts; whole-header click target, painted ▲▼ pair, accent on the live direction, persisted to settings (`SortColumn`, `header_cell`) |
| 3 | "Steal the pin button from the phosphor app" | phosphor's bevel_toggle feel ported into `glyph_button` (eased accent face, press nudge), push-pin glyph hand-painted, `P` shortcut, tooltip names it (`gui/cards.rs`, `gui/app.rs::keyboard`) |
| 4 | "Scrollbars should fit the various themes" | solid carved scrollbars on palette tokens: stone handle, accent while dragged, recessed rail (`gui/theme.rs::apply`) |
| 5 | "Overview, right click → open in process viewer" | context-menu item everywhere outside the table; jumps, selects, scrolls to the row, clears a hiding filter with a toast (`AppAction::RevealInProcesses`) |
| 6 | "Multi select (≤5), right click Details → combined meta overview, color/icon coded per process" | Ctrl+click / Shift+range in table + overview rows; Compare button; combined window: summed metrics + per-metric share bars + color-spined per-process blocks (`details.rs::combined_details_window`) |
| 7 | "Menu options have no hover effect" | custom hover-lit menu rows/chips + `hovered.weak_bg_fill` ink tint for stock menu buttons (`cards.rs::menu_item`, `theme.rs`) |

## 2026-07-07 · v2.2.1 round

| # | ask | landed |
|---|---|---|
| 8 | same seven, re-sent | diagnosis, not re-implementation: all seven live in 2.2.0; Ben's settings carried `show_pin_button:false` from an earlier era (hiding the new pin), and upgrade-day left a plain relaunch raising the OLD binary. Fixed: relaunch now replaces an older running GUI (`gui/mod.rs::negotiate_socket` compares `status.version`) |
| 9 | "README should always be sharp and consistent — not a journal" | README feature prose collapsed into the standing description, extra screenshot embed dropped; rule encoded in the `ben-repo-packaging-preferences` skill (what-changed narrative → release notes / HANDOFF / docs/dev only) |
| 10 | "only one branch (main/master) by the end" | standing law (AGENTS.md §4) — work branch folded after every merge |

## 2026-07-07 · v2.2.2 round ("refix everything")

| # | ask (Ben's words, condensed) | landed |
|---|---|---|
| 11 | "Pin button unreadable when selected (washed out) regardless of theme — I hid it because it looked horrible. Fix it. Issue still present" | the real fix this time: the active glyph no longer chases the accent (phosphor's bevel_toggle rule — glyph stays ink, the face tint + accent border + inset bevel signal active); mixes matched to phosphor's numbers (`gui/cards.rs::glyph_button`). Verified active+inactive across dark and light rooms |
| 12 | "Scrollbars hard af to see cross theme, blend into ui too much. Fix it." | handles now wear the palette's text-grade tones (ink_2 idle / ink hover / accent drag) on the recessed rail — readable in all ten rooms by construction — and widened 8→11 px (`gui/theme.rs`) |
| 13 | "Ensure this [mysterious slowness] never happens when a user is using the program" | never-silently-slow: the GUI names its adapter at startup; a CPU-rasterizer fallback (llvmpipe) is disclosed loudly — stderr with the fix, an in-window toast, and `status.renderer`/`renderer_hint` on the wire. Idle receipts: 7.9% of one core *on software rendering* (the worst case), event-driven repaints (zero work between samples) |
| 14 | "You can't find any other sensor/temp information?" (round 2 ask, now landed) | plenty: fixed the gap bug that HID k10temp's Tccd1 (channel indices aren't contiguous — now a directory scan, pinned by a new accuracy identity); added voltage rails, power draw vs cap, fan rated max; drive temps with the drive resolved (`sda · KINGSTON…`) via drivetemp/nvme, `drivetemp` loaded + persisted on Ben's machine; a gentle in-card hint names the modprobe when drives are temp-less |
| 15 | "Make a new icon — the phosphor scope art, magnifying glass over a computer screen" (round-1 ask, was never done) | drawn in phosphor's icon language: CRT-black, hairline frame, two-layer glow traces (P7-green CPU spikes + blossom-pink baseline) on a monitor, ice-blue magnifying glass showing them at 1.6×; guard-band scanned, 64 px squint-tested, resvg chain pinned by a new test |
| 16 | "Delete any other branches you have active on github, update readme/all that" | verified: `main` is the only branch, local and remote; README evergreen-edited (sensors/Sensors-card truth, disclosure line, live version strings), screenshots regenerated from the release build |

Standing (memory + AGENTS.md): always push GitHub · one working
branch at a time · deb+rpm every release · install the fresh deb on
Ben's machine · README stays evergreen.

## Earlier eras

v2.0.0 (Rust rewrite) and v2.1.0 (ten rooms) predate this ledger —
their asks live in `docs/dev/PLAN.md` and the release notes.

## 2026-08-01 · v3.0.0 round (the standard)

| # | ask (Ben's words, condensed) | landed |
|---|---|---|
| 17 | "Bring sysmon up to our UI/CLI/any other standards, w/o breaking any app functionality" | `scripts/conformance.sh` makes the standard executable (13 checks, 6/13 → 13/13); schema carries the envelope (R5) and a strict contract; `ts` is ISO-8601 (R1); stream lines carry `event` (R3); unknown commands go through the envelope; socket errors classify their own exit code. No regressions: every key and exit code of 2.2.2 survives |
| 18 | "Treat gtk like python. No gtk or python." | verified rather than assumed: no `.py` files, no v1 leftovers, and no gtk/gdk/glib/gobject/pango/atk crate in the 455-package lock tree or in the deb/rpm/desktop deps. Now a standing check (C11) |
| 19 | "The orchestrator only may edit, no subagents editing" | three read-only recon agents (CLI, UI, packaging); every finding reproduced by hand before acting; two of their claims were wrong (an egui version misread) and were rejected |
| 20 | UI law conformance | five of seven stated rules were already clean. Fixed: motion is now 120 ms smoothstep and honors reduced-motion; a Greyscale a11y floor exists, is colourless by assertion, clears WCAG AAA, and `system` mode picks it under high contrast. All eleven rooms now hold to WCAG AA by test |
| 21 | packaging/release law | `release.sh` now gates on the whole `--version` string, every README install command, `conformance.sh`, `e2e.sh`, and a checksum re-verify — and builds artifacts *before* pushing a tag, so a packaging failure can no longer strand a published tag |

## 2026-08-01 · v3.0.1 / v3.0.2 (the adversarial rounds)

| # | ask | landed |
|---|---|---|
| 22 | "verification agents are cheap, just takes time" | two read-only verifiers were pointed at the *released* binary and told to prove the conformance claim wrong. Most attacks bounced; three landed and all three were real: `tap -i NaN` panicked (exit 101, outside the standard's set), `--json` was ignored on error paths, and `event` was widening the f32 readings (`0.825` → `0.824999988079071`). Fixed in 3.0.1, with C13/C14 added to the harness |
| 23 | (self-found, walking the agent's actual path) | the docs and skill told agents to drive the socket with `socat`, which is not installed on this machine — an instruction that fails on the machine it documents. Switched to `nc -U`, and C15 now checks every external tool named in a docs code block actually exists |
| 24 | (self-found, closing the same class) | the skill lived only outside version control, which is why it drifted unseen. It is now authored at `docs/skill/SKILL.md`, installed by `scripts/install-skill.sh`, and C16 fails the release on mirror drift. Hermes stays on Ben's manual ping |

## 2026-10-01 · v3.1.0 round (can I trust the numbers?)

| # | ask (Ben's words, condensed) | landed |
|---|---|---|
| 25 | "A complete statistics accuracy check … idk if I can fully trust the statistics" | failure modes listed first (`docs/dev/FAILURE-MODES.md`), then a live audit of every shown number against the tool a skeptic would open (`scripts/accuracy-audit.sh`: idle + stress-ng). 3.0.3 scored 34 pass / 16 fail, 3.1 scores 52 / 0. A read-only Opus 5.5 auditor found 22 issues, all reproduced before acting. Each fix got a red test first, and two were proven by mutation. Biggest catches: the busy CPU clock read up to 2.1 GHz low (now ACPI CPPC delivered clock, within 5 MHz of turbostat), GPU clocks were a single 0–1760 MHz coin toss (now the window's mean), probe printed widened floats, `\| head` crashed probe (exit 101), a fresh `serve` answered with all-zero rates, per-process CPU moved in 4% steps |
| 26 | "It's saying weird things about RAM usage" | three honest numbers: *Installed* 137.4 GB (DMI sticks), *usable* 135.0 GB (kernel), *Reserved* 2.5 GB, each with a hover note. Subtitle names the sticks and speed. RSS from `statm` (it was up to 6.4 MiB low). Audit M1–M6 exact against `free -b`, DMI and `/sys/firmware/memmap` |
| 27 | "Freedom units, celsius (togglable, both on at the same time an option)" … "turn both on for me" | Settings → Temperature: °C / °F / °F + °C ("131°F · 55°C", each scale rounded on its own). `sysmon ctl temperature celsius\|fahrenheit\|both`. Ben's install set to both. JSON stays `*_celsius` |
| 28 | "GB not GiB (big memory psyop)" … "decimal everywhere, hover note okay" | decimal by default everywhere. Binary still one chip away. The Installed note says RAM is sold in binary units |
| 29 | "Take another look at the sensors section, make it easier to read" | regrouped by what it measures (CPU · each drive by model · Motherboard · Other), friendly names (Die (Tctl), Chiplet 1, Fan header N), heat bars against each sensor's own limit, unconnected inputs (−62 °C, 0 °C PCH stubs) folded away and listed on hover, fans show drive % beside rpm (a driven fan at 0 rpm is visible). `nct6775` loaded and persisted for the board's Super-I/O |
| 30 | "Allow for deeper inspection of processes" | the Inspector: parent-chain breadcrumbs, live CPU/RSS sparklines, RSS vs PSS vs USS vs swap ("memory, honestly"), open files, OOM score, cgroup, cwd, children (drill down), sockets, actions. Group by app ("thorium ×12") |
| 31 | "Nice UI/UX integration in between overview and processes tab" | top-3 rows inspect on click, "all by …" opens the table sorted that way, a summary strip on Processes links back to each card. E2E `ui_bridge` walks it both ways with a receipt |
| 32 | "When I right click … it remembers the correct selected entry" | the menu latches its target on open. A red kittest re-ranks the list while the menu is open and asserts the action lands on the right-clicked pid |
| 33 | "Cute little custom symbols … chef's touch" | 29 hand-painted glyphs (section icons, process states, sensors, inspector sections), each tested to stay inside its square. The test caught one spilling (Disk) |
| 34 | "Reduce code complexity if you can" | one card dispatcher for overview and pop-outs, one serializer for every producer, one stdout writer, shared widgets split from cards. Duplicated helpers removed (`println_checked`, two stream-line builders) |
| 35 | "Take care of github … test application" | release 3.1.0 with deb + rpm; e2e covers X11 and native Wayland (a private headless sway) and asserts a clean exit on quit, which it hadn't checked before |
| 36 | "If they need a boot flag … leave it for the next instance" | nothing needed one: `nct6775` loads without `acpi_enforce_resources`. Hardware findings for Ben (CPU Tctl ~90 °C at light load, fan1 driven at 0 rpm, RAM at 2133 not its rated 3200 XMP) are in SERIOUS-TODOS |
