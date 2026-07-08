# Extending SysMon — worked recipes

Checklists for the common changes, written for a zero-context
executor: every touch point named, nothing left to inference. Find
symbols with `grep -rn "<symbol>" crates/` — symbol anchors are used
instead of line numbers because lines rot.

Legend (stable): ▸ one step · 📁 exact file · ✅ runnable verify +
expected result · ⚠ verified gotcha.

⚠ Global: after ANY user-visible change, three mirrors must agree —
`sysmon schema` (agent.rs::build_schema), the manpage
(packaging/sysmon.1.scd), and docs/API.md. Stale mirrors are bugs.

---

## Recipe 1 — add a field to an existing section

Example shape: add `iowait_percent` to the CPU section.

- ▸ 📁 `crates/sysmon-core/src/snapshot.rs` — add the field to the
  section struct. Name carries the unit (`_percent`). Optional
  values are `Option<T>` + `#[serde(skip_serializing_if = "Option::is_none")]`
  — never fake a 0 for "unknown".
- ▸ 📁 `crates/sysmon-core/src/collect/<section>.rs` — fill it in
  `collect()`. If it's a rate, the delta state lives in the
  collector struct and the window comes from the existing
  `SelfInterval` (`let interval_seconds = self.window.tick(now)` is
  already there — reuse it, never add a second clock).
- ▸ Add/extend a parser unit test in the same file's `mod tests`
  with a fixture string.
- ▸ 📁 `crates/sysmon-core/tests/accuracy.rs` — if an independent
  authority exists (a /proc re-read, a coreutils tool), add a
  cross-check with a justified tolerance. A field nobody can verify
  should make you suspicious of the field.
- ▸ 📁 `crates/sysmon-app/src/gui/cards.rs` — render it (stat_grid
  entry or a row) using `sysmon_core::units` formatters.
- ▸ Update the three mirrors (⚠ Global above).
- ✅ `cargo test -p sysmon-core` → all green, including your new test.
- ✅ `./target/debug/sysmon probe <section> --json | jq .result.<section>.<field>`
  → a plausible live value, not null (unless honestly absent).

---

## Recipe 2 — add a whole new section + card

The longest checklist. Every step is real; skipping one produces a
section that exists on the wire but not in the UI (or vice versa).

**core:**
- ▸ 📁 `crates/sysmon-core/src/collect/<name>.rs` — new collector.
  Copy the shape of `sensors.rs` (stateless) or `disk.rs` (delta
  state + `SelfInterval`). Parsers are pure functions over `&str`
  with fixture tests in `mod tests`.
- ▸ 📁 `crates/sysmon-core/src/snapshot.rs` — new section struct
  (serde, unit-suffixed names) + an `Option<…>` field on
  `SystemSnapshot` + a `bool` on `Wants` + arms in
  `Wants::from_section_name`, `Wants::all`, `Wants::union`.
  ⚠ If the section depends on another (like cpu⇒sensors for the
  package temp), encode that in `from_section_name`, not in callers.
- ▸ 📁 `crates/sysmon-core/src/collect/mod.rs` — field on `Sampler`,
  init in `with_options`, `if wants.<name> { … }` arm in `sample()`.

**app:**
- ▸ 📁 `crates/sysmon-app/src/gui/settings.rs` — add the key to
  `SECTION_KEYS` (drives visibility toggles + pop-out validation).
- ▸ 📁 `crates/sysmon-app/src/gui/cards.rs` — `pub fn <name>_card(
  ui, cx: &mut CardContext, …)` using `card_frame` + `card_header`
  (gives you the title/headline/pop-out chrome for free). Honest
  empty state in the gentle voice when the data can be absent.
- ▸ 📁 `crates/sysmon-app/src/gui/app.rs` — three places:
  `overview()` (render arm, respecting `section_visible` +
  `popped(…)`), `popout_viewports()` title match, and its section
  render match.
- ▸ 📁 `crates/sysmon-app/src/gui/app.rs :: process_commands` — the
  popout/popin fix-text lists sections; update it.
- ▸ 📁 `crates/sysmon-app/src/agent.rs` — `SECTION_NAMES` const +
  the `build_schema` sections object.
- ▸ 📁 `crates/sysmon-app/src/control.rs :: wants_from_request` —
  fix-text section list.
- ▸ 📁 `crates/sysmon-app/src/lib.rs :: print_help` — SECTIONS line.
- ▸ Mirrors: manpage + docs/API.md.
- ✅ `cargo test` → green.
- ✅ `./target/debug/sysmon probe <name> --json | jq '.result.<name>'`
  → real data.
- ✅ `scripts/e2e.sh /tmp/e2e-out target/debug/sysmon` → still green
  end to end; eyeball your card in `/tmp/e2e-out/*-overview.png`.

---

## Recipe 3 — add a ctl verb

- ▸ Decide the shape: does it need the GUI (like `theme`) or should
  serve answer too (like `interval`)? GUI-only verbs go through the
  `GuiCommand` queue; both-mode verbs become `Backend` trait methods.
- ▸ 📁 `crates/sysmon-app/src/agent.rs :: run_ctl` — argument
  parsing: verbs taking one value belong in the
  `"page" | "theme" | …` match arm; also the no-verb fix text.
- ▸ 📁 `crates/sysmon-app/src/control.rs :: dispatch` — route it:
  GUI verbs join the `"raise" | "page" | …` arm; both-mode verbs
  get a `Backend` method (implement in `serve.rs::ServeBackend` and
  `gui/backend.rs::GuiBackend` — an honest error in whichever mode
  can't do it). Update the unknown-verb fix text.
- ▸ GUI verbs: 📁 `crates/sysmon-app/src/gui/app.rs ::
  process_commands` — the match arm that actually does it (main
  thread; persist via `self.settings.save()` when it changes
  settings). Reply through `Ok(json!({…}))` / `Err((msg, fix))`.
  ⚠ Deferred replies (like `shot`): stash the reply sender, `continue`
  instead of replying, answer later — see `pending_screenshot`.
- ▸ Mirrors: schema verbs list (agent.rs), CTL VERBS in
  `lib.rs::print_help`, manpage, docs/API.md.
- ✅ with a running `--background` instance:
  `./target/debug/sysmon ctl <verb> <value> | jq .` → your ok
  envelope; and `sysmon ctl <verb>` with bad args → error + fix,
  exit 2.

---

## Recipe 4 — add a theme palette

- ▸ 📁 `crates/sysmon-app/src/gui/theme.rs` — new `Palette` entry in
  `PALETTES`. ⚠ Bump the array length in the type
  (`[Palette; N]`) or it won't compile. Fill EVERY token including
  the stone triple and `title`/`value`; design the theme whole,
  don't invert another.
- ▸ Same file, `companion_graph_palette` — if the theme should pull
  a matching graph palette (v1 behavior for the blossom family).
- ▸ 📁 `crates/sysmon-app/src/gui/app.rs :: process_commands` — the
  `theme` verb's fix text enumerates ids; add yours.
- ▸ Menu, settings validation, and ctl acceptance are automatic
  (they iterate `PALETTES` / use `palette_by_id`).
- ▸ Mirrors: docs/AGENTS.md theme list, manpage if it enumerates.
- ✅ `sysmon ctl theme <id>` on a `--background` instance, then
  `sysmon ctl shot /tmp/t.png` → eyeball contrast: title/value
  readable on surface, muted readable on plane, hairline visible on
  both plane and surface.
- ⚠ Graph palettes are a separate table (`GRAPH_PALETTES`, same
  file — same array-length bump rule; five semantic series:
  gpu, memory, cpu, net-down, net-up; light AND dark variants).

---

## Recipe 5 — add a process-table column

- ▸ 📁 `crates/sysmon-app/src/gui/processes.rs` — five places:
  `SortColumn` enum · its `id()`/`from_id()` maps (stable settings
  ids) · the `columns` array (title, sort id, width — ⚠ bump its
  `[…; N]` length) · a compare arm in `sort_records` (default sort
  direction lives in `SortColumn::defaults_descending`) · the row
  cell in body order (cells MUST stay in column-array order —
  there's no keying, order is the contract).
- ▸ Numeric cells: right-aligned mono via `mono_cell`; blank noise
  below a threshold with `blank_under`; `None` renders "—" (means
  unreadable) vs "" (means zero/noise) — keep that distinction.
- ✅ `cargo test -p sysmon-app --test ui_kittest` → green.
- ✅ run the GUI, click the new header twice → sorts both ways, no
  shimmer (the stable pid tiebreak does that).

---

## Recipe 6 — bump the egui family

⚠ egui, eframe, egui_extras, egui_kittest, egui-winit (transitive),
and wgpu move in **lockstep**. The compatible set is pinned in
`Cargo.toml [workspace.dependencies]` with a comment naming it.

- ▸ Update all of them together; never one.
- ▸ Keep `eframe` features: `default_fonts, wgpu, x11, wayland,
  accesskit` (accesskit off breaks test builds; wgpu backend
  features come from the direct `wgpu` dep).
- ✅ `cargo test` → green. ✅ `scripts/e2e.sh` → green (catches
  render/viewport regressions the type system can't).

---

## Debug instruments

- `SYSMON_DEBUG_FPS=1` — prints frames-per-10s to stderr (idle
  should sit ≈ interval-driven, ~15/10s at 2 s + safety repaint;
  hundreds = a repaint loop, go hunting).
- `SYSMON_DEBUG_DRAG=1` — per-frame pop-out drag signals
  (outer rect, button state, main rect) — the drag-dock triad.
- `RUST_BACKTRACE=1` — as usual; the GUI logs panics to stderr.
