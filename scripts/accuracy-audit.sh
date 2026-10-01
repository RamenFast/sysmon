#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The live accuracy audit: every number SysMon shows, next to the
# independent tool a skeptical human would open beside it, on THIS
# machine's real hardware — once idle and once under load.
#
#   scripts/accuracy-audit.sh [path-to-sysmon] [out-dir]
#
# Output: <out-dir>/report.json (one envelope, every check) and
# <out-dir>/report.md (the same, human-readable). Exit 0 when every
# check passes, 4 when any fails. Failure modes this hunts are listed
# in docs/dev/FAILURE-MODES.md; ids here match ids there.
#
# Authorities: free, /sys/firmware/memmap, udev DMI (DIMMs), turbostat
# (root via sudoplz when available), mpstat, sensors -j, lspci, df,
# findmnt, ps, /proc/net/dev. Missing authorities skip their checks
# out loud (verdict "skip" + the package to install), never silently.
#
# Isolation: probes only (no GUI, no socket, no settings); load comes
# from stress-ng with a hard timeout.
set -uo pipefail

bin="$(realpath "${1:-target/release/sysmon}")"
out="${2:-audit-out/$(date +%Y%m%dT%H%M%S)}"
mkdir -p "${out}"
results="${out}/checks.ndjson"
: >"${results}"
export XDG_RUNTIME_DIR="$(mktemp -d)" XDG_CONFIG_HOME="$(mktemp -d)"
trap 'rm -rf "${XDG_RUNTIME_DIR}" "${XDG_CONFIG_HOME}"' EXIT

have() { command -v "$1" >/dev/null 2>&1; }
root() { if have sudoplz; then sudoplz sudo "$@"; else sudo -n "$@"; fi; }

# record <id> <phase> <what> <ours> <authority> <source> <verdict> [note]
record() {
  jq -cn --arg id "$1" --arg phase "$2" --arg what "$3" --arg ours "$4" \
    --arg authority "$5" --arg source "$6" --arg verdict "$7" --arg note "${8:-}" \
    '{id:$id, phase:$phase, what:$what, ours:$ours, authority:$authority,
      source:$source, verdict:$verdict, note:$note}' >>"${results}"
}
# within <a> <b> <tolerance> → pass|fail (absolute difference)
within() { awk -v a="$1" -v b="$2" -v t="$3" 'BEGIN {d=a-b; if (d<0) d=-d; print (d<=t) ? "pass" : "fail"}'; }
skip() { record "$1" "$2" "$3" "-" "-" "$4" "skip" "install: $5"; }

probe() { "${bin}" probe "$@" --json | jq -c .result; }

audit_phase() {
  local phase="$1"
  local snap
  snap="$(probe all)"

  # ── memory ─────────────────────────────────────────────────────────
  local mem_total mem_avail free_total free_avail
  mem_total="$(jq -r .memory.total_bytes <<<"${snap}")"
  mem_avail="$(jq -r .memory.available_bytes <<<"${snap}")"
  read -r free_total free_avail < <(free -b | awk '/^Mem:/ {print $2, $7}')
  record M4 "${phase}" "memory usable total (bytes)" "${mem_total}" "${free_total}" "free -b" \
    "$([ "${mem_total}" = "${free_total}" ] && echo pass || echo fail)"
  record M1 "${phase}" "memory available (bytes)" "${mem_avail}" "${free_avail}" "free -b" \
    "$(within "${mem_avail}" "${free_avail}" $((free_total / 33)))" "3% of RAM moves between reads"

  local dimm_sum installed_ours speed_cfg speed_ours
  dimm_sum="$(udevadm info -q property -p /sys/devices/virtual/dmi/id 2>/dev/null \
    | awk -F= '/^MEMORY_DEVICE_[0-9]+_SIZE=/ {s+=$2} END {print s+0}')"
  installed_ours="$(jq -r '.memory.installed_bytes // "absent"' <<<"${snap}")"
  if [ "${dimm_sum}" -gt 0 ]; then
    record M2 "${phase}" "memory installed (bytes)" "${installed_ours}" "${dimm_sum}" \
      "udev DMI MEMORY_DEVICE_*_SIZE" \
      "$([ "${installed_ours}" = "${dimm_sum}" ] && echo pass || echo fail)"
    speed_cfg="$(udevadm info -q property -p /sys/devices/virtual/dmi/id \
      | awk -F= '/^MEMORY_DEVICE_0_CONFIGURED_SPEED_MTS=/ {print $2}')"
    speed_ours="$(jq -r '.memory.modules[0].configured_speed_mts // "absent"' <<<"${snap}")"
    record M5 "${phase}" "memory configured speed (MT/s)" "${speed_ours}" "${speed_cfg}" \
      "udev DMI CONFIGURED_SPEED_MTS" \
      "$([ "${speed_ours}" = "${speed_cfg}" ] && echo pass || echo fail)"
  else
    skip M2 "${phase}" "memory installed" "udev DMI" "systemd-udev (DMI memory tables)"
  fi
  local memmap_ram
  memmap_ram=0
  for d in /sys/firmware/memmap/*; do
    [ "$(cat "$d/type")" = "System RAM" ] || continue
    memmap_ram=$((memmap_ram + $(($(cat "$d/end") - $(cat "$d/start") + 1))))
  done
  record M2b "${phase}" "usable ≤ firmware System RAM ≤ installed" \
    "${mem_total}" "${memmap_ram} / ${dimm_sum}" "/sys/firmware/memmap" \
    "$([ "${mem_total}" -le "${memmap_ram}" ] && [ "${memmap_ram}" -le "${dimm_sum}" ] && echo pass || echo fail)"
  # Commit charge + kernel memory: the same /proc/meminfo lines, read
  # right after the probe. Committed_AS moves with every mmap, so a
  # 2% band; the kernel lines barely move.
  local mi_committed mi_limit mi_slab mi_stack mi_pt
  read -r mi_committed mi_limit mi_slab mi_stack mi_pt < <(awk '
    /^Committed_AS:/ {c=$2*1024} /^CommitLimit:/ {l=$2*1024} /^Slab:/ {s=$2*1024}
    /^KernelStack:/ {k=$2*1024} /^PageTables:/ {p=$2*1024}
    END {print c, l, s, k, p}' /proc/meminfo)
  local ours_committed
  ours_committed="$(jq -r .memory.committed_bytes <<<"${snap}")"
  record M7 "${phase}" "commit charge Committed_AS (bytes)" "${ours_committed}" "${mi_committed}" \
    "/proc/meminfo (next read)" "$(within "${ours_committed}" "${mi_committed}" $((mi_committed / 50)))" \
    "±2%: address space is promised and released constantly"
  record M7b "${phase}" "commit limit (bytes)" "$(jq -r .memory.commit_limit_bytes <<<"${snap}")" "${mi_limit}" \
    "/proc/meminfo CommitLimit" \
    "$([ "$(jq -r .memory.commit_limit_bytes <<<"${snap}")" = "${mi_limit}" ] && echo pass || echo fail)"
  record M8 "${phase}" "kernel memory Slab/KernelStack/PageTables (bytes)" \
    "$(jq -r '"\(.memory.slab_bytes)/\(.memory.kernel_stack_bytes)/\(.memory.page_tables_bytes)"' <<<"${snap}")" \
    "${mi_slab}/${mi_stack}/${mi_pt}" "/proc/meminfo (next read)" \
    "$({ [ "$(within "$(jq -r .memory.slab_bytes <<<"${snap}")" "${mi_slab}" $((mi_slab / 20)))" = pass ] \
        && [ "$(within "$(jq -r .memory.kernel_stack_bytes <<<"${snap}")" "${mi_stack}" $((mi_stack / 10 + 1)))" = pass ] \
        && [ "$(within "$(jq -r .memory.page_tables_bytes <<<"${snap}")" "${mi_pt}" $((mi_pt / 10 + 1)))" = pass ]; } \
       && echo pass || echo fail)" "±5% slab, ±10% stacks/page tables between reads"
  local swaps
  swaps="$(awk 'NR>1' /proc/swaps | wc -l)"
  record M6 "${phase}" "swap devices vs swap_total" "$(jq -r .memory.swap_total_bytes <<<"${snap}")" \
    "${swaps} device(s)" "/proc/swaps" \
    "$({ [ "${swaps}" -eq 0 ] && [ "$(jq -r .memory.swap_total_bytes <<<"${snap}")" -eq 0 ]; } \
       || { [ "${swaps}" -gt 0 ] && [ "$(jq -r .memory.swap_total_bytes <<<"${snap}")" -gt 0 ]; } && echo pass || echo fail)"

  # ── cpu ────────────────────────────────────────────────────────────
  if have mpstat; then
    # The same 2 s window for both: tap's second line (the first is
    # the delta baseline) runs concurrently with mpstat's interval.
    # Identity: busy = everything but idle AND iowait (htop's, ours);
    # mpstat's 100 − %idle would count iowait as busy.
    local ours_cpu mp_cpu mp_iowait ours_iowait mp_kernel ours_kernel tap_file
    tap_file="$(mktemp)"
    ("${bin}" tap cpu -i 2 2>/dev/null | head -2 | tail -1 >"${tap_file}") &
    local tap_pid=$!
    # mpstat columns: CPU %usr %nice %sys %iowait %irq %soft %steal %guest %gnice %idle
    read -r mp_cpu mp_iowait mp_kernel < <(LC_ALL=C mpstat 2 1 \
      | awk '/^Average:/ && $2=="all" {print 100-$NF-$6, $6, $5+$7+$8}')
    # Its own pid: a bare `wait` would also wait out the load generator.
    wait "${tap_pid}"
    ours_cpu="$(jq -r .cpu.overall_percent "${tap_file}")"
    ours_iowait="$(jq -r '.cpu.iowait_percent // "absent"' "${tap_file}")"
    ours_kernel="$(jq -r '.cpu.kernel_percent // "absent"' "${tap_file}")"
    rm -f "${tap_file}"
    record C2 "${phase}" "cpu overall busy % (iowait counted idle)" "${ours_cpu}" "${mp_cpu}" \
      "mpstat 2 1: 100 − idle − iowait (same window)" \
      "$(within "${ours_cpu}" "${mp_cpu}" 8)" "concurrent 2 s windows, ±8 pp"
    record C2b "${phase}" "cpu iowait %" "${ours_iowait}" "${mp_iowait}" "mpstat 2 1 %iowait" \
      "$([ "${ours_iowait}" = absent ] && echo fail || within "${ours_iowait}" "${mp_iowait}" 8)" \
      "concurrent 2 s windows, ±8 pp"
    record C7 "${phase}" "cpu kernel time % (sys+irq+soft)" "${ours_kernel}" "${mp_kernel}" \
      "mpstat 2 1 %sys + %irq + %soft" \
      "$([ "${ours_kernel}" = absent ] && echo fail || within "${ours_kernel}" "${mp_kernel}" 8)" \
      "concurrent 2 s windows, ±8 pp"
    record C7b "${phase}" "cpu kernel % ≤ busy %" "${ours_kernel}" "${ours_cpu}" "same snapshot" \
      "$(awk -v k="${ours_kernel}" -v b="${ours_cpu}" 'BEGIN {print (k<=b+0.001) ? "pass" : "fail"}')"
  else
    skip C2 "${phase}" "cpu overall busy %" "mpstat" "sysstat"
  fi
  local ts_out bzy avg ours_peak ours_mean
  # turbostat prints its own column order whatever --show says; map
  # by header name. The cpu probe runs concurrently with its window.
  local cpu_file
  cpu_file="$(mktemp)"
  ("${bin}" tap cpu -i 2 2>/dev/null | head -2 | tail -1 >"${cpu_file}") &
  local cpu_tap_pid=$!
  ts_out="$(root turbostat --quiet --Summary --show Busy%,Bzy_MHz,Avg_MHz --interval 2 \
    --num_iterations 1 2>/dev/null | awk 'NR==1 {for (i=1;i<=NF;i++) col[$i]=i; next}
      NR==2 {print $col["Bzy_MHz"], $col["Avg_MHz"]}')"
  wait "${cpu_tap_pid}"
  ours_mean="$(jq -r .cpu.frequency_mhz "${cpu_file}")"
  ours_peak="$(jq -r '.cpu.frequency_busy_mhz // "absent"' "${cpu_file}")"
  rm -f "${cpu_file}"
  if [ -n "${ts_out}" ]; then
    read -r bzy avg <<<"${ts_out}"
    record C1 "${phase}" "cpu clock while busy (MHz)" "${ours_peak}" "${bzy}" "turbostat Bzy_MHz (same window)" \
      "$([ "${ours_peak}" = absent ] && echo fail || within "${ours_peak}" "${bzy}" 700)" \
      "busy-weighted CPPC delivered clock vs APERF/MPERF (same counters, separate 2 s windows); ±700 MHz"
    record C1b "${phase}" "cpu mean clock (MHz)" "${ours_mean}" "${avg}" "turbostat Avg_MHz (incl. idle)" \
      "info" "different quantities: mean of requested clocks vs time-averaged incl. sleep"
  else
    skip C1 "${phase}" "cpu clock" "turbostat" "linux-tools (turbostat) + root"
  fi
  local tctl ours_temp
  ours_temp="$("${bin}" probe cpu --json | jq -r '.result.cpu.temperature_celsius // "absent"')"
  tctl="$(sensors -j 2>/dev/null | jq -r '[.[] | objects | to_entries[] | select(.key=="Tctl") | .value.temp1_input][0] // empty')"
  if [ -n "${tctl}" ]; then
    record C3 "${phase}" "cpu temperature (°C)" "${ours_temp}" "${tctl}" "sensors -j k10temp Tctl (next read)" \
      "$(within "${ours_temp}" "${tctl}" 5)" "±5 °C: Tctl moves several °C per second under load"
  else
    skip C3 "${phase}" "cpu temperature" "sensors" "lm-sensors"
  fi
  local hw_min hw_max
  hw_min="$(($(cat /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq) / 1000))"
  hw_max="$(($(cat /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq) / 1000))"
  record C6 "${phase}" "cpu hardware range (MHz)" \
    "$(jq -r '"\(.cpu.frequency_min_mhz|floor)–\(.cpu.frequency_max_mhz|floor)"' <<<"${snap}")" \
    "${hw_min}–${hw_max}" "cpufreq cpuinfo_min/max_freq" \
    "$([ "$(jq -r '.cpu.frequency_max_mhz|floor' <<<"${snap}")" = "${hw_max}" ] && echo pass || echo fail)"

  # ── gpu ────────────────────────────────────────────────────────────
  local gpu_name lspci_name
  gpu_name="$(jq -r .gpu.device_name <<<"${snap}")"
  lspci_name="$(lspci -mm -d 1002: -nn 2>/dev/null | awk -F'"' '/VGA|Display/ {print $6; exit}')"
  record G1 "${phase}" "gpu name" "${gpu_name}" "${lspci_name}" "lspci -mm (pci.ids)" \
    "$(grep -qE '^(Device [0-9a-f]{4}|AMD GPU|GPU)$' <<<"${gpu_name}" && echo fail || echo pass)" \
    "never a bare device id"
  local amd_hwmon
  amd_hwmon="$(sensors -j 2>/dev/null | jq -c '[to_entries[] | select(.key|startswith("amdgpu"))][0].value // empty')"
  if [ -n "${amd_hwmon}" ]; then
    for pair in "edge:temperature_edge_celsius" "junction:temperature_junction_celsius" "mem:temperature_memory_celsius"; do
      local label="${pair%%:*}" field="${pair##*:}" theirs ours
      theirs="$(jq -r --arg l "${label}" '.[$l] | to_entries[] | select(.key|endswith("_input")) | .value' <<<"${amd_hwmon}")"
      ours="$(jq -r --arg f "${field}" '.gpu[$f] // "absent"' <<<"${snap}")"
      record G4 "${phase}" "gpu ${label} temperature (°C)" "${ours}" "${theirs}" "sensors -j amdgpu ${label}" \
        "$(within "${ours}" "${theirs}" 4)" "label-matched, ±4 °C"
    done
    local theirs_power ours_power
    theirs_power="$(jq -r '[.. | objects | to_entries[] | select(.key|test("^power1_(average|input)$")) | .value][0] // empty' <<<"${amd_hwmon}")"
    ours_power="$(jq -r '.gpu.power_draw_watts // "absent"' <<<"${snap}")"
    [ -n "${theirs_power}" ] && record G5 "${phase}" "gpu power (W)" "${ours_power}" "${theirs_power}" \
      "sensors -j amdgpu power1" "$(within "${ours_power}" "${theirs_power}" 25)" "±25 W: power moves fast"
    local theirs_fan ours_fan
    theirs_fan="$(jq -r '[.. | objects | to_entries[] | select(.key=="fan1_input") | .value][0] // empty' <<<"${amd_hwmon}")"
    ours_fan="$(jq -r '.gpu.fan_rpm // "absent"' <<<"${snap}")"
    [ -n "${theirs_fan}" ] && record G2 "${phase}" "gpu fan (rpm)" "${ours_fan}" "${theirs_fan}" \
      "sensors -j amdgpu fan1" "$(within "${ours_fan}" "${theirs_fan}" 300)" "±300 rpm"
  else
    skip G4 "${phase}" "gpu sensors" "sensors" "lm-sensors"
  fi
  local card vram_used vram_ours
  card="$(dirname "$(dirname "$(ls -d /sys/class/drm/card*/device/mem_info_vram_used 2>/dev/null | head -1)")")"
  if [ -n "${card}" ] && [ "${card}" != "." ]; then
    vram_used="$(cat "${card}/device/mem_info_vram_used")"
    vram_ours="$(jq -r .gpu.vram_used_bytes <<<"${snap}")"
    record G3 "${phase}" "gpu vram used (bytes)" "${vram_ours}" "${vram_used}" "sysfs mem_info_vram_used" \
      "$(within "${vram_ours}" "${vram_used}" $((256 * 1024 * 1024)))" "±256 MiB"
  fi

  # ── sensors ────────────────────────────────────────────────────────
  local dupes
  dupes="$(jq -r '[.sensors.chips[] as $c | $c.temps[] | "\($c.name)|\($c.device // "")|\(.label)"] | group_by(.) | map(select(length>1)) | length' <<<"${snap}")"
  record S1 "${phase}" "sensor rows with identical identity" "${dupes}" "0" "jq over probe" \
    "$([ "${dupes}" = 0 ] && echo pass || echo fail)"
  local sensors_temps ours_temps
  sensors_temps="$(sensors -j 2>/dev/null | jq '[.. | objects | to_entries[] | select(.key|test("^temp[0-9]+_input$"))] | length')"
  ours_temps="$(jq '[.sensors.chips[].temps[]] | length' <<<"${snap}")"
  record S0 "${phase}" "temperature channel census" "${ours_temps}" "${sensors_temps}" "sensors -j temp*_input" \
    "$([ "${ours_temps}" = "${sensors_temps}" ] && echo pass || echo fail)" "every readable channel, none invented"
  local bogus_visible
  # `.plausible != false`, not `(.plausible // true)`: jq's `//`
  # treats false as missing, so the latter reads every marked row as
  # unmarked.
  bogus_visible="$(jq '[.sensors.chips[].temps[] | select((.celsius < -30 or .celsius == 0) and .plausible != false)] | length' <<<"${snap}")"
  record S2 "${phase}" "implausible readings marked" "${bogus_visible} unmarked" "0" "jq over probe" \
    "$([ "${bogus_visible}" = 0 ] && echo pass || echo fail)" "−62 °C / 0 °C must carry plausible:false"

  # ── disks ──────────────────────────────────────────────────────────
  local ours_mounts real_mounts
  ours_mounts="$(jq -r '.disks[].mount_point' <<<"${snap}" | LC_ALL=C sort -u | paste -sd, -)"
  real_mounts="$(findmnt -rn -o SOURCE,TARGET | awk '$1 ~ "^/dev/" && $1 !~ "/loop" {print $2}' \
    | sed 's/\\x20/ /g' | LC_ALL=C sort -u | paste -sd, -)"
  record D1 "${phase}" "mounted block filesystems" "${ours_mounts}" "${real_mounts}" "findmnt /dev/*" \
    "$([ "${ours_mounts}" = "${real_mounts}" ] && echo pass || echo fail)"
  local root_used df_used
  root_used="$(jq -r '.disks[] | select(.mount_point=="/") | .used_bytes' <<<"${snap}")"
  df_used="$(df -B1 --output=used / | tail -1 | tr -d ' ')"
  record D2 "${phase}" "/ used (bytes)" "${root_used}" "${df_used}" "df -B1" \
    "$(within "${root_used}" "${df_used}" $((64 * 1024 * 1024)))" "±64 MiB of writes"

  # ── processes ──────────────────────────────────────────────────────
  local ours_count ps_count
  ours_count="$(jq '.processes | length' <<<"${snap}")"
  ps_count="$(ps -e --no-headers | wc -l)"
  record P0 "${phase}" "process census" "${ours_count}" "${ps_count}" "ps -e" \
    "$(within "${ours_count}" "${ps_count}" 50)" "±50 spawn churn"
  local generic offenders
  generic="$(jq '[.processes[] | select((.display_name // .name)|test("^(MainThread|Main Thread|GMainThread|node|python3?(\\.[0-9]+)?)$"))
    | select(.command_line != "")] | length' <<<"${snap}")"
  offenders="$(jq -r '[.processes[] | select((.display_name // .name)|test("^(MainThread|Main Thread|GMainThread|node|python3?(\\.[0-9]+)?)$"))
    | select(.command_line != "") | "\(.pid): \(.command_line[0:120])"] | join(" | ")' <<<"${snap}")"
  record P2 "${phase}" "processes shown by a thread or bare interpreter name" "${generic}" "0" "jq over probe" \
    "$([ "${generic}" = 0 ] && echo pass || echo fail)" \
    "comm 'MainThread' / 'python3' must resolve to the program${offenders:+; offenders: ${offenders}}"

  # ── network ────────────────────────────────────────────────────────
  local ours_rx dev_rx
  ours_rx="$(jq -r .network.total_received_bytes <<<"${snap}")"
  dev_rx="$(for i in /sys/class/net/*; do [ -e "$i/device" ] && cat "$i/statistics/rx_bytes"; done | awk '{s+=$1} END {print s+0}')"
  record N2 "${phase}" "received total, physical (bytes)" "${ours_rx}" "${dev_rx}" "/sys/class/net/*/statistics" \
    "$(within "${ours_rx}" "${dev_rx}" $((16 * 1024 * 1024)))" "±16 MiB in flight"
  record N1 "${phase}" "per-process source" "$(jq -r .network.process_source <<<"${snap}")" \
    "$(have nethogs && getcap "$(command -v nethogs)" | grep -q cap_net_raw && echo 'nethogs (capable)' || echo 'no nethogs')" \
    "getcap nethogs" "info" "probe is one-shot (tcp_diag); GUI/serve use nethogs when capable"
}

audit_phase idle

if have stress-ng; then
  stress-ng --cpu 8 --vm 2 --vm-bytes 2G --timeout 25s >/dev/null 2>&1 &
  stress_pid=$!
  sleep 4
  audit_phase load
  # Every "load" row must have been measured under load: if anything in
  # the phase waited on stress-ng (a bare `wait` does), the rows after
  # it were idle readings wearing a load label.
  load_alive="$(kill -0 "${stress_pid}" 2>/dev/null && echo running || echo exited)"
  record L1 load "stress-ng still running when the load phase ends" "${load_alive}" "running" "kill -0" \
    "$([ "${load_alive}" = running ] && echo pass || echo fail)" "no load row is an idle reading"
  wait "${stress_pid}" 2>/dev/null
else
  skip LOAD load "load phase" "stress-ng" "stress-ng"
fi

# ── disk + per-process IO (D3–D6, P6b) ───────────────────────────────
# A known writer: fio, direct I/O (no page cache, so the bytes hit the
# device inside the window), bounded by size and time, in the audit's
# own output dir; fio unlinks its file when done. Our partition row and
# the writer's process row are compared with iostat (same partition,
# same window) and pidstat -d (same pid, same window).
if have iostat && have pidstat && have fio; then
  io_dir="${out}/io-scratch"
  mkdir -p "${io_dir}"
  io_partition="$(basename "$(readlink -f "$(df --output=source "${io_dir}" | tail -1)")")"
  io_mount="$(df --output=target "${io_dir}" | tail -1)"
  # 8 s at a fixed 64 MB/s: steady across both 2 s windows, and gentle
  # enough for spinning rust.
  fio --name=sysmon-audit --directory="${io_dir}" --rw=write --bs=1M --size=1G \
    --direct=1 --rate=64m --time_based --runtime=8 --unlink=1 \
    --output=/dev/null >/dev/null 2>&1 &
  fio_parent=$!
  sleep 1.5
  # fio forks a worker; the worker is the one doing the I/O.
  writer_pid="$(pgrep -P "${fio_parent}" -n fio || echo "${fio_parent}")"
  io_file="$(mktemp)"
  ("${bin}" tap disks processes -i 2 2>/dev/null | head -2 | tail -1 >"${io_file}") &
  tap_pid=$!
  iostat_line="$(LC_ALL=C iostat -dxyk "${io_partition}" 2 1 | awk -v d="${io_partition}" '$1==d {print}')"
  pidstat_wr="$(LC_ALL=C pidstat -d -p "${writer_pid}" 2 1 2>/dev/null | awk '/^Average:/ && $3 ~ /^[0-9]+$/ {print $5}')"
  wait "${tap_pid}" 2>/dev/null
  wait "${fio_parent}" 2>/dev/null
  rmdir "${io_dir}" 2>/dev/null || true
  if [ -s "${io_file}" ] && [ -n "${iostat_line}" ]; then
    # iostat -x columns by header name (the order moved between sysstat versions).
    hdr="$(LC_ALL=C iostat -dxyk "${io_partition}" 1 1 | awk '/^Device/ {print; exit}')"
    io_wkb="$(awk -v h="${hdr}" -v l="${iostat_line}" 'BEGIN {n=split(h,H); split(l,L); for (i=1;i<=n;i++) if (H[i]=="wkB/s") print L[i]}')"
    io_util="$(awk -v h="${hdr}" -v l="${iostat_line}" 'BEGIN {n=split(h,H); split(l,L); for (i=1;i<=n;i++) if (H[i]=="%util") print L[i]}')"
    ours_wbps="$(jq -r --arg m "${io_mount}" '.disks[] | select(.mount_point==$m) | .write_bps' "${io_file}")"
    ours_util="$(jq -r --arg m "${io_mount}" '.disks[] | select(.mount_point==$m) | .util_percent' "${io_file}")"
    ours_proc_w="$(jq -r --argjson p "${writer_pid}" '.processes[] | select(.pid==$p) | .disk_write_bps // "absent"' "${io_file}")"
    auth_bps="$(awk -v k="${io_wkb}" 'BEGIN {printf "%.0f", k*1024}')"
    record D3 io "partition write rate (B/s)" "${ours_wbps}" "${auth_bps}" "iostat -dxyk ${io_partition} 2 1 wkB/s" \
      "$(within "${ours_wbps}" "${auth_bps}" "$(awk -v a="${auth_bps}" 'BEGIN {print a*0.25 + 1048576}')")" \
      "fio direct-I/O writer at 64 MB/s, concurrent 2 s windows; ±25% + 1 MiB/s"
    record D5 io "partition util %" "${ours_util}" "${io_util}" "iostat %util (same partition)" \
      "$(within "${ours_util}" "${io_util}" 20)" "partition io_ticks share; ±20 pp across two windows"
    if [ -n "${pidstat_wr}" ]; then
      auth_proc="$(awk -v k="${pidstat_wr}" 'BEGIN {printf "%.0f", k*1024}')"
      record P6b io "per-process disk write (B/s)" "${ours_proc_w}" "${auth_proc}" "pidstat -d -p <fio writer> kB_wr/s" \
        "$([ "${ours_proc_w}" = absent ] && echo fail || within "${ours_proc_w}" "${auth_proc}" "$(awk -v a="${auth_proc}" 'BEGIN {print a*0.25 + 1048576}')")" \
        "/proc/pid/io write_bytes, not wchar; ±25% + 1 MiB/s"
    else
      skip P6b io "per-process disk write" "pidstat gave no row for the writer" "sysstat"
    fi
  else
    skip D3 io "disk IO" "iostat/tap produced no row" "sysstat"
  fi
  rm -f "${io_file}"
else
  skip D3 io "disk + per-process IO" "iostat, pidstat, fio" "sysstat fio"
fi

failed="$(jq -s '[.[] | select(.verdict=="fail")] | length' "${results}")"
passed="$(jq -s '[.[] | select(.verdict=="pass")] | length' "${results}")"
jq -s --arg version "$("${bin}" --version)" --arg host "$(hostname)" \
  --arg kernel "$(uname -r)" --arg ts "$(date --iso-8601=seconds)" \
  --argjson failed "${failed}" --argjson passed "${passed}" \
  '{status: (if $failed == 0 then "ok" else "error" end), tool: "sysmon-accuracy-audit",
    version: $version, ts: $ts,
    result: {host: $host, kernel: $kernel, passed: $passed, failed: $failed, checks: .}}' \
  "${results}" >"${out}/report.json"

{
  echo "# SysMon accuracy audit"
  echo
  echo "$("${bin}" --version) · $(hostname) · kernel $(uname -r) · $(date --iso-8601=seconds)"
  echo
  echo "**${passed} pass · ${failed} fail**"
  echo
  echo "| id | phase | what | sysmon | authority | source | verdict | note |"
  echo "|---|---|---|---|---|---|---|---|"
  jq -r '"| \(.id) | \(.phase) | \(.what) | \(.ours) | \(.authority) | \(.source) | \(.verdict) | \(.note) |"' "${results}"
} >"${out}/report.md"

cat "${out}/report.md"
[ "${failed}" -eq 0 ] && exit 0 || exit 4
