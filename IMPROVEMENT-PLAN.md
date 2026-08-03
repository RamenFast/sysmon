# SysMon — Improvement Plan

*Compiled 2026-08-03 at `cf5d728` (v3.0.3) from two independent high-reasoning
reviews: Fable 5 and Opus 5, both read-only, both reproducing claims against
the real 3.0.3 binary in sandboxed `XDG_RUNTIME_DIR`/`XDG_CONFIG_HOME`. Merged
by the coordinating agent; cited files/lines spot-checked. Nothing here is
implemented yet.*

**Both models' verdict:** one of the healthiest repos in the estate — the
findings are holes in the *gates*, not the architecture. The headline P1 was
found only by Opus 5 and is a live, reproduced bug. Items unique to one model
marked `[F5]` / `[O5]`.

---

## P1

### 1. The v3.0.1 `event`-f32 fix landed on only one of `tap`'s two producers `[O5]` — live bug, reproduced
`agent.rs:216-227` splices `event` into serialized text (correct, direct
path). But when a `serve`/GUI instance owns the socket, `tap` rides it, and
`control.rs:294-301` inserts `event` through `serde_json::Value` — the exact
round-trip v3.0.1 fixed. The widening actually happens at `serve.rs:46`
(`serde_json::to_value`). Reproduced: direct tap prints `0.8`; with `serve`
up, the same command prints `0.800000011920929`. Contradicts the promise at
`agent.rs:205-207` ("a consumer cannot tell … which process sampled it").
- **Fix:** one serializer + one splice for both producers — e.g.
  `Backend::snapshot_line() -> Result<String, VerbError>` implemented by both
  backends, per the note already written at `agent.rs:205-215`.
- **Verify:** the reproduce block prints identical text daemon-up and
  daemon-down; extend
  `contract.rs::stream_lines_self_identify_without_reformatting_the_numbers`
  (`:317`) to run both ways.

### 2. The harness cannot see item 1: C9/C5b check `has("event")`, never the number text `[O5]`
`scripts/conformance.sh:296-305` (C9) and `:227-235` (C5b) assert the field
exists; the only over-precision check (`contract.rs:350-364`) runs without a
daemon. The gate has a blind spot on the one path where the bug lives.
- **Fix:** add the over-precise-number count (fraction ≥12 digits) to C5b's
  socket-stream rider; mutation-proof by reverting the agent.rs splice.
- **Verify:** new clause fails today (live bug), passes after item 1.

### 3. Socket `subscribe` silently clamps intervals the CLI refuses *(both — found independently)*
`control.rs:285-288` does `.unwrap_or(1.0).clamp(0.2, 60.0)`; the CLI refuses
out-of-range as exit 3 since v3.0.1, and the sibling socket verb `interval`
(`control.rs:243-249`) also refuses. One socket, two policies for one concept.
`"interval": 99999` → silent 60s stream; `"NaN"` → silent 1.0. C13 fuzzes CLI
argv only; nothing fuzzes raw socket requests.
- **Fix:** validate in `run_subscription` with the same range/message as
  `:243-249`, `VerbError::bad_args` before the stream. Keep the default for an
  *absent* field; refuse present-but-invalid.
- **Verify:** the two `nc -U` payloads agree; add socket-hostile cases to C13.

### 4. `probe` accepts `--interval` and silently ignores it *(both)*
Shared `parse_section_args` means probe accepts/validates `-i` then discards
it (`agent.rs:78` `_interval`; window hardcoded 250ms at `:113`). The schema's
probe arguments (`agent.rs:432-436`) rightly omit it — the binary accepts a
flag its own contract doesn't publish. The exact "quietly ignored what the
caller asked" class v3.0.1 named.
- **Fix:** refuse `--interval` on probe as bad args ("probe has no cadence;
  use tap") — smaller and matches the schema (both models prefer refusal).
- **Verify:** `sysmon probe cpu -i 5; echo $?` → 3 with a fix; C13 case added.

### 5. HANDOFF.md is four releases stale — "Era: v2.2.2" *(both)*
`CLAUDE.md:16` routes zero-context agents to HANDOFF.md; it opens on v2.2.2
(2026-07-07), claims 41 green tests (real: 61), "Ten palettes" (real: 11), and
the entire v3.0.x standard/adversarial era — the most load-bearing recent
work — is absent. `docs/dev/RECEIPTS.md` is also still titled "v2 build
receipts" `[O5]`.
- **Fix:** rewrite the era block for v3.0.3; fold in the v3 decisions
  (conformance as release gate, in-repo skill, fail_forced/event-splice);
  correct counts; date-stamp RECEIPTS.md sections as historical.
- **Verify:** `head -3 HANDOFF.md` names 3.0.x; test count matches
  `cargo test --release`.

### 6. FEEDBACK.md is three releases behind its own binding SOP *(both)*
Ben's v3.0.1-round words — "verification agents are cheap, just takes time" —
are recorded as ask #22 in `docs/ASKS.md:60` but never reached FEEDBACK.md
(last entry 2026-08-01 pre-3.0.0, last commit `e209f70`). `[O5]` structural
cause worth surfacing to Ben: three overlapping ledgers (FEEDBACK.md,
docs/ASKS.md, docs/SERIOUS-TODOS.md) with no rule about which is
authoritative.
- **Fix:** backfill atomic items from asks 22-24 as dated lines; ask Ben to
  rule on ledger authority. No autonomous action beyond recording — the SOP
  forbids it.
- **Verify:** every Ben-quote row in ASKS.md §3.0.1+ has a matching dated
  ledger line.

## P2

### 7. C15 does not cover the file whose drift motivated it *(both)*
`conformance.sh:381` hardcodes `docs=(docs/AGENTS.md docs/API.md README.md)`.
`docs/skill/SKILL.md` — the file v3.0.3 pulled in-repo *so it would be
gated* — names `socat` in a code block (`SKILL.md:126`) and socat is not
installed. C16 verifies mirrors match; nothing verifies the repo's copy is
runnable here. `[F5]` also: the tool-name grep (`conformance.sh:386`) is a
closed whitelist, so docs naming any other missing tool escape.
- **Fix:** glob `docs/**/*.md` + README.md instead of the array; extend or
  generalize the tool-token extraction.
- **Verify:** mutation — bare `socat` line in SKILL.md drops the score and
  names the file.

### 8. Two CLI error paths still bypass the envelope post-v3.0.1 `[F5]`
(a) probe's serialization-failure branch calls plain `envelope::fail`,
dropping `--json`-on-a-pty (`agent.rs:117-121` — the exact C14 bug shape);
(b) tap's `try_clone` failure returns `EXIT_RUNTIME` with *nothing* on any
stream (`agent.rs:174-176`) — exit 4, zero diagnostics, no fix.
- **Fix:** (a) thread `force_json` into `fail_forced`; (b) emit an error
  envelope before returning 4.
- **Verify:** unit test forcing the branch, or grep: no bare
  `return EXIT_RUNTIME` without an emit.

### 9. `serve` and `schema` silently swallow unknown flags `[O5]`
`sysmon serve --nonsuchflag` starts serving (verified); `sysmon schema
--nonsuch` exits 0. Both discard argv (`serve.rs:79`, `agent.rs:340`). Same
shape as phosphor's "Hole 3" — a typo'd `--backgroud` daemon reports success.
- **Fix:** reject unknown args with `fail_forced(… EXIT_BAD_ARGS)`, matching
  `run_ctl` (`agent.rs:260-268`); add both to C13.
- **Verify:** both exit 3.

### 10. `ui_kittest` encodes only half the isolation law *(both)*
The law (`CLAUDE.md:50-54`): scratch `XDG_CONFIG_HOME` *and*
`XDG_RUNTIME_DIR`. `ui_kittest.rs:41-46` sets only the former. Safe today
solely because the harness passes `control_server: None` — but
`gui/app.rs:1047` resolves shot paths through `control::socket_directory()`,
so one refactor
away from writing into the real runtime dir. `contract.rs:88-100` and both
shell harnesses set both.
- **Fix:** set both vars; `[O5]` stronger: move contract.rs's `Sandbox` into a
  shared test-support module so the law is structural.
- **Verify:** `XDG_RUNTIME_DIR=/nonexistent cargo test --test ui_kittest`
  passes; nothing appears under the real runtime dir.

### 11. Per-process honesty: complete on the wire, partial on human surfaces *(both)*
The wire is exemplary (`process_source`/`process_source_hint`,
`snapshot.rs:156-160`; honest table in docs/API.md). Gaps: (a) `render_human`
(`agent.rs:707-718`) prints top_processes with no source line — a TTY user
can't tell nethogs truth from tcp_diag own-UID-TCP-only; (b) `[F5]` the GUI
TcpDiag footer (`cards.rs:1026-1030`) names the UDP/QUIC limit but not the
own-UID limit (`collect/net_process.rs:9-12`), and renders no disclosure when
shown==0.
- **Fix:** one source line in render_human's network block; extend the GUI
  footer wording; render it regardless of `shown`.
- **Verify:** `script -qec "sysmon probe network" /dev/null` shows the source.

### 12. Manpage missed the v3.0.1 `pause`/`resume`/`interval` correction `[O5]`
`docs/dev/EXTENDING.md:11-13` makes three mirrors law (schema/manpage/API.md).
v3.0.1 fixed API.md; `packaging/sysmon.1.scd:55-56` still lists `pause resume`
flat, and the `serve` section never says which verbs it refuses (authority:
schema's `needs_gui`, `agent.rs:489-492`).
- **Fix:** mirror API.md's sentence into the manpage; consider a C-check
  diffing the manpage verb list against `schema.ctl.verbs` keys.
- **Verify:** manpage and `jq '..|.needs_gui?'` tell the same story.

### 13. `schema` reply violates its own published envelope json_schema `[F5]`
`contract.envelope.json_schema` declares `additionalProperties: false` and
requires `result` when ok (`agent.rs:384-406`), but the schema reply itself is
ok with no `result` and four undeclared top-level keys — exempted only in
prose (`applies_to`, `agent.rs:377`). An agent validating replies against the
published schema fails on the very reply that delivered it.
- **Fix:** wrap the contract under `result` (clean, breaking) or publish a
  machine-readable second json_schema for the schema-reply shape.
- **Verify:** validating the schema reply against the applicable published
  schema passes.

## P3 (grouped)

- **Stale working-tree receipts** *(both)*: `e2e-out/` holds 22 PNGs from
  2026-07-07 (v2.2.2 UI), correctly gitignored/untracked — the repo question
  answers itself. Optionally point `scripts/e2e.sh` output under `target/` or
  mktemp (release.sh already passes one) so staleness stops accumulating.
- **deb description drift** `[O5]`: `packaging/build-deb.sh:80-84` says "six
  built-in themes"; the binary publishes 12 theme ids. Drop the count rather
  than pin a new one.
- **`probe` still widens f32s** `[O5]`: with item 1, three surfaces disagree
  (direct tap exact, socket tap widened, probe widened) while
  `docs/SERIOUS-TODOS.md:87-91` describes only probe as odd. Fold into item
  1's one-serializer fix, or update the note to the three-way truth.
- **`conformance.sh --only <id>`** `[O5]`: 464 lines, 17 checks,
  all-or-nothing. The dispatch table (`:447-463`) already has stable ids; a
  filter makes mutation-proving one clause cheap.
- **One clippy warning** `[O5]`: in
  `contract.rs:320-341` — the very test whose gap let item 1 survive.
  `and_then(|x| Ok(y))` → `map`. "Zero warnings is the resting state"
  (CLAUDE.md:78-79).
- **`tap --json` accepted "for symmetry" but undeclared** `[F5]`:
  `agent.rs:153` vs schema tap arguments (`:462-467`). Declare it.
- **Skill envelope example pins "3.0.0"** `[F5]`: `docs/skill/SKILL.md:38`.
  Use a placeholder so it never ages.
- **Vulkan idle numbers promised, never recorded** `[F5]`:
  `docs/dev/RECEIPTS.md` + HANDOFF promise real-desktop figures; README table
  still software-rendering only. Measure once or strike the promise.

---

## Verified healthy (both reviewers)
Clean core/app crate split (engine free of GUI deps; lib+thin-bin). Envelope
R1/R2/R3/R5 verified live, including the hand-rolled leap-day-correct
iso8601. Exit codes 0/2/3/4 correct on every provoked path including socket
`exit` classification. Schema enums read from code
(`PALETTES`/`SECTION_KEYS`), every documented jq path proven by contract
tests — "the strongest single idea in the repo". Language law holds (C11,
transitively via Cargo.lock). Netlink offsets pinned against uapi. Packaging
versions derived from the binary; dist/ and e2e-out/ correctly ignored.
61 tests green, mutation-proven C13-C16.

## Suggested order
1+2 together (fix + the gate that proves it, red-then-green) → 3+4+9 (one
"refuse what you won't honor" round, all C13-gated) → 5, 6 (ledger; needs
Ben's ruling on authority) → 7, 10 (gate hygiene) → 8, 11, 12, 13 → P3s as
touched.
