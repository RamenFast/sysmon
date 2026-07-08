# SERIOUS TODOS — honest uncertainties, tracked

*The metacognitive ledger (AGENTS.md §6): while building, "what am I
not confident about?" — each answer lands here as a real TODO.*

## From the v2.2.0 build (2026-07-07)

- **[UX] Vertical scrollbar hides when horizontally scrolled left.**
  The table's own vscroll bar sits at the right edge of the *full
  table width*; on a narrow window at hscroll=0 it's off-view (the
  wheel still scrolls). Fix candidates: pin the vscroll bar to the
  viewport via `scroll_bar_rect`, or clamp the table width and let
  only the column set overflow.
- **[UX] One combined-details window at a time.** A new Compare
  replaces the previous selection's window (viewport id changes,
  old one vanishes). Fine for the 5-cap use case; revisit if Ben
  wants side-by-side comparisons.
- **[refactor] Command column width is frozen at first build.**
  Inside the horizontal ScrollArea the Command column takes
  viewport-derived spare width as `Column::initial` — after the user
  resizes any column, egui's table state owns widths and a window
  resize no longer re-stretches Command. Standard table behavior,
  but a `↺ reset columns` affordance would be honest.
- **[test] Shift+click range selection has no kittest coverage** —
  kittest's `click_modifiers` can do it; the anchor logic
  (`range_select`) is unit-testable if extracted from the page fn.
- **[debt] `run_steps(8)` sprinkled through ui_kittest** after the
  animation port — a `click_and_settle` helper would read better
  than the magic number.
- **[watch] egui upgrades vs. custom menu rows.** `menu_item` mimics
  menu row metrics by hand; an egui menu-style overhaul could make
  them look foreign. The kittest labels are the canary.

## Resolved

- ~~Details/combined viewports not poked when no pop-out open~~ —
  found during 2.2: `popout_viewports` returned early before
  rebuilding `open_viewports`; registration moved to `update()`.
