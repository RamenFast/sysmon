sysmon(1)

# NAME

sysmon - compact system monitor with an agent-drivable API

# SYNOPSIS

*sysmon*++
*sysmon* --background++
*sysmon* probe [_sections_...] [--json]++
*sysmon* tap [_sections_...] [--interval _N_]++
*sysmon* ctl _verb_ [_args_...]++
*sysmon* serve [--record [--interval _N_]]++
*sysmon* specs [_section_] [--json]++
*sysmon* history [_section_] [--since _D_] [--until _D_] [--specs] [--records] [--json]++
*sysmon* history --info | --export _PATH_ | --clear --yes | --pack++
*sysmon* schema

# DESCRIPTION

SysMon watches an AMD-GPU Linux desktop: GPU (and an NVIDIA card
beside it, read through NVML), memory, CPU, network, disks, and sensors as live cards with history graphs, per-process
top lists with real application icons, and a full sortable process
table with end/kill/renice actions. Any card pops out into its own
always-on-top window; drag it back onto the main window to dock it.

Every reading the window shows is also one command away: the same
engine answers a JSON API over the CLI and a control socket, so
scripts, bars, and agents can ask about *any* system state —
networking first — without scraping a UI.

# COMMANDS

*sysmon*
	Launch the GUI (the default command). If a same-version instance
	is already running, raise it and exit 0; an older instance (left
	up across an upgrade) is asked to quit and replaced.

*sysmon --background*
	Run the GUI on a private Xvfb display: it renders and serves the
	control socket without ever mapping a window on your screen.
	Needs _xvfb-run_.

*probe* [_sections_...] [--json]
	One-shot snapshot as a JSON envelope (JSON automatically when
	piped; human summary on a terminal). Asks the live instance when
	one is running; otherwise samples in-process, twice, 250 ms
	apart, so rates are real. Sections: *all system cpu memory gpu
	nvidia network disks processes sensors connections*. *nvidia* is
	the first NVIDIA card through libnvidia-ml (opened at run time;
	_available_ is false where there is none).

*tap* [_sections_...] [--interval _N_]
	Stream snapshots as NDJSON, one per line, at the given cadence
	(default: the *network* section, every 1 s). The intended diet
	for desktop bars.

*ctl* _verb_ [_args_...]
	Drive the running instance: *status quit pause resume raise*;
	*interval* _seconds_; *page* overview|processes; *theme* _id_;
	*palette* _id_; *popout*/*popin* _section_; *shot* [_path_]
	(writes a PNG of the window and prints its path); *compact*
	on|off; *units* decimal|binary; *temperature*
	celsius|fahrenheit|both; *cpugraph* total|threads|grid (the
	Overview's CPU card: one trace, a coloured line per thread, or a
	small graph per thread). A GUI's *status* names the
	adapter the window renders on (_renderer_); a CPU-rasterizer
	fallback — no usable Vulkan driver — adds _renderer_hint_ with
	the fix, and the window says so at startup too. Degraded modes
	are disclosed, never silent.

*serve* [--record [--interval _N_]]
	Headless sampling daemon on the control socket. Samples only
	when a client asks — an idle daemon does nothing at all. With
	*--record* it also keeps the usage history (see *history*) at its
	own cadence, for a machine where the window is not open. One
	process records at a time; a second exits 2 and names the first.

*specs* [_section_] [--json]
	What this machine is, rather than what it is doing: OS, desktop,
	board and firmware; CPU topology, clocks, caches and instruction
	sets; memory modules; each GPU (a compute-only NVIDIA card too,
	its details from NVML) with its architecture, PCIe link,
	power cap and driver stack; displays; every physical drive;
	network; audio. On a terminal it prints the plain-text sheet the
	GUI's copy buttons copy, which leaves out the hostname, serials,
	addresses and mount paths. Piped, the envelope with every field
	and the sheet as _result.text_. Sections: *system cpu memory
	graphics storage network audio*. Needs no running instance; the
	optional helpers it asks (vulkaninfo, glxinfo, swaymsg, pactl)
	each run under a 3 s deadline.

*history* [_section_] [--since _D_] [--until _D_] [--specs] [--records] [--blocks _N_] [--raw] [--json]
	What the machine did over a time range, from the usage history
	SysMon keeps on disk while its window (or *serve --record*) runs,
	the hourly files and the packed archive read as one. Per reading:
	min, avg, p95, max and a timeline; for each GPU, every program that
	held it with its peak and average VRAM and busy share. Times read
	as _90s_, _15m_, _2h_, _7d_, _1y_, plain seconds, or a unix time;
	*--since* defaults to 1 h. *--specs* puts the section's specs above
	the usage, exactly what its pop-out's copy button copies.
	*--records* prints every raw record in the range as NDJSON instead.
	*--raw* reads every record instead of merging sealed hours from
	their rollups (same numbers; a check). A timeline block with no
	samples is blank (JSON: null), never zero. Sections: *all cpu
	memory gpu nvidia network disks sensors*. Exits 2 when no history exists
	yet.

*history* --info | --export _PATH_ [--force] | --clear --yes | --pack
	The store itself. *--info*: size per tier, how far back it goes,
	and its growth per month and per year at the cadence being
	recorded. *--export*: the whole history as one _.tar.zst_ (a file,
	or a directory for _sysmon-history-<date>.tar.zst_); an existing
	file is replaced only with *--force*. Restore with *tar --zstd -xf*
	_FILE_ *-C ~/.local/state/sysmon* after stopping the recorder.
	*--clear --yes*: delete the whole history (refused without
	*--yes*); only the store's own files go, nothing outside its
	directory. *--pack*: pack every sealed hour into the archive now.
	When a recorder holds the history, *--clear* and *--pack* ask it to
	act, between two samples.

*schema*
	The machine-readable map of every command, section, field, and
	the envelope/exit-code contract — strict, generated from the
	binary, and tested against it. An agent with no prior knowledge
	of sysmon goes from *schema* to a correct call.

# API CONTRACT

Every one-shot reply is one JSON envelope:
_{"status","tool","version","ts","result"|"error"+"fix"+"exit"}_ —
errors always carry a *fix* and the exit code the failure means.
_ts_ is ISO-8601 with a UTC offset; _ts_epoch_ carries the same
instant as a number. Exit codes: 0 ok, 2 unavailable, 3 bad
arguments, 4 runtime failure. Every NDJSON stream line carries
_event_. The control socket lives at
_$XDG_RUNTIME_DIR/sysmon/ctl.sock_ (one JSON object per line in, one
envelope out; *subscribe* upgrades to a raw snapshot stream).

# PER-PROCESS NETWORK

Native attribution uses netlink sock_diag TCP byte counters — no
setup, no privileges, own-user sockets. Installing *nethogs*(8) with
packet-capture capability upgrades it to all protocols (UDP/QUIC)
and all users:

	sudo apt install nethogs++
sudo setcap 'cap_net_admin,cap_net_raw+ep' $(which nethogs)

The API and the Network card state which source fed them.

# FILES

_~/.config/sysmon/settings.json_
	Preferences (v1 files are read and migrated in place).

_~/.local/state/sysmon/history/_ (_$XDG_STATE_HOME_)
	Usage history. The last 7 days as one file per UTC hour (gzipped
	when the hour ends, ~140 bytes a sample) with a small rollup each;
	older days packed losslessly into _archive/<day>.jsonl.zst_ (~100
	bytes a sample, readable with *zstd -dc*). Nothing is deleted by
	age or size unless a keep or a size cap is set in Display options
	→ Usage history. _.lock_ names the one writer.

# EXAMPLES

Network rate for a bar, every 2 s:
	sysmon tap network --interval 2 | jq --unbuffered '.network.download_bps'

Who is using the network right now:
	sysmon probe network --json | jq '.result.network.top_processes'

Screenshot the running window:
	sysmon ctl shot /tmp/sysmon.png

A spec sheet for a forum post (piped output is JSON, so take the text):
	sysmon specs --json | jq -j .result.text | wl-copy

How much GPU and VRAM each program used over the last two hours:
	sysmon history gpu --since 2h

The same as the GPU pop-out's copy button, to paste somewhere:
	sysmon history gpu --specs --json | jq -j .result.text | wl-copy

How big the history is and how fast it grows; then a backup:
	sysmon history --info
	sysmon history --export ~/sysmon-history.tar.zst

# AUTHORS

Ben Miller, with Claude (Anthropic). GPL-3.0-or-later.
