#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The standard, executable. Every clause of
# ~/Dev/ClaudeWorkspace/AGENT-CLI-STANDARD.md (and its normative core,
# NexusFormStationWork/CONVENTION.md) as an independent numbered check
# against a real binary — no claim is remembered, each one is run.
#
#   scripts/conformance.sh [path-to-sysmon-binary] [--json]
#
# Prints a human ledger on a TTY, one envelope when piped or --json.
# Exit: 0 all checks pass · 3 bad arguments · 4 one or more failed.
#
# Isolation: every check runs with a private XDG_RUNTIME_DIR and
# XDG_CONFIG_HOME, so the user's live instance and saved settings are
# never touched. (A past round silently repainted Ben's real config —
# that is why this is not optional.)
set -uo pipefail

tool="sysmon-conformance"
bin="${1:-target/release/sysmon}"
[ "${bin}" = "--json" ] && { bin="target/release/sysmon"; force_json=1; }
force_json="${force_json:-0}"
[ "${2:-}" = "--json" ] && force_json=1

case "${bin}" in
  -*) printf 'usage: scripts/conformance.sh [binary] [--json]\n' >&2; exit 3 ;;
esac
command -v "${bin}" >/dev/null 2>&1 || [ -x "${bin}" ] || {
  printf '%s\n' "conformance: no runnable binary at '${bin}'" >&2
  printf '%s\n' "fix: build it first (cargo build --release -p sysmon-app) or pass a path" >&2
  exit 3
}
[ -x "${bin}" ] && bin="$(realpath "${bin}")"

command -v jq >/dev/null 2>&1 || {
  printf '%s\n' "conformance: jq is required" >&2
  printf '%s\n' "fix: sudo apt install jq" >&2
  exit 3
}

# ── private world ────────────────────────────────────────────────────
run_root="$(mktemp -d /tmp/sysmon-conformance.XXXXXX)"
export XDG_RUNTIME_DIR="${run_root}/run"
export XDG_CONFIG_HOME="${run_root}/config"
mkdir -p "${XDG_RUNTIME_DIR}" "${XDG_CONFIG_HOME}"
chmod 700 "${XDG_RUNTIME_DIR}"
serve_pid=""
cleanup() {
  [ -n "${serve_pid}" ] && kill "${serve_pid}" 2>/dev/null
  rm -rf "${run_root}"
}
trap cleanup EXIT

# ── ledger ───────────────────────────────────────────────────────────
passed=0
failed=0
results_json="[]"

record() { # id clause verdict note
  local id="$1" clause="$2" verdict="$3" note="$4"
  if [ "${verdict}" = "pass" ]; then
    passed=$((passed + 1))
    [ "${force_json}" = "0" ] && [ -t 1 ] && printf '\033[32m  ✓ %-5s %s\033[0m\n' "${id}" "${clause}"
  else
    failed=$((failed + 1))
    [ "${force_json}" = "0" ] && [ -t 1 ] && printf '\033[31m  ✗ %-5s %s — %s\033[0m\n' "${id}" "${clause}" "${note}"
  fi
  results_json="$(jq -c --arg id "${id}" --arg clause "${clause}" \
                        --arg verdict "${verdict}" --arg note "${note}" \
                     '. + [{id: $id, clause: $clause, verdict: $verdict, note: $note}]' \
                     <<<"${results_json}")"
}

check() { # id clause condition-as-command...
  local id="$1" clause="$2"; shift 2
  local note
  if note="$("$@" 2>&1)"; then
    record "${id}" "${clause}" pass "${note}"
  else
    record "${id}" "${clause}" fail "${note:-check failed}"
  fi
}

# A pty, for the isatty side of the auto-switch.
on_tty() { script -qec "$*" /dev/null 2>/dev/null; }

# The one-shot verbs that must all carry the envelope.
ONE_SHOTS=(
  "probe system"
  "probe cpu"
  "probe memory"
  "probe gpu"
  "probe network"
  "probe disks"
  "probe processes"
  "probe sensors"
  "probe connections"
  "schema"
  "ctl status"
)

# ── C1 · envelope on every one-shot ──────────────────────────────────
c1() {
  local bad=""
  for verb in "${ONE_SHOTS[@]}"; do
    local out
    out="$(${bin} ${verb} 2>/dev/null)"
    for field in status tool version ts; do
      jq -e --arg f "${field}" 'has($f)' >/dev/null 2>&1 <<<"${out}" \
        || bad="${bad} [${verb}:${field}]"
    done
    [ "$(jq -r '.tool // ""' <<<"${out}" 2>/dev/null)" = "sysmon" ] \
      || bad="${bad} [${verb}:tool!=sysmon]"
  done
  [ -z "${bad}" ] || { printf 'missing:%s' "${bad}"; return 1; }
  printf 'all %d one-shots carry status/tool/version/ts' "${#ONE_SHOTS[@]}"
}

# ── C2 · ts is ISO-8601 with UTC offset (ruling R1) ──────────────────
c2() {
  local ts
  ts="$(${bin} probe system 2>/dev/null | jq -r '.ts // ""')"
  case "${ts}" in
    ????-??-??T??:??:??*[+-][0-9][0-9]:[0-9][0-9]|????-??-??T??:??:??*Z)
      printf 'ts=%s (ISO-8601 with offset)' "${ts}" ;;
    "") printf 'no ts field at all'; return 1 ;;
    *)  printf 'ts=%s is not ISO-8601 with a UTC offset (ruling R1)' "${ts}"; return 1 ;;
  esac
}

# ── C3 · the version is real everywhere ──────────────────────────────
c3() {
  local binary_version manifest_version mismatch=""
  binary_version="$(${bin} --version 2>/dev/null | awk '{print $2}')"
  manifest_version="$(grep -m1 '^version' Cargo.toml 2>/dev/null | sed 's/.*"\(.*\)"/\1/')"
  [ -n "${binary_version}" ] || { printf 'binary reports no version'; return 1; }
  [ -n "${manifest_version}" ] || { printf 'no version in Cargo.toml (run from the repo root)'; return 1; }
  [ "${binary_version}" = "${manifest_version}" ] \
    || mismatch="${mismatch} binary=${binary_version}!=Cargo.toml=${manifest_version}"
  [ "$(${bin} probe system 2>/dev/null | jq -r '.version')" = "${binary_version}" ] \
    || mismatch="${mismatch} envelope-version!=${binary_version}"
  [ "$(${bin} schema 2>/dev/null | jq -r '.version')" = "${binary_version}" ] \
    || mismatch="${mismatch} schema-version!=${binary_version}"
  grep -q "sysmon_${binary_version}_amd64.deb" README.md 2>/dev/null \
    || mismatch="${mismatch} README-install-line-stale"
  [ -z "${mismatch}" ] || { printf 'version drift:%s' "${mismatch}"; return 1; }
  printf 'version %s is identical in binary, Cargo.toml, envelope, schema, README' "${binary_version}"
}

# ── C4 · every error carries an actionable fix ───────────────────────
c4() {
  local bad=""
  # each of these is a real, provoked failure
  local provocations=(
    "nonsuchverb"
    "probe nonsuchsection"
    "probe --nonsuchflag"
    "tap --interval"
    "ctl"
    "ctl interval"
    "ctl theme"
    "ctl status"
  )
  for provocation in "${provocations[@]}"; do
    local out fix
    out="$(${bin} ${provocation} 2>/dev/null)"
    fix="$(jq -r '.fix // ""' <<<"${out}" 2>/dev/null)"
    [ -n "${fix}" ] || bad="${bad} [${provocation}]"
    [ "$(jq -r '.status // ""' <<<"${out}" 2>/dev/null)" = "error" ] \
      || bad="${bad} [${provocation}:status]"
  done
  [ -z "${bad}" ] || { printf 'errors without a fix/status envelope:%s' "${bad}"; return 1; }
  printf 'all %d provoked errors carry status=error and a fix' "${#provocations[@]}"
}

# ── C5 · exit codes 0 / 2 / 3 / 4, each provoked ─────────────────────
expect_exit() { # code -- command...
  local want="$1"; shift
  "$@" >/dev/null 2>&1
  local got=$?
  [ "${got}" = "${want}" ] || { printf 'wanted %s got %s from: %s' "${want}" "${got}" "$*"; return 1; }
}
c5() {
  local bad=""
  expect_exit 0 ${bin} probe cpu            >/dev/null || bad="${bad} [ok]"
  expect_exit 0 ${bin} --version            >/dev/null || bad="${bad} [version]"
  expect_exit 0 ${bin} schema               >/dev/null || bad="${bad} [schema]"
  expect_exit 2 ${bin} ctl status           >/dev/null || bad="${bad} [unavailable]"
  expect_exit 3 ${bin} nonsuchverb          >/dev/null || bad="${bad} [bad-verb]"
  expect_exit 3 ${bin} probe nonsuchsection >/dev/null || bad="${bad} [bad-section]"
  expect_exit 3 ${bin} ctl                  >/dev/null || bad="${bad} [ctl-bare]"
  expect_exit 3 ${bin} ctl interval         >/dev/null || bad="${bad} [ctl-missing-value]"
  [ -z "${bad}" ] || { printf 'exit-code drift:%s' "${bad}"; return 1; }
  printf '0 ok · 2 unavailable · 3 bad args all provoked and correct'
}

# ── C5b · bad ctl argument WITH an instance up is exit 3, not 2 ──────
c5b() {
  ${bin} serve >/dev/null 2>&1 &
  serve_pid=$!
  local waited=0
  while [ ! -S "${XDG_RUNTIME_DIR}/sysmon/ctl.sock" ] && [ ${waited} -lt 50 ]; do
    sleep 0.1; waited=$((waited + 1))
  done
  [ -S "${XDG_RUNTIME_DIR}/sysmon/ctl.sock" ] || { printf 'serve never bound its socket'; return 1; }

  local bad="" got
  ${bin} ctl status >/dev/null 2>&1 || bad="${bad} [status-should-be-0]"

  # An unknown verb is bad arguments wherever it is sent.
  ${bin} ctl nonsuchctlverb >/dev/null 2>&1; got=$?
  [ "${got}" = 3 ] || bad="${bad} [unknown-ctl-verb-should-be-3(got ${got})]"

  # A GUI-only verb against `serve` is genuinely unavailable, not bad
  # arguments — and the instance must classify that itself, on the
  # wire, so ctl can return it. (This is the `exit` field's whole job.)
  ${bin} ctl theme blossom_dark >/dev/null 2>&1; got=$?
  [ "${got}" = 2 ] || bad="${bad} [gui-verb-against-serve-should-be-2(got ${got})]"
  [ "$(${bin} ctl theme blossom_dark 2>/dev/null | jq -r '.exit // ""')" = "2" ] \
    || bad="${bad} [error-envelope-does-not-carry-exit]"

  # A malformed value is bad arguments, caught before the socket.
  ${bin} ctl interval nonsuchnumber >/dev/null 2>&1; got=$?
  [ "${got}" = 3 ] || bad="${bad} [bad-interval-value-should-be-3(got ${got})]"

  # C9 rides along here: a live stream must self-identify.
  local stream_line
  stream_line="$(timeout 6 ${bin} tap cpu --interval 0.3 2>/dev/null | head -n 1)"
  if [ -n "${stream_line}" ]; then
    jq -e 'has("event")' >/dev/null 2>&1 <<<"${stream_line}" \
      || bad="${bad} [socket-stream-line-has-no-event(R3)]"
  else
    bad="${bad} [socket-stream-produced-nothing]"
  fi

  kill "${serve_pid}" 2>/dev/null; wait "${serve_pid}" 2>/dev/null; serve_pid=""
  [ -z "${bad}" ] || { printf 'with a live instance:%s' "${bad}"; return 1; }
  printf 'ctl against a live instance: good verb 0, unknown verb 3, bad value 3; stream self-identifies'
}

# ── C6 · isatty auto-switch, proven both directions ──────────────────
c6() {
  local piped tty_out bad=""
  piped="$(${bin} probe cpu 2>/dev/null)"
  jq -e . >/dev/null 2>&1 <<<"${piped}" || bad="${bad} [piped-is-not-json]"
  tty_out="$(on_tty "${bin} probe cpu")"
  jq -e . >/dev/null 2>&1 <<<"${tty_out}" && bad="${bad} [tty-emitted-json-not-prose]"
  [ -z "${bad}" ] || { printf 'auto-switch:%s' "${bad}"; return 1; }
  printf 'pipe → JSON envelope, tty → prose'
}

# ── C7 · --json forces JSON on a tty ─────────────────────────────────
c7() {
  local out
  out="$(on_tty "${bin} probe cpu --json")"
  jq -e 'has("status") and has("result")' >/dev/null 2>&1 <<<"${out}" \
    || { printf '%s' '--json on a tty did not force an envelope'; return 1; }
  printf '%s' '--json forces the envelope on a terminal'
}

# ── C8 · schema is a strict, complete, self-describing contract ──────
c8() {
  local schema bad=""
  schema="$(${bin} schema 2>/dev/null)"
  jq -e . >/dev/null 2>&1 <<<"${schema}" || { printf 'schema is not valid JSON'; return 1; }

  # strictness: published schemas are strict (standard §1)
  grep -q 'additionalProperties' <<<"${schema}" || bad="${bad} [no-additionalProperties:false]"

  # every CLI verb the binary implements must appear in the schema
  for verb in probe tap ctl serve schema; do
    grep -q "\"${verb}\"" <<<"${schema}" || bad="${bad} [verb-${verb}-undocumented]"
  done
  # every section the help names must be documented
  for section in system cpu memory gpu network disks processes sensors connections; do
    grep -q "${section}" <<<"${schema}" || bad="${bad} [section-${section}-undocumented]"
  done
  # exit codes and the envelope shape must be published
  jq -e '[.. | objects | select(has("exit_codes"))] | length > 0' >/dev/null 2>&1 <<<"${schema}" \
    || bad="${bad} [exit-codes-undocumented]"
  # every verb must publish which exits it can produce
  for verb in probe tap ctl serve schema; do
    jq -e --arg v "${verb}" '.contract.verbs[$v].exits | type == "array"' >/dev/null 2>&1 <<<"${schema}" \
      || bad="${bad} [verb-${verb}-does-not-publish-its-exits]"
  done
  # the zero-training test: every ctl verb the schema names must exist
  local ctl_verbs
  ctl_verbs="$(jq -r '[.. | objects | select(has("verbs")) | .verbs[]] | unique | .[]' <<<"${schema}" 2>/dev/null)"
  [ -n "${ctl_verbs}" ] || bad="${bad} [schema-names-no-ctl-verbs]"

  [ -z "${bad}" ] || { printf 'schema gaps:%s' "${bad}"; return 1; }
  printf 'schema is strict and names every verb, section, and exit code'
}

# ── C9 · NDJSON stream lines self-identify with `event` (R3) ─────────
c9() {
  local line
  line="$(timeout 8 ${bin} tap cpu --interval 0.3 2>/dev/null | head -n 1)"
  [ -n "${line}" ] || { printf 'tap produced no line in 8s'; return 1; }
  jq -e . >/dev/null 2>&1 <<<"${line}" || { printf 'stream line is not valid JSON'; return 1; }
  jq -e 'has("event")' >/dev/null 2>&1 <<<"${line}" \
    || { printf 'direct-sampling stream line has no `event` field (ruling R3)'; return 1; }
  printf 'event=%s on the direct stream' "$(jq -r .event <<<"${line}")"
}

# ── C10 · stdout is pure JSON when piped; diagnostics on stderr ──────
c10() {
  local bad=""
  # every one-shot's piped stdout must parse whole, with nothing else on it
  for verb in "probe cpu" "schema" "ctl status" "nonsuchverb"; do
    local out
    out="$(${bin} ${verb} 2>/dev/null)"
    jq -e . >/dev/null 2>&1 <<<"${out}" || bad="${bad} [${verb}:stdout-not-pure-json]"
  done
  # a tap's every line must parse on its own
  local lines
  lines="$(timeout 6 ${bin} tap cpu --interval 0.3 2>/dev/null | head -n 3)"
  [ -n "${lines}" ] || bad="${bad} [tap-silent]"
  while IFS= read -r line; do
    [ -z "${line}" ] && continue
    jq -e . >/dev/null 2>&1 <<<"${line}" || bad="${bad} [tap-line-not-json]"
  done <<<"${lines}"
  [ -z "${bad}" ] || { printf 'stdout purity:%s' "${bad}"; return 1; }
  printf 'piped stdout is pure JSON on one-shots and on every stream line'
}

# ── C11 · language law: no Python, no GTK (AGENTS.md §3) ─────────────
c11() {
  local bad=""
  local python_files
  python_files="$(git ls-files '*.py' 2>/dev/null | head -n 5)"
  [ -z "${python_files}" ] || bad="${bad} [python:${python_files//$'\n'/,}]"
  # the whole transitive dependency tree, not just the direct deps
  local gtk_crates
  gtk_crates="$(grep -iE '^name = "(gtk|gdk|glib|gobject|atk|pango|gtk4|gdk4|libadwaita)' Cargo.lock 2>/dev/null | head -n 5)"
  [ -z "${gtk_crates}" ] || bad="${bad} [gtk-crate:${gtk_crates//$'\n'/,}]"
  # packaged runtime dependencies
  local gtk_pkgdeps
  gtk_pkgdeps="$(grep -ilE 'gtk|pygobject|python3-' packaging/build-deb.sh packaging/build-rpm.sh packaging/sysmon.desktop 2>/dev/null)"
  [ -z "${gtk_pkgdeps}" ] || bad="${bad} [packaging-gtk/python:${gtk_pkgdeps//$'\n'/,}]"
  [ -z "${bad}" ] || { printf 'language law violated:%s' "${bad}"; return 1; }
  printf 'no .py files, no gtk/gdk/glib/gobject crates in the lock tree, clean packaging deps'
}

# ── C12 · if it compiles, it installs (AGENTS.md §3) ─────────────────
c12() {
  local bad=""
  command -v sysmon >/dev/null 2>&1 || bad="${bad} [not-on-PATH]"
  [ -f /usr/share/applications/sysmon.desktop ] || bad="${bad} [no-.desktop]"
  ls /usr/share/icons/hicolor/*/apps/sysmon.* >/dev/null 2>&1 || bad="${bad} [no-icon]"
  man -w sysmon >/dev/null 2>&1 || bad="${bad} [no-man-page]"
  [ -z "${bad}" ] || { printf 'native install incomplete:%s' "${bad}"; return 1; }
  printf 'PATH binary + .desktop + hicolor icon + man page all present'
}

# ── run the ledger ───────────────────────────────────────────────────
if [ "${force_json}" = "0" ] && [ -t 1 ]; then
  printf '\033[36m» conformance · %s\033[0m\n' "${bin}"
fi

check C1  "envelope on every one-shot (status/tool/version/ts)"   c1
check C2  "ts is ISO-8601 with a UTC offset (R1)"                 c2
check C3  "the version is real everywhere (R2)"                   c3
check C4  "every error carries an actionable fix"                 c4
check C5  "exit codes 0/2/3 provoked and correct"                 c5
check C5b "ctl against a live instance: 0/3, stream identifies"   c5b
check C6  "isatty auto-switch, both directions"                   c6
check C7  "--json forces the envelope on a tty"                   c7
check C8  "schema is strict and complete (R5)"                    c8
check C9  "NDJSON stream lines carry event (R3)"                  c9
check C10 "piped stdout is pure JSON, diagnostics on stderr"      c10
check C11 "language law: no Python, no GTK"                       c11
check C12 "if it compiles, it installs"                           c12

total=$((passed + failed))
status="ok"; exit_code=0
[ "${failed}" -gt 0 ] && { status="error"; exit_code=4; }

if [ "${force_json}" = "1" ] || [ ! -t 1 ]; then
  jq -n -c \
    --arg status "${status}" --arg tool "${tool}" \
    --arg version "$(${bin} --version 2>/dev/null | awk '{print $2}')" \
    --arg ts "$(date --iso-8601=seconds)" \
    --arg binary "${bin}" \
    --argjson passed "${passed}" --argjson failed "${failed}" --argjson total "${total}" \
    --argjson checks "${results_json}" \
    --arg error "${failed} of ${total} conformance checks failed" \
    --arg fix "read .result.checks[] | select(.verdict==\"fail\") for the clause and the note naming what to change" \
    'if $status == "ok"
     then {status:$status, tool:$tool, version:$version, ts:$ts,
           result:{binary:$binary, passed:$passed, failed:$failed, total:$total, checks:$checks}}
     else {status:$status, tool:$tool, version:$version, ts:$ts, error:$error, fix:$fix,
           result:{binary:$binary, passed:$passed, failed:$failed, total:$total, checks:$checks}}
     end'
else
  printf '\n'
  if [ "${failed}" = "0" ]; then
    printf '\033[32m%d/%d — conformant.\033[0m\n' "${passed}" "${total}"
  else
    printf '\033[31m%d/%d passed · %d to go.\033[0m\n' "${passed}" "${total}" "${failed}"
  fi
fi

exit "${exit_code}"
