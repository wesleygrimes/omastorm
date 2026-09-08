#!/usr/bin/env bash
# The home view (docs/protocol.md, configuration) in two passes: the camera
# itself in the map harness, then the real window driven through its IPC
# handler against a config.toml that names a home point, including a
# coordinate out of range reported in the status slot with the station's own
# home view left standing. The window pass selects the point's station, so
# it hands the scratch daemon back on the fixture station for the checks
# that follow.
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir="$PWD/target/check-map-home"
mkdir -p "$check_dir"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl

# The camera: no point, a point, a pan, a reset, a hand-off, and a station
# chosen by hand, against the real RadarMap.
cp ui/RadarMap.qml ui/Engine.qml "$check_dir/"
cp tests/map-home.qml "$check_dir/shell.qml"
ln -sfn "$PWD/ui/shaders" "$check_dir/shaders"
OMASTORM_QML="$check_dir/shell.qml" timeout 25 bash run.sh > "$check_dir/result.log" 2>&1
cat "$check_dir/result.log"
rg -q MAP_HOME_PASSED "$check_dir/result.log"
if rg -q 'TypeError|ReferenceError|Unable to assign|Failed to create.*context' "$check_dir/result.log"; then exit 1; fi

# The window: config.toml to camera, station, and status slot. Oklahoma
# City, well inside the fixture station's range and not the station.
printf 'home_lat = 35.47\nhome_lon = -97.52\n' > "$check_dir/config.toml"
OMASTORM_CONFIG="$check_dir/config.toml" bash run.sh > "$check_dir/window.log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
call() { quickshell ipc --pid "$pid" call keys "$@"; }
field() { call field "$1"; }
fail() { printf '%s\n' "$@" >&2; cat "$check_dir/window.log" >&2; exit 1; }
expect() { [[ "$3" == "$2" ]] || fail "$1" "Expected: $2" "Actual:   $3"; }
until_field() { # name, wanted
  for attempt in {1..100}; do [[ $(field "$1") == "$2" ]] && return; sleep .1; done
  fail "$1 never became $2: $(call status)"
}
for attempt in {1..100}; do call status > /dev/null 2>&1 && break; sleep .1; done

until_field lat 35.47
expect "The home view did not take the configured longitude" -97.52 "$(field lon)"
expect "A home point should read as a config home" config "$(field homeSource)"
home=$(field home)
[[ -n $home ]] || fail "A home point did not pick a home station"
until_field site "$home"
expect "A valid home point should not be reported" "" "$(field error)"

# A coordinate out of range is named and leaves the station's own home view.
printf 'home_lat = 200\nhome_lon = -97.52\n' > "$check_dir/config.toml"
until_field error 'HOME_LAT = 200: A LATITUDE BETWEEN -90 AND 90'
[[ $(field lat) != 35.47 ]] || fail "A rejected home point still placed the home view"

# One coordinate alone is not a point either.
printf 'home_lat = 35.47\n' > "$check_dir/config.toml"
until_field error 'HOME_LAT AND HOME_LON: THE HOME VIEW NEEDS BOTH'

# Hand the shared daemon back on the fixture station, so a check that runs
# after this one starts where it would have without it.
printf 'home_site = "KTLX"\n' > "$check_dir/config.toml"
until_field site KTLX

echo "MAP_HOME_WINDOW_PASSED"
