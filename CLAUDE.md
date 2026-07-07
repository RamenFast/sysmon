# CLAUDE.md — working on SysMon

SysMon is one Rust binary: an egui system monitor (default command)
plus an agent API (`probe`/`tap`/`ctl`/`schema`/`serve`) over a
control socket. Two crates: `sysmon-core` (the engine — collectors,
snapshot model, icon index) and `sysmon-app` (GUI + CLI + socket;
lib + thin bin).

**Read next, by task:**
- driving the running app / using the API → `docs/AGENTS.md`
- understanding the code → `docs/ARCHITECTURE.md`
- adding anything (collector, card, ctl verb, palette, field) →
  `docs/dev/EXTENDING.md` (exact touch-point checklists)
- running / writing tests → `docs/dev/TESTING.md`
- what each number means + tolerances → `docs/dev/ACCURACY.md`
- project state + next-session ledger → `HANDOFF.md`

## Commands

```bash
cargo build --release          # binary at target/release/sysmon
cargo test                     # the full suite (needs this machine: live /proc, network)
cargo test -p sysmon-core --lib                 # parsers only, fast, machine-agnostic
cargo test -p sysmon-core --test accuracy       # live cross-checks vs free/df/ps
cargo test -p sysmon-app  --test ui_kittest     # UI through AccessKit
scripts/e2e.sh out/ target/release/sysmon       # full live receipt run (Xvfb+openbox)
packaging/build-deb.sh && packaging/build-rpm.sh  # → packaging/dist/ (gitignored)
```

## ⛔ Laws (violating one = stop and reconsider)

- **Version law**: workspace version == `sysmon --version` == package
  filenames == git tag. One source: `[workspace.package]` in
  `Cargo.toml`.
- **Release law**: every commit landing on main IS a release — bump
  the version on the branch, merge `--no-ff`, run
  `scripts/release.sh <notes-file>` (tags, builds deb+rpm+tarball+
  SHA256SUMS, publishes on GitHub). main never carries an unreleased
  version.
- **Accuracy law**: a failing `tests/accuracy.rs` check means fix the
  collector — never widen the tolerance. Identities are documented in
  `docs/dev/ACCURACY.md`; changing one is a breaking API change.
- **Envelope law**: every CLI/socket error carries a `fix`. Exit
  codes 0/2/3/4. No silent fallbacks — degraded modes are disclosed
  (see `process_source` / `process_source_hint`).
- **Design law**: sharp corners (`CornerRadius::ZERO`), 1px hairline
  frames, monospace for data, depth only on the few stone-carved
  controls. Palettes are token blocks in `gui/theme.rs`; components
  read tokens, never hardcode colors.
- **Testing law**: anything that launches the GUI in a test uses a
  scratch `XDG_CONFIG_HOME` *and* scratch `XDG_RUNTIME_DIR` — never
  the real settings file, never the real control socket. Never map a
  window on the user's real display; use Xvfb (`scripts/e2e.sh`
  shows the pattern).
- **v1 compatibility**: `~/.config/sysmon/settings.json` must keep
  loading v1-era files (no `settings_version` key ⇒ v1; its
  `"blossom"` theme means the AMOLED look → `amoled`). Tests:
  `gui/settings.rs::tests`.

## ⚠ Gotchas that already cost a debugging loop

- egui 0.33 ↔ wgpu 27 ↔ winit 0.30 move **together**; the direct
  `wgpu` workspace dep exists only to switch backend features on
  (eframe's `wgpu` feature alone enables none → startup panic).
- `eframe`'s `accesskit` feature must stay on everywhere or
  egui-winit fails to compile under test-build feature unification.
- Glyphs outside the monospace font (Hack) tofu in the proportional
  font — arrows `↓↑` are safe only in `.monospace()` text; UI icons
  are hand-painted in `gui/cards.rs::glyph_button` for this reason.
- One concave filled polygon tessellates as stripes in egui — graph
  fills are per-segment trapezoids (`gui/graphs.rs`).
- `/proc/<pid>/stat` comm can contain spaces and parens: parse after
  the *last* `)` (`collect/process.rs::parse_stat_line`).
- Bare Xvfb has no WM: no keyboard focus, windows can't move. See
  `docs/dev/TESTING.md` before touching GUI tests.

## Conventions

- Edition 2024, toolchain pinned in `rust-toolchain.toml`. Zero
  clippy warnings is the resting state.
- Every rate-producing collector owns a `SelfInterval` window —
  never share one interval across collectors (multi-client rates
  would corrupt).
- Comments explain constraints ("why"), not narration; SPDX header
  on every source file.
- Commit messages are receipts-bearing narrative: root cause, what
  changed, the receipt.
- Wire field names carry units (`_bytes`, `_bps`, `_percent`,
  `_celsius`, `_mhz`, `_seconds`). New snapshot fields need:
  `snapshot.rs` + `sysmon schema` + `docs/API.md`.
