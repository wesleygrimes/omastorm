#!/usr/bin/env bash
# The view export (docs/configuration.md, remembered state and view
# export) in the real window against the fixture daemon: state.json names
# the station on screen with the frame's scan time and whether it is the
# live head; stepping back flips `live` to false and moves `scan` to that
# frame; jumping to the newest sets it true again. The window and the bar
# are separate processes on one state.json, so the check also simulates the
# other client: a pan lands on disk while this process stays put, the daemon
# restarts, and the reconnect and the next sweep must leave the panned
# camera alone. Run through check.sh's window lane.
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir="$PWD/target/check-export"
mkdir -p "$check_dir"
rm -f "$check_dir/state.json"
printf '{\n  "name": "Stokesdale",\n  "latitude": 36.23708,\n  "longitude": -79.97948\n}\n' > "$check_dir/weather.json"
: > "$check_dir/config.toml"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
OMASTORM_CONFIG="$check_dir/config.toml" OMASTORM_LOCATION="$check_dir/weather.json" OMASTORM_STATE="$check_dir/state.json" bash run.sh > "$check_dir/log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
call() { quickshell ipc --pid "$pid" call keys "$@"; }
field() { call field "$1"; }
fail() { printf '%s\n' "$@" >&2; cat "$check_dir/log" >&2; exit 1; }
expect() { [[ "$3" == "$2" ]] || fail "$1" "Expected: $2" "Actual:   $3"; }
# `//` would swallow a JSON false, which is the value this check is after.
exported() { jq -r --arg k "$1" 'if has($k) then (.[$k] | tostring) else empty end' "$check_dir/state.json" 2>/dev/null; }
# The station the weather location picks is a live one, so stepping to an
# older frame waits on a real fetch: allow it the time.
until_export() { # field, wanted
  for _ in {1..400}; do [[ $(exported "$1") == "$2" ]] && return; sleep .1; done
  fail "state.json $1 never became $2" "$(cat "$check_dir/state.json" 2>/dev/null || echo '(no file)')"
}
until_field() { # name, wanted
  for _ in {1..150}; do [[ $(field "$1") == "$2" ]] && return; sleep .1; done
  fail "$1 never became $2: $(call status)"
}
for _ in {1..100}; do call status > /dev/null 2>&1 && break; sleep .1; done
call status > /dev/null || fail "The window's keys IPC never answered"

# The weather location places the view and the nearest station follows;
# the fixture daemon serves its archive as a live source, so the newest
# frame is the head: the export names that station, the scan, and live.
for _ in {1..150}; do [[ -n $(field site) ]] && break; sleep .1; done
site=$(field site)
[[ -n $site ]] || fail "No station ever showed: $(call status)"
call run newest
until_export site "$site"
until_export live true
head=$(exported scan)
[[ $head =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] || fail "The exported scan is not an RFC 3339 instant: $head"

# Stepping to the oldest frame: not the head, and the scan is that frame's.
call run oldest
until_export live false
older=$(exported scan)
[[ -n $older && $older != "$head" ]] || fail "The oldest frame's scan did not travel: head $head, now $older"

# A sweep overlays the trio onto the file in place; the camera and lock
# keys from the launch-time snapshot must survive. (`Location.overlay`
# preserves every key it didn't put there itself, and the sweep path now
# uses that merge instead of a whole-file replace.)
camera_lat=$(exported lat)
camera_lon=$(exported lon)
camera_span=$(exported span)
[[ -n $camera_lat && -n $camera_lon && -n $camera_span ]] || fail "The launch-time camera did not land on disk: $(cat "$check_dir/state.json" 2>/dev/null || echo '(no file)')"

# Push another sweep through: back to the newest. The merge re-records the
# trio while leaving the camera alone.
call run newest
until_export live true
[[ $(exported lat) == "$camera_lat" ]] || fail "A sweep rewrote lat: was $camera_lat, now $(exported lat)"
[[ $(exported lon) == "$camera_lon" ]] || fail "A sweep rewrote lon: was $camera_lon, now $(exported lon)"
[[ $(exported span) == "$camera_span" ]] || fail "A sweep rewrote span: was $camera_span, now $(exported span)"

# Back to the newest: the head again — the same scan, or a newer one if
# the live station delivered a sweep meanwhile.
[[ $(exported scan) > $head || $(exported scan) == "$head" ]] || fail "The head scan went backwards: head $head, now $(exported scan)"

# The cross-client clobber: the other client (the bar) pans while this
# process stays put, so the file moves on and this process's camera does
# not. When the daemon returns, the reconnect holds only the launch-time
# camera — writing the whole snapshot here would put it back over the pan.
# The reconnect and the sweep that follows may re-record site/scan/live,
# never the camera keys.
panned_lat=37.11; panned_lon=-80.25; panned_span=160
jq --argjson lat "$panned_lat" --argjson lon "$panned_lon" --argjson span "$panned_span" \
    '.lat = $lat | .lon = $lon | .span = $span' "$check_dir/state.json" > "$check_dir/state.next"
mv "$check_dir/state.next" "$check_dir/state.json"
sleep 1
target/debug/omastorm-engine stop
target/debug/omastorm-engine ensure
until_export site "$site"
pan_intact() { [[ $(exported lat) == "$panned_lat" && $(exported lon) == "$panned_lon" && $(exported span) == "$panned_span" ]]; }
pan_intact || fail "The reconnect wrote the launch camera over the other client's pan: $(cat "$check_dir/state.json")"
call run oldest
until_export live false
pan_intact || fail "A sweep rewrote the camera after the reconnect: $(cat "$check_dir/state.json")"
call run newest
until_export live true
pan_intact || fail "A sweep rewrote the camera after the reconnect: $(cat "$check_dir/state.json")"

if rg -q 'TypeError|ReferenceError|Unable to assign|is not a function' "$check_dir/log"; then fail "QML errors in the log"; fi
echo "EXPORT_PASSED"
