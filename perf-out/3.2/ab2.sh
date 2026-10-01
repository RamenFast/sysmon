#!/usr/bin/env bash
set -u
cd "$(dirname "$0")/../.."
for round in 3 4; do
  SYSMON_PERF_PAGE=overview scripts/perf.sh /usr/bin/sysmon 20 > "perf-out/3.2/310-overview-$round.json" 2>/dev/null
  SYSMON_PERF_PAGE=performance SYSMON_PERF_CPUGRAPH=per_thread scripts/perf.sh target/release/sysmon 20 > "perf-out/3.2/320-per_thread-$round.json" 2>/dev/null
done
for f in perf-out/3.2/*-[34].json; do printf '%-28s ' "$(basename "$f")"; jq -c '{gui_cpu:.gui.cpu_percent_of_one_core, gui_pss_mb:(.gui.pss_kb/1024|floor)}' "$f"; done
