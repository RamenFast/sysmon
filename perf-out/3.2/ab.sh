#!/usr/bin/env bash
# Interleaved A/B: 3.1.0 (installed) Overview vs 3.2 Performance (both modes) vs 3.2 Overview.
set -u
cd "$(dirname "$0")/../.."
for round in 1 2; do
  SYSMON_PERF_PAGE=overview scripts/perf.sh /usr/bin/sysmon 20 > "perf-out/3.2/310-overview-$round.json" 2>/dev/null
  SYSMON_PERF_PAGE=performance SYSMON_PERF_CPUGRAPH=combined scripts/perf.sh target/release/sysmon 20 > "perf-out/3.2/320-combined-$round.json" 2>/dev/null
  SYSMON_PERF_PAGE=performance SYSMON_PERF_CPUGRAPH=per_thread scripts/perf.sh target/release/sysmon 20 > "perf-out/3.2/320-per_thread-$round.json" 2>/dev/null
  SYSMON_PERF_PAGE=overview scripts/perf.sh target/release/sysmon 20 > "perf-out/3.2/320-overview-$round.json" 2>/dev/null
  echo "JCODE_PROGRESS {\"message\":\"round $round done\"}"
done
for f in perf-out/3.2/*.json; do printf '%-28s ' "$(basename "$f")"; jq -c '{v:.version, gui_cpu:.gui.cpu_percent_of_one_core, gui_pss_mb:(.gui.pss_kb/1024|floor), sampler_cpu:.sampler.cpu_percent_of_one_core, r:.gui.renderer}' "$f"; done
