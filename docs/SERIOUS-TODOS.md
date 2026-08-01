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

## From the v3.0.0 standard round (2026-08-01)

- **[fixed] `run_steps(8)` after a right-click was a real flake, not
  debt.** The 2.2.0 entry below called the magic number a style
  problem; it was failing ~1 run in 6. Two causes, both now closed:
  the context menu needed a *wait-until-present* (`settle_until`)
  rather than a fixed frame count, and the overview's live top-process
  ranking could re-order between reading a row's pid and clicking its
  menu item. The test now freezes sampling (the app's own pause) and
  outwaits the in-flight sample. 0 failures in 25 consecutive runs,
  from ~4 in 25 before.
- **[fixed] A rate limiter was being reported as a collector bug.**
  `live_net_attribution` probed the network with a 1 KB GET and then
  depended on a 20 MB one. When Cloudflare's speed endpoint started
  answering 429 (after repeated runs), the small probe still passed,
  so three tests failed claiming "tcp_diag saw only 0 B/s" — blaming
  sysmon for someone else's refusal. The suite now asks each candidate
  endpoint for a real megabyte, falls back to a second host, and skips
  honestly when neither will serve.
- **[watch] The greyscale room's chroma comes only from app icons.**
  A sampled scan of the release screenshot shows 0.5% of pixels with
  chroma > 12 (a normal room is ~88%), and the remainder is
  desktop-entry icons, which are deliberately left recognizable. If a
  strict monochrome mode is ever wanted, the icon rasterizer is the
  one place left to desaturate.
- **[watch] `scripts/conformance.sh` encodes the standard as it reads
  today.** If AGENT-CLI-STANDARD.md changes, the harness is the second
  place to edit, and there is no automation linking them. The doctor
  checks the envelope; only this harness checks the whole contract.

## From the v3.0.1 adversarial round (2026-08-01)

- **[fixed] `sysmon tap -i NaN` panicked.** `NaN` and `inf` parse as
  f64 and then panic inside `Duration::from_secs_f64` — a raw Rust
  backtrace and exit 101, a code the standard does not define. Found
  by an adversarial verification pass, not by the suite. Non-finite
  and out-of-range intervals are now refused as bad arguments (exit 3)
  rather than clamped silently. New check C13 fuzzes fourteen hostile
  inputs for panics and out-of-set exits.
- **[fixed] `--json` was ignored on error paths.** On a pty, an agent
  passing `--json` got prose on stderr and nothing parseable on
  stdout — precisely when it most needed a machine answer. New check
  C14.
- **[fixed] `event` widened the f32 readings.** Inserting the field
  via `serde_json::Value` re-typed every f32 as f64, so a GPU voltage
  that printed `0.825` began printing `0.824999988079071`. Now spliced
  into the serialized text, and pinned by a contract test.
- **[watch] `probe` has always widened f32s** (it went through `Value`
  in v2 as well), so `probe` and `tap` disagree on the text of the
  same reading. Left alone deliberately: changing `probe` now would be
  the very regression this round was fixing. Worth unifying behind one
  serializer if the shapes are ever revisited.

## From the v3.0.2 walkthrough (2026-08-01)

- **[fixed] The docs told agents to use a tool this machine does not
  have.** `docs/AGENTS.md`, `docs/API.md` and the sysmon skill all
  demonstrated the raw socket with `socat`, which is not installed
  here — found by running the skill's own examples verbatim instead of
  reading them. Switched to `nc -U` (base system, verified working).
  C15 now fails if any external tool named in a docs code block is
  missing, and is mutation-proven.
- **[fixed] The skill files are no longer checked by hand.** C15
  covered the repo's docs, but `~/.claude/skills/sysmon/SKILL.md` and
  its `~/.agents` mirror live outside the repo and had drifted the same
  way. The skill is now authored at `docs/skill/SKILL.md` — in version
  control, reviewed with the code that it documents — and
  `scripts/install-skill.sh` installs or verifies the mirrors. C16
  fails the release gate on drift (mutation-proven). The Hermes mirror
  is deliberately left to Ben's manual ping.
