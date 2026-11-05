#!/usr/bin/env bash
# The view export (docs/configuration.md, remembered state and view export) in the real window against the
# suite's daemon. The KTLX fixture is one volume scan, so the check asserts the fixture's own values: the station
# on screen, the scan time 2013-05-20T20:16:43Z, and live false (the archive is not a live source). A real pan
# (keys run pan_right) must reach the camera on disk. The daemon then restarts, and the camera must survive the
# reconnect. Not covered here: the sweep and reconnect overlays racing a second client (the bar), since this
# fixture has no second frame to make the trio change. The reconnect check waits a fixed 2 s, so it is a bound,
# not a condition on a write.
set -euo pipefail
# shellcheck source=tests/integration/common.sh
source "$(dirname "$0")/common.sh"
cd "$(dirname "$0")/../.."
check_dir="$PWD/target/check-export"
mkdir -p "$check_dir"
rm -f "$check_dir/state.json"
# Oklahoma City places the view; the nearest station is KTLX.
printf '{\n  "name": "Oklahoma City",\n  "latitude": 35.4676,\n  "longitude": -97.5164\n}\n' > "$check_dir/weather.json"
: > "$check_dir/config.toml"
fail_log="$check_dir/log"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
OMASTORM_CONFIG="$check_dir/config.toml" OMASTORM_LOCATION="$check_dir/weather.json" OMASTORM_STATE="$check_dir/state.json" bash run.sh > "$check_dir/log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
call() { quickshell ipc --pid "$pid" call keys "$@"; }
field() { call field "$1"; }
# `//` would swallow a JSON false, which is the value this check is after.
exported() { jq -r --arg k "$1" 'if has($k) then (.[$k] | tostring) else empty end' "$check_dir/state.json" 2>/dev/null; }
until_export() { # field, wanted
  for _ in {1..400}; do [[ $(exported "$1") == "$2" ]] && return; sleep .1; done
  fail "state.json $1 never became $2" "$(cat "$check_dir/state.json" 2>/dev/null || echo '(no file)')"
}
for _ in {1..100}; do call status > /dev/null 2>&1 && break; sleep .1; done
call status > /dev/null || fail "The window's keys IPC never answered"

# Oklahoma City places the view, and the nearest station on screen is KTLX.
for _ in {1..150}; do [[ -n $(field site) ]] && break; sleep .1; done
[[ $(field site) == KTLX ]] || fail "Oklahoma City should show KTLX: $(call status)"
until_export site KTLX
until_export live false
until_export scan 2013-05-20T20:16:43Z
jq -e '(.live | type) == "boolean"' "$check_dir/state.json" > /dev/null || fail "live is not a boolean: $(cat "$check_dir/state.json")"

# A real pan moves the camera on disk. The window owns the camera, so the panned values are what the rest of
# the check must keep.
for _ in {1..150}; do [[ -n $(exported lon) ]] && break; sleep .1; done
launch_lon=$(exported lon)
[[ -n $launch_lon ]] || fail "The launch-time camera did not land on disk: $(cat "$check_dir/state.json" 2>/dev/null || echo '(no file)')"
call run pan_right
for _ in {1..150}; do [[ $(exported lon) != "$launch_lon" ]] && break; sleep .1; done
panned_lat=$(exported lat); panned_lon=$(exported lon); panned_span=$(exported span)
[[ $panned_lon != "$launch_lon" ]] || fail "The pan never reached the file: $(cat "$check_dir/state.json")"
pan_intact() { [[ $(exported lat) == "$panned_lat" && $(exported lon) == "$panned_lon" && $(exported span) == "$panned_span" ]]; }

# Reconnect: the daemon restarts and the window reconnects; the panned camera must still be on disk after a
# fixed 2 s settle. Time-bounded by design (see the header).
"${OMASTORM_ENGINE_BINARY:?selected by the suite}" stop
"${OMASTORM_ENGINE_BINARY:?selected by the suite}" ensure
sleep 2
pan_intact || fail "The reconnect changed the panned camera: was $panned_lat $panned_lon $panned_span, now $(cat "$check_dir/state.json")"

check_qml_log "$check_dir/log"
echo "EXPORT_PASSED"
