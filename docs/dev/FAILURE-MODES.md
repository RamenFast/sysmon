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

## CPU

| # | way it could lie | caught by |
|---|---|---|
| C1 | headline frequency = mean over all 32 threads incl. parked ones, so it understates what busy cores run at | audit: turbostat Bzy_MHz vs our busiest-core and mean |
| C1a | the "busy clock" (busy-weighted `scaling_cur_freq`) is one instant read per core, so a core busy 60% of the window but idle at the instant of the read reports its idle clock. Measured 2026-10-01 vs CPPC delivered/reference over the same 2 s windows: 572–2100 MHz low; audit C1 failed both phases (3074 vs 3929, 2213 vs 3868) | the window's true delivered clock from ACPI CPPC `feedback_ctrs` (delivered/reference × nominal_freq, unprivileged, the same APERF/MPERF ratio turbostat reads). Reading it costs ~0.6 ms per core (a firmware mailbox, every read). Read cost bounded by reading it on a background poller thread like the GPU clock, never on the sampling path |
| C1b | CPPC counters wrap (`wraparound_time`), or a core goes offline between reads, or the firmware doesn't expose CPPC (Intel, VMs) | wrap: delivered < previous → drop that core for the window; missing file → fall back to the instant read, labelled `frequency_busy_source: "instant read"` |
| C2 | per-core % double-counts guest time | existing pinned-spinner test |
| C3 | Tctl reported as die temperature (Zen offset) | audit: k10temp label set; Zen3 has Tctl == Tdie, older Zen had +10/+20 °C |
| C4 | °F conversion rounding drifts from °C reading | unit test `c_to_f` exact points (0, 37, 100, -40) |
| C5 | load average mislabeled as % | visual review |
| C6 | min/max range taken from scaling_ (governor) instead of cpuinfo_ (hardware) | audit vs lscpu |

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
