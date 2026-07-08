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
