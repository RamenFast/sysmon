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
  # A run that failed mid-way may leave the GUI deaf to the socket;
  # never leave it behind on the private display.
  if [ -n "${gui_pid:-}" ]; then
    for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$gui_pid" 2>/dev/null || break; sleep 0.3; done
    kill "$gui_pid" 2>/dev/null || true
  fi
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
# WAYLAND_DISPLAY unset: this GUI belongs on the private X display.
# LD_LIBRARY_PATH unset: a custom compositor's (swayfx ships its own
# libxkbcommon) mixed with the system libxkbcommon-x11 segfaults the
# X11 path on quit — measured: exit 139 with it, 0 without.
env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u LD_LIBRARY_PATH DISPLAY=$display XDG_SESSION_TYPE=x11 \
  XDG_CONFIG_HOME="$config" XDG_RUNTIME_DIR="$runtime" \
  "$bin" >"$out/gui.log" 2>&1 &
gui_pid=$!
sleep 7
export XDG_RUNTIME_DIR="$runtime"

"$bin" ctl status | jq -e '.result.mode == "gui"' >/dev/null || fail "ctl status"
pass "gui owns the socket"

say "temperature scale round-trips (°F + °C for the screenshots)"
for scale in fahrenheit celsius both; do
  "$bin" ctl temperature "$scale" | jq -e --arg s "$scale" '.result.temperature == $s' >/dev/null \
    || fail "ctl temperature $scale"
done
code=0; "$bin" ctl temperature kelvin >/dev/null 2>&1 || code=$?
[ "$code" = 3 ] || fail "bad scale must exit 3 (got $code)"
jq -e '.temperature_scale == "both"' "$config/sysmon/settings.json" >/dev/null || fail "scale persisted"
pass "temperature: celsius/fahrenheit/both apply + persist, junk exits 3"

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
gui_exit=0; wait "$gui_pid" || gui_exit=$?
[ "$gui_exit" = 0 ] || fail "GUI exited ${gui_exit} on quit (139 = segfault)"
[ ! -S "$runtime/sysmon/ctl.sock" ] || fail "socket removed on quit"
pass "clean shutdown (exit 0, socket unlinked)"

# ── native Wayland: the path Ben actually runs ───────────────────────
# A private headless Sway (never the desktop's): its own runtime dir,
# no input devices, software renderer for the compositor only.
sway_bin="$(command -v /opt/swayfx-ux/bin/sway || command -v sway || true)"
if [ -n "$sway_bin" ]; then
  say "native Wayland on a private headless sway ($sway_bin)"
  wl_runtime="/run/user/$(id -u)/sysmon-e2e-wl.$$"
  mkdir -p "$wl_runtime" && chmod 700 "$wl_runtime"
  printf 'output HEADLESS-1 mode 1280x900\nxwayland disable\nseat seat0 fallback true\n' >"$wl_runtime/sway.conf"
  sway_libs=""
  case "$sway_bin" in /opt/swayfx-ux/*) sway_libs="/opt/swayfx-ux/lib/x86_64-linux-gnu:/opt/swayfx-ux/lib" ;; esac
  setsid env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$wl_runtime" \
    WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
    LD_LIBRARY_PATH="$sway_libs" "$sway_bin" -c "$wl_runtime/sway.conf" >"$out/sway.log" 2>&1 &
  sway_pid=$!
  for _ in $(seq 40); do [ -S "$wl_runtime/wayland-1" ] && break; sleep 0.25; done
  [ -S "$wl_runtime/wayland-1" ] || fail "private sway never offered wayland-1"
  wl_app_runtime="$(mktemp -d /tmp/sysmon-e2e-wlrt.XXXX)"
  # The compositor's library path IS inherited here, as on the desktop.
  env -u DISPLAY LD_LIBRARY_PATH="$sway_libs" WAYLAND_DISPLAY="$wl_runtime/wayland-1" \
    XDG_SESSION_TYPE=wayland XDG_CONFIG_HOME="$config" XDG_RUNTIME_DIR="$wl_app_runtime" \
    "$bin" >"$out/gui-wayland.log" 2>&1 &
  wl_gui=$!
  for _ in $(seq 40); do [ -S "$wl_app_runtime/sysmon/ctl.sock" ] && break; sleep 0.25; done
  XDG_RUNTIME_DIR="$wl_app_runtime" "$bin" ctl status | jq -e '.result.mode == "gui"' >/dev/null \
    || fail "GUI on Wayland didn't come up"
  XDG_RUNTIME_DIR="$wl_app_runtime" "$bin" ctl page processes >/dev/null
  sleep 1.5
  shot=$(XDG_RUNTIME_DIR="$wl_app_runtime" "$bin" ctl shot | jq -r .result.path)
  [ -s "$shot" ] && cp "$shot" "$out/wayland-processes.png"
  XDG_RUNTIME_DIR="$wl_app_runtime" "$bin" ctl quit >/dev/null
  wl_exit=0; wait "$wl_gui" || wl_exit=$?
  kill "$sway_pid" 2>/dev/null || true
  wait "$sway_pid" 2>/dev/null || true
  rm -rf "$wl_runtime" "$wl_app_runtime"
  [ "$wl_exit" = 0 ] || fail "Wayland GUI exited ${wl_exit} on quit"
  pass "Wayland: up, rendered, quit exit 0"
else
  say "no sway binary: native Wayland stage skipped (install sway to run it)"
fi

echo
pass "e2e complete — receipts in $out/"
