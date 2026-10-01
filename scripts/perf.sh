#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Performance receipt: what one SysMon binary costs on this machine.
#
#   scripts/perf.sh [path-to-sysmon] [seconds]      → one JSON object on stdout
#
# Three numbers, each measured from the outside (/proc), never from
# the binary's own opinion of itself:
#   sampler   `sysmon serve` fed by `tap all -i 1`: CPU% of the daemon
#             and its PSS — the cost of sampling every section at 1 Hz
#   gui       the full window on a private Xvfb at a 1 s interval:
#             CPU% and PSS after a warm-up (llvmpipe: software
#             rendering, so CPU here is an upper bound)
#   probe     wall time of `probe all` (includes the fixed 250 ms
#             two-sample window), median of five
#
# Isolation: private XDG_RUNTIME_DIR and XDG_CONFIG_HOME, private
# Xvfb display; the user's window and settings are never touched.
set -euo pipefail

bin="$(realpath "${1:-target/release/sysmon}")"
seconds="${2:-30}"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/sysmon-perf.XXXXXX")"
export XDG_RUNTIME_DIR="${scratch}/run" XDG_CONFIG_HOME="${scratch}/config"
mkdir -p "${XDG_RUNTIME_DIR}" "${XDG_CONFIG_HOME}/sysmon"
chmod 700 "${XDG_RUNTIME_DIR}"
unset WAYLAND_DISPLAY WAYLAND_SOCKET
clk_tck="$(getconf CLK_TCK)"
pids=()

cleanup() {
  for pid in "${pids[@]}"; do kill "${pid}" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "${scratch}"
}
trap cleanup EXIT

# utime+stime+cutime+cstime ticks of one pid (fields 14-17 after the
# comm parens). The reaped-children terms matter: a binary that shells
# out per frame (3.2 reviewer, R1: `gsettings` per graph) hides its
# cost there, and the 3.1 version of this line could not see it.
ticks() { sed 's/.*) //' "/proc/$1/stat" | awk '{print $12 + $13 + $14 + $15}'; }
pss_kb() { awk '/^Pss:/ {print $2}' "/proc/$1/smaps_rollup"; }
rss_kb() { awk '/^VmRSS:/ {print $2}' "/proc/$1/status"; }

# cpu_percent <pid> <seconds>: share of ONE core over the window
cpu_percent() {
  local before after
  before="$(ticks "$1")"
  sleep "$2"
  after="$(ticks "$1")"
  awk -v a="${after}" -v b="${before}" -v t="${clk_tck}" -v s="$2" \
    'BEGIN {printf "%.2f", (a - b) / t / s * 100}'
}

wait_socket() {
  for _ in $(seq 1 100); do
    [ -S "${XDG_RUNTIME_DIR}/sysmon/ctl.sock" ] && return 0
    sleep 0.1
  done
  echo "socket never appeared" >&2
  return 1
}

# ── sampler ──────────────────────────────────────────────────────────
"${bin}" serve >/dev/null 2>&1 &
serve_pid=$!
pids+=("${serve_pid}")
wait_socket
"${bin}" tap all -i 1 >/dev/null 2>&1 &
pids+=("$!")
sleep 5 # warm-up: caches filled, nethogs child settled
sampler_cpu="$(cpu_percent "${serve_pid}" "${seconds}")"
sampler_pss="$(pss_kb "${serve_pid}")"
sampler_rss="$(rss_kb "${serve_pid}")"
"${bin}" ctl quit >/dev/null 2>&1 || true
sleep 1

# ── gui ──────────────────────────────────────────────────────────────
display=":$((90 + RANDOM % 9))"
Xvfb "${display}" -screen 0 1280x900x24 >/dev/null 2>&1 &
pids+=("$!")
sleep 1
# Which page the window measures on (SYSMON_PERF_PAGE: performance |
# overview | processes; 3.1 and older ignore the key and open the
# Overview) and the Performance page's CPU graph (SYSMON_PERF_CPUGRAPH:
# auto | combined | per_thread).
perf_page="${SYSMON_PERF_PAGE:-performance}"
perf_cpugraph="${SYSMON_PERF_CPUGRAPH:-auto}"
printf '{"settings_version":2,"update_interval_seconds":1.0,"start_page":"%s","cpu_graph_mode":"%s"}' \
  "${perf_page}" "${perf_cpugraph}" >"${XDG_CONFIG_HOME}/sysmon/settings.json"
DISPLAY="${display}" "${bin}" >/dev/null 2>&1 &
gui_pid=$!
pids+=("${gui_pid}")
wait_socket
sleep 10 # warm-up: icon index, first textures, font atlas
gui_cpu="$(cpu_percent "${gui_pid}" "${seconds}")"
gui_pss="$(pss_kb "${gui_pid}")"
gui_rss="$(rss_kb "${gui_pid}")"
renderer="$("${bin}" ctl status 2>/dev/null | jq -r '.result.renderer // "unknown"')"
"${bin}" ctl quit >/dev/null 2>&1 || true

# ── probe ────────────────────────────────────────────────────────────
probe_ms=()
for _ in 1 2 3 4 5; do
  start="$(date +%s%N)"
  "${bin}" probe all --json >/dev/null
  probe_ms+=("$(( ($(date +%s%N) - start) / 1000000 ))")
done
probe_median="$(printf '%s\n' "${probe_ms[@]}" | sort -n | sed -n 3p)"

jq -n \
  --arg version "$("${bin}" --version)" \
  --arg renderer "${renderer}" \
  --arg page "${perf_page}" --arg cpugraph "${perf_cpugraph}" \
  --argjson seconds "${seconds}" \
  --argjson sampler_cpu "${sampler_cpu}" --argjson sampler_pss "${sampler_pss}" \
  --argjson sampler_rss "${sampler_rss}" \
  --argjson gui_cpu "${gui_cpu}" --argjson gui_pss "${gui_pss}" --argjson gui_rss "${gui_rss}" \
  --argjson probe_ms "${probe_median}" \
  --argjson processes "$(ls -d /proc/[0-9]* | wc -l)" \
  '{version: $version, window_seconds: $seconds, processes_on_machine: $processes,
    sampler: {cpu_percent_of_one_core: $sampler_cpu, pss_kb: $sampler_pss, rss_kb: $sampler_rss},
    gui: {cpu_percent_of_one_core: $gui_cpu, pss_kb: $gui_pss, rss_kb: $gui_rss, renderer: $renderer,
          page: $page, cpu_graph: $cpugraph},
    probe_all_wall_ms_median: $probe_ms}'
