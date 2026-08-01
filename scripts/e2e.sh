#!/usr/bin/env bash
# End-to-end receipts on a private Xvfb + openbox: boots the GUI,
# walks it with `sysmon ctl`, screenshots every theme and page, tests
# the pop-out + drag-dock gesture and single-instance forward, and
# checks the agent contract with jq. Never touches the real display
# or the real settings file.
#
#   scripts/e2e.sh [output-dir] [path-to-sysmon-binary]
#
# Screenshots land in <output-dir> (default: ./e2e-out). Exits nonzero
# on the first failed check.
set -euo pipefail

out="${1:-e2e-out}"
bin="${2:-target/release/sysmon}"
display=":83"
config="$(mktemp -d)"
runtime="$(mktemp -d /tmp/sysmon-e2e-rt.XXXX)"
mkdir -p "$out"
bin="$(realpath "$bin")"

say()  { printf '\033[36m» %s\033[0m\n' "$*"; }
pass() { printf '\033[32m✓ %s\033[0m\n' "$*"; }
fail() { printf '\033[31m✗ %s\033[0m\n' "$*"; exit 1; }

cleanup() {
  XDG_RUNTIME_DIR="$runtime" "$bin" ctl quit >/dev/null 2>&1 || true
  [ -n "${wm_pid:-}" ] && kill "$wm_pid" 2>/dev/null || true
  [ -n "${xvfb_pid:-}" ] && kill "$xvfb_pid" 2>/dev/null || true
  rm -rf "$config" "$runtime"
}
trap cleanup EXIT

say "virtual display + window manager"
Xvfb "$display" -screen 0 1280x900x24 >/dev/null 2>&1 &
xvfb_pid=$!
sleep 1
DISPLAY=$display openbox >/dev/null 2>&1 &
wm_pid=$!
sleep 1

say "launch (scratch config, scratch socket)"
DISPLAY=$display XDG_CONFIG_HOME="$config" XDG_RUNTIME_DIR="$runtime" \
  "$bin" >"$out/gui.log" 2>&1 &
sleep 7
export XDG_RUNTIME_DIR="$runtime"

"$bin" ctl status | jq -e '.result.mode == "gui"' >/dev/null || fail "ctl status"
pass "gui owns the socket"

say "screenshot every theme × both pages"
for theme in blossom_dark blossom amoled light dark funky paper basalt amber chromacore greyscale; do
  "$bin" ctl theme "$theme" >/dev/null
  for page in overview processes; do
    "$bin" ctl page "$page" >/dev/null
    sleep 1.2
    shot=$("$bin" ctl shot | jq -r .result.path)
    [ -s "$shot" ] || fail "shot $theme/$page"
    cp "$shot" "$out/$theme-$page.png"
  done
done
pass "22 theme/page screenshots"

say "pop-out + drag-dock"
"$bin" ctl theme blossom_dark >/dev/null
"$bin" ctl page overview >/dev/null
"$bin" ctl popout gpu | jq -e '.result.popped_out == true' >/dev/null || fail "popout verb"
sleep 2.5
pop_win=$(DISPLAY=$display xdotool search --name "GPU" | head -1)
main_win=$(DISPLAY=$display xdotool search --name "^SysMon$" | head -1)
[ -n "$pop_win" ] || fail "pop-out window exists"
DISPLAY=$display import -window root "$out/popout.png"
eval "$(DISPLAY=$display xdotool getwindowgeometry --shell "$main_win" | sed 's/^/m_/')"
eval "$(DISPLAY=$display xdotool getwindowgeometry --shell "$pop_win" | sed 's/^/p_/')"
DISPLAY=$display xdotool mousemove 1150 850 mousedown 1
sleep 0.3
tx=$((m_X + m_WIDTH / 2 - p_WIDTH / 2)); ty=$((m_Y + m_HEIGHT / 2 - p_HEIGHT / 2))
for i in 1 2 3 4 5 6; do
  DISPLAY=$display xdotool windowmove "$pop_win" $((p_X + (tx - p_X) * i / 6)) $((p_Y + (ty - p_Y) * i / 6))
  sleep 0.25
done
sleep 0.5
DISPLAY=$display xdotool mouseup 1
sleep 2.5
[ -z "$(DISPLAY=$display xdotool search --name 'GPU' | head -1)" ] || fail "drag-dock"
pass "pop-out docked on drop"

say "single instance + agent surface"
DISPLAY=$display XDG_CONFIG_HOME="$config" timeout 10 "$bin" | grep -q "raised" || fail "second launch raise"
pass "second launch raised, exit 0"
"$bin" probe network | jq -e '.result.via == "socket"' >/dev/null || fail "probe via socket"
timeout 5 "$bin" tap cpu --interval 1 | head -2 | tail -1 | jq -e '.cpu.core_count > 0' >/dev/null || fail "tap stream"
"$bin" probe nonsense >/dev/null 2>&1 && fail "bad section must fail" || [ $? -eq 3 ] || fail "bad section exit code"
pass "probe/tap/exit-codes"

say "quit"
"$bin" ctl quit >/dev/null
sleep 1
[ ! -S "$runtime/sysmon/ctl.sock" ] || fail "socket removed on quit"
pass "clean shutdown"

echo
pass "e2e complete — receipts in $out/"
