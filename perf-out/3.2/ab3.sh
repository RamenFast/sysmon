#!/usr/bin/env bash
# A/B after R1, with perf.sh counting reaped children (cutime+cstime).
# Asserts V8: per_thread ≤ 3.1.0 Overview × 1.05. Exits 1 on a miss.
set -u
cd "$(dirname "$0")/../.."
SYSMON_PERF_PAGE=overview scripts/perf.sh /usr/bin/sysmon 20 > perf-out/3.2/ab3-310-overview.json 2>/dev/null
SYSMON_PERF_PAGE=performance SYSMON_PERF_CPUGRAPH=per_thread scripts/perf.sh target/release/sysmon 20 > perf-out/3.2/ab3-320-per_thread.json 2>/dev/null
SYSMON_PERF_PAGE=performance SYSMON_PERF_CPUGRAPH=combined scripts/perf.sh target/release/sysmon 20 > perf-out/3.2/ab3-320-combined.json 2>/dev/null
base=$(jq .gui.cpu_percent_of_one_core perf-out/3.2/ab3-310-overview.json)
pt=$(jq .gui.cpu_percent_of_one_core perf-out/3.2/ab3-320-per_thread.json)
cb=$(jq .gui.cpu_percent_of_one_core perf-out/3.2/ab3-320-combined.json)
echo "3.1.0 overview (incl children): $base   3.2 per_thread: $pt   3.2 combined: $cb"
awk -v b="$base" -v p="$pt" 'BEGIN { if (p <= b * 1.05) { print "V8 pass"; exit 0 } else { print "V8 FAIL"; exit 1 } }'
