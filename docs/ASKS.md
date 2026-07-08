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

Standing (memory + AGENTS.md): always push GitHub · one working
branch at a time · deb+rpm every release · install the fresh deb on
Ben's machine.

## Earlier eras

v2.0.0 (Rust rewrite) and v2.1.0 (ten rooms) predate this ledger —
their asks live in `docs/dev/PLAN.md` and the release notes.
