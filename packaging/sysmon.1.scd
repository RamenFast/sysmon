sysmon(1)

# NAME

sysmon - compact system monitor with an agent-drivable API

# SYNOPSIS

*sysmon*++
*sysmon* --background++
*sysmon* probe [_sections_...] [--json]++
*sysmon* tap [_sections_...] [--interval _N_]++
*sysmon* ctl _verb_ [_args_...]++
*sysmon* serve++
*sysmon* schema

# DESCRIPTION

SysMon watches an AMD-GPU Linux desktop: GPU, memory, CPU, network,
disks, and sensors as live cards with history graphs, per-process
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
	network disks processes sensors connections*.

*tap* [_sections_...] [--interval _N_]
	Stream snapshots as NDJSON, one per line, at the given cadence
	(default: the *network* section, every 1 s). The intended diet
	for desktop bars.

*ctl* _verb_ [_args_...]
	Drive the running instance: *status quit pause resume raise*;
	*interval* _seconds_; *page* overview|processes; *theme* _id_;
	*palette* _id_; *popout*/*popin* _section_; *shot* [_path_]
	(writes a PNG of the window and prints its path); *compact*
	on|off; *units* decimal|binary.

*serve*
	Headless sampling daemon on the control socket. Samples only
	when a client asks — an idle daemon does nothing at all.

*schema*
	The machine-readable map of every command, section, field, and
	the envelope/exit-code contract.

# API CONTRACT

Every one-shot reply is one JSON envelope:
_{"status","tool","version","ts","result"|"error"+"fix"}_ — errors
always carry a *fix*. Exit codes: 0 ok, 2 unavailable, 3 bad
arguments, 4 runtime failure. The control socket lives at
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

# EXAMPLES

Network rate for a bar, every 2 s:
	sysmon tap network --interval 2 | jq --unbuffered '.network.download_bps'

Who is using the network right now:
	sysmon probe network --json | jq '.result.network.top_processes'

Screenshot the running window:
	sysmon ctl shot /tmp/sysmon.png

# AUTHORS

Ben Miller, with Claude (Anthropic). GPL-3.0-or-later.
