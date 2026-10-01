# Failure modes — written before the 3.1 audit and the isolated tests

*Ben's law (AGENTS.md brain-dump): "If you must test a system in
isolation, first write down all the ways it could fail, then write the
code." This file is that list for the 3.1 round. Every line names how
the number could lie and which check catches it. `scripts/accuracy-audit.sh`
and `tests/accuracy.rs` are the checks.*

## Memory

| # | way it could lie | caught by |
|---|---|---|
| M1 | `used` uses a definition no reader expects (htop's total−free−buff−cache vs ours total−available) and looks "wrong" next to another tool | audit prints both identities, card labels which one is shown |
| M2 | "Total" reads as installed RAM when it is MemTotal (kernel-usable, minus firmware/GPU/kernel reservations): 135.0 GB vs 137.4 GB of sticks | audit: DMI DIMM sum vs memmap System RAM vs MemTotal; card shows all three |
| M3 | GB vs GiB mislabel (dividing by 1024 and printing "GB") | unit test on `format_size` boundaries + audit compares to `free --si` |
| M4 | kB in /proc/meminfo is KiB (×1024), easy to treat as ×1000 | existing identity test (`total == free -b` to the byte) |
| M5 | DIMM speed shown is the rated (XMP) speed, not the configured speed | audit compares `CONFIGURED_SPEED_MTS` to what the card shows |
| M6 | swap "none" when zram/zswap exists | audit reads /proc/swaps |
| M7 | commit charge read from a line that isn't Committed_AS (Committed_AS vs CommitLimit swapped, or VmallocTotal mistaken for a limit) | unit test on the meminfo fixture; audit M7 vs /proc/meminfo read in the same second |
| M8 | kernel memory (Slab, PageTables, KernelStack) parsed off by the kB unit, or Slab counted twice (SReclaimable is inside Slab and already inside `cached_bytes`) | unit test on the fixture: slab = Slab line exactly; audit M8 vs /proc/meminfo |

## CPU

| # | way it could lie | caught by |
|---|---|---|
| C1 | headline frequency = mean over all 32 threads incl. parked ones, so it understates what busy cores run at | audit: turbostat Bzy_MHz vs our busiest-core and mean |
| C1a | the "busy clock" (busy-weighted `scaling_cur_freq`) is one instant read per core, so a core busy 60% of the window but idle at the instant of the read reports its idle clock. Measured 2026-10-01 vs CPPC delivered/reference over the same 2 s windows: 572–2100 MHz low; audit C1 failed both phases (3074 vs 3929, 2213 vs 3868) | the window's true delivered clock from ACPI CPPC `feedback_ctrs` (delivered/reference × nominal_freq, unprivileged, the same APERF/MPERF ratio turbostat reads). Reading it costs ~0.6 ms per core (a firmware mailbox, every read). Read cost bounded by reading it on a background poller thread like the GPU clock, never on the sampling path |
| C1b | CPPC counters wrap (`wraparound_time`), or a core goes offline between reads, or the firmware doesn't expose CPPC (Intel, VMs) | wrap: delivered < previous → drop that core for the window; missing file → fall back to the instant read, labelled `frequency_busy_source: "instant read"` |
| C1c | the CPPC edge is read off the sampling path and used one sample later, so the "window" is the *previous* window (or, in a probe, a ~25 ms slice at prime time) weighed by this window's busy %. Reviewer B: after an all-core window, a 1-core window reported 3943 MHz where CPPC said 4357. Constant-load tests can't see it | red test alternates the load between consecutive windows (half the cores, then one); the edge is read in the sample that uses it |
| C2 | per-core % double-counts guest time | existing pinned-spinner test |
| C3 | Tctl reported as die temperature (Zen offset) | audit: k10temp label set; Zen3 has Tctl == Tdie, older Zen had +10/+20 °C |
| C4 | °F conversion rounding drifts from °C reading | unit test `c_to_f` exact points (0, 37, 100, -40) |
| C5 | load average mislabeled as % | visual review |
| C6 | min/max range taken from scaling_ (governor) instead of cpuinfo_ (hardware) | audit vs lscpu |
| C7 | kernel time % counts iowait or idle, or guest time twice, so it can exceed the busy % it is drawn inside | unit test: system + irq + softirq only, and kernel ≤ busy on every tick pair; audit C7 vs `mpstat` %sys + %irq + %soft over the same window |

## GPU

| # | way it could lie | caught by |
|---|---|---|
| G1 | device name "Device 7551" from a stale pci.ids | accuracy test: name never matches `^Device [0-9a-f]{4}$` |
| G2 | fan unreadable rendered "0 rpm" (looks like a stopped fan) | card shows "—" when `fan_rpm` is None |
| G3 | VRAM/GTT numbers in wrong unit | existing sysfs re-read test |
| G4 | junction/mem temps on the wrong channel (temp2/temp3 order differs per ASIC) | read `temp*_label` instead of trusting index |
| G5 | power average vs input confusion (RDNA4 exposes only `power1_average` or `_input`) | audit vs `sensors -j` |
| G6 | per-process busy % > 100 or double-counting dup'd fds | existing client-id dedupe + audit vs amdgpu_top |
| G7 | core clock reported while GPU is idle-gated (freq file stale) | audit vs amdgpu_top |
| G8 | clock from one instantaneous read: jumps 0..1760 MHz between reads, can print "0 MHz" (audit F15). Measured 2026-10-01: on Navi 48 gpu_metrics' `average_gfxclk` equals `current_gfxclk` and changes every few ms, and hwmon `freq1_input` is the driver's decode of the same field, so switching source fixes nothing | the clock is the **mean of ~10 reads/s across the sample window** (a 10 Hz poller that only runs while the GPU is being sampled); accuracy test compares it to an independent 100 Hz poll of the same window, tolerance from the two estimators' standard error |
| G9 | `gpu_metrics` parsed with the wrong layout (format/content revision differs per ASIC; v1.0 puts `system_clock_counter` first) | parser keyed on (format, content) revision, unknown revisions → None, never a guess; fixture tests per layout |
| G10 | unsupported fields read as 65535 MHz / 655 °C (the firmware's 0xFFFF "not supported") | 0xFFFF → None; fixture test |
| G11 | short read / truncated blob taken as zeros | length checked against `structure_size` and the field's offset |
| G12 | VR temps (vrgfx/vrsoc/vrmem) exist but are never shown — hot VRMs invisible | surfaced as GPU sensor readings when supported |
| G13 | a faster DRM-client scan that trusts fd *link text* misses clients opened via another path (bind-mounted /dev in flatpak/containers) | not taken: detection stays `stat` → char major 226, the kernel's own answer. Measured 2026-10-01 the fd walk is 6 ms median, p90 16 ms on refresh samples; the remaining cost is syscalls a link-text shortcut would not remove |
| G14 | the negative cache hides a process that *starts* using the GPU (Ollama loading a model) for up to N samples | accepted, bounded: re-checked every 5 samples (10 s at the default 2 s). No live test yet: an idle render-node client shows 0% and no VRAM, so a faithful test needs real GPU work. SERIOUS-TODO |
| G15 | the cache is keyed by pid, so a recycled pid inherits "no GPU" | same 5-sample bound as G14; pid reuse inside 10 s is rare on a 4M pid_max. SERIOUS-TODO with G14 |

## Sensors

| # | way it could lie | caught by |
|---|---|---|
| S1 | multiple channels of one chip collapse to one label (nvme Composite/Sensor 1/2/8 all show the model) | accuracy test: every (chip, device, label) row is unique |
| S2 | disconnected thermistor shows absurd values (AUXTIN1 −62 °C, PCH 0 °C) | sanity filter with disclosure; audit lists filtered channels |
| S3 | `in*` channels lack labels on nct6798, so rails read "in0..in14" with no meaning | friendly names only where the board's mapping is known; otherwise the raw channel, never a guessed name |
| S4 | crit thresholds of 0 or absurd (nct6798 in* min/max = 0) treated as alarms | only temps use crit; voltage limits ignored |
| S5 | hottest headline picks a bogus sensor (SMBUSMASTER 0 72 °C is real; −62 is not) | headline excludes filtered channels and names its source |
| S6 | duplicate chips (amdgpu shown in both GPU and Sensors) | GPU card owns amdgpu; Sensors references it |

## Disks

| # | way it could lie | caught by |
|---|---|---|
| D1 | a mounted filesystem missing from the GUI but present on the wire | audit: GUI-visible set == probe set (fuseblk NTFS on nvme0n1p1) |
| D2 | used vs available confusion (df "used" vs root-reserved blocks) | existing df test |
| D3 | read/write rates per partition vs per whole disk | audit vs iostat |
| D4 | rates are per *partition* (what's mounted), iostat's default rows are per device: comparing / to `sdc` mixes /boot/efi in | audit compares against `iostat -dxyk <partition>`, the same partition row, same window |
| D5 | util% from a partition's own `io_ticks`: on this kernel a partition's ticks (sdc2 272 s) can exceed its parent device's (sdc 251 s), so partition util% is not the device's busy share | stated, not hidden: audit compares partition util% to iostat's partition `%util` (same counter), and the card's hover says "share of the window this partition had I/O in flight" |
| D6 | a write burst lands between our two reads and iostat's | the audit runs `fio --direct=1 --rate=64m` for 8 s across both windows, so both see a steady rate; tolerance 25% of the authority + 1 MiB/s |
| P6b | per-process disk read/write from `/proc/pid/io` misread (rchar/wchar are syscall bytes incl. page cache; read_bytes/write_bytes are storage) | audit: our rate for fio's writer process vs `pidstat -d` kB_wr/s for that pid, same window |
| L1 | the audit's "load" rows were measured after the load ended (a bare `wait` in a phase waits for the backgrounded `stress-ng` too, so every row after it is an idle reading under a load label; 3.1 runs 1–3 had this) | audit row L1: `stress-ng` must still be running when the load phase finishes; each concurrent window waits on its own pid |

## Processes

| # | way it could lie | caught by |
|---|---|---|
| P1 | RSS double-counts shared pages; users read RSS as "this app's RAM" | Inspector shows RSS + PSS + USS from smaps_rollup |
| P2 | thread name (comm "MainThread") hides the program (node) | name resolution: comm → exe basename when comm is a generic thread name |
| P3 | CPU % of a process that exits between samples | existing starttime keying |
| P4 | right-click acts on a different pid than the one clicked (live re-rank) | kittest that re-ranks between open and click |
| P5 | Overview row click opens the Inspector on the *slot's* pid after a re-rank, not the clicked one | `ui_bridge`: re-ranks between frames, asserts the Inspector pid |
| P6 | "all by memory" link lands on Processes sorted by something else (or ascending) | `ui_bridge`: asserts sort column + biggest-first after the link |
| P7 | summary-strip chip lands on the Overview but not on the card it named | `ui_bridge`: asserts page + one-shot scroll target consumed |
| P8 | group-by-app sums a different set than the filter shows (×N count wrong) | unit test on `group_by_app` + `ui_bridge` grouped row label |
| P9 | Inspector keeps a dead pid's history when the pid is recycled | unit test `a_recycled_pid_starts_a_fresh_history` (starttime keyed) |
| P10 | Inspector breadcrumb loops forever on a reparenting race | unit test `ancestry_survives_a_reparenting_loop` |
| P11 | the Inspector pane eats the table's click targets at narrow widths | `ui_kittest` header clicks at 430 px (the pane only docks ≥ 760 px) |
| P12 | group-by / inspector toggles don't survive a restart | `ui_bridge`: settings file read back after toggles |
| P13 | a clustered inline-code flag (`bash -lc "cd x && cargo build"`, `-ec`, `python3 -Bc`) is read as a script path, so the code becomes the name ("Dev && cargo build --release") | `display_names_say_what_a_person_would`: a short cluster carrying c/e means inline code; `-uB x.py` still names x.py |
| P14 | group-by-app keys on the executable, so five unrelated python3.12 programs become one "tray.py ×5" row with summed CPU, and End process signals only the root | `unrelated_scripts_on_one_interpreter_stay_apart`: interpreters are keyed by the script they run |

## Network

| # | way it could lie | caught by |
|---|---|---|
| N1 | per-process rates labeled as total when only TCP own-UID is covered | `process_source` disclosure; audit checks nethogs availability |
| N2 | virtual interfaces double-count WAN | existing physical-only identity |

## Presentation

| # | way it could lie | caught by |
|---|---|---|
| U1 | `probe` (human) shows °C while GUI shows °F | both read the same temperature setting... the CLI human view states °C and °F together |
| U2 | `tap` through a socket prints 0.800000011920929 for 0.8 | one serializer for every producer + contract test both ways |
| U3 | a stdout write fails for a reason other than a closed pipe (ENOSPC, EIO), the answer is lost, and the CLI exits 0 as if it arrived | `a_full_disk_is_not_a_clean_exit` (stdout = /dev/full): exit 4 with a fix on stderr; `a_closed_pipe_is_a_clean_exit` keeps EPIPE at 0 |

## Shared sampler windows (`sysmon serve`)

| # | way it could lie | caught by |
|---|---|---|
| W1 | one client keeps the sampler warm (`tap network`), so a second client's first `probe cpu processes` sees a "0.65 s window" and every rate in it is 0 (those collectors had never run) | each section records when it last ran; serve opens a fresh common window unless the wanted sections share one. Contract test `a_warm_serve_measures_each_section_over_its_own_window` (red: overall 0%, spinner 0%) |
| W2 | a section last asked for 8 s ago answers with its 8 s average under the 0.35 s label of whichever section ran most recently | `interval_seconds` is the oldest wanted section's window, and sections more than 10% apart get a fresh common window |
| W3 | the GPU clock poller parks after 5 s idle but keeps its sums, so the next window's "mean of N reads" covers only its first 5 s | a take after the idle limit reports no reads (clock_source falls back to "instant read"); unit test `a_window_after_a_park_is_not_a_partial_mean` (red: 15 reads). The parked thread waits on its condvar with no timer (`the_poller_shares_its_last_table_and_sleeps_when_parked`, red: no shared table) and the VRM temps reuse the poller's last table instead of a second SMU wake |

## Performance page (3.2, `docs/dev/PERFORMANCE-VIEW.md`)

| # | way it could lie | caught by |
|---|---|---|
| V1 | the LED meter shows one sample and the graph's newest point another (meter from the snapshot, graph from a history pushed a frame earlier or later) | `ui_kittest`: meter label equals the newest history sample equals the snapshot field, same frame |
| V2 | kernel time plotted above total busy (wrong tick columns, or kernel from one window and busy from another) | C7 unit test (kernel ≤ busy always); both come from the same `CpuSnapshot` |
| V3 | per-thread mini graphs drawn in a different order than `/proc/stat` (a HashMap, a sort by value), so "cpu7 is pegged" points at the wrong thread | `ui_kittest`: the grid's labels read 0..core_count in row-major order and each graph's newest sample equals `per_core_percent[i]` |
| V4 | a rate graph autoscales to a 2 GB/s spike and the 5 MB/s that follows reads as a flat zero line | the ceiling is written in the field's corner; unit test on the scale picker (ceiling ≥ observed max, label present) |
| V5 | the scrolling graticule drifts away from the data (grid advanced per frame instead of per sample) | the grid offset is derived from the history length, not from time or frames; unit test: offset(n) == offset(n+15) |
| V6 | the greyscale theme can't tell the two lines apart | the second series is dashed when `palette.id == "greyscale"`; screenshot review |
| V7 | the status bar or a group box clips a value at 430 px | `ui_kittest` at 430 px: every status/box value node is fully inside the panel rect |
| V8 | the page costs more CPU than the Overview it replaces as the startup page (32 mini graphs × 150 points per frame) | `scripts/perf.sh` A/B on the Performance page in both modes vs 3.1.0 Overview; one shape per series, no per-point allocations |
| V9 | the per-thread grid overflows or squashes to unreadable at some width (fixed column count) | columns follow width (`clamp(width/96, 2, 8)`), minimum graph height 28 px; `ui_kittest` at 430 and 1240 px asserts every mini graph rect is inside the panel and ≥ 28 px tall |
