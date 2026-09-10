#!/usr/bin/env bash
# gpsd follow (DESIGN.md, gpsd follow as built) in the real window against
# the fixture daemon: with `gpsd = true` a stand-in `gpspipe` on PATH
# streams gpsd JSON, and the map follows each fix — a Dallas fix centres
# the map there, names GPS as the source and hands the radar off to KFWS;
# a fix in Oklahoma City hands off to KTLX; a parked receiver's jitter
# moves nothing; a lock holds the radar while the map still follows; a
# lost fix leaves the view where it was. Run through check.sh's window
# lane, whose scratch daemon it leaves on KTLX.
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir="$PWD/target/check-gps"
mkdir -p "$check_dir/bin"
# The stand-in replays whatever fixes.txt holds, one line a second, then
# waits, so the check drives the position by appending to the file.
cat > "$check_dir/bin/gpspipe" <<EOF
#!/bin/sh
while :; do
  if [ -s "$check_dir/fixes.txt" ]; then
    while IFS= read -r line; do printf '%s\n' "\$line"; sleep 1; done < "$check_dir/fixes.txt"
    : > "$check_dir/fixes.txt"
  fi
  sleep .5
done
EOF
chmod +x "$check_dir/bin/gpspipe"
: > "$check_dir/fixes.txt"
rm -f "$check_dir/state.json"
printf '{\n  "name": "Stokesdale",\n  "latitude": 36.23708,\n  "longitude": -79.97948\n}\n' > "$check_dir/weather.json"
printf 'gpsd = true\n' > "$check_dir/config.toml"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
PATH="$check_dir/bin:$PATH" OMASTORM_CONFIG="$check_dir/config.toml" OMASTORM_LOCATION="$check_dir/weather.json" OMASTORM_STATE="$check_dir/state.json" bash run.sh > "$check_dir/log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
call() { quickshell ipc --pid "$pid" call keys "$@"; }
field() { call field "$1"; }
fail() { printf '%s\n' "$@" >&2; cat "$check_dir/log" >&2; exit 1; }
expect() { [[ "$3" == "$2" ]] || fail "$1" "Expected: $2" "Actual:   $3"; }
near() { awk -v a="$1" -v b="$2" 'BEGIN { d = a - b; exit !(d < .002 && d > -.002) }'; }
until_field() { # name, wanted
  for attempt in {1..150}; do [[ $(field "$1") == "$2" ]] && return; sleep .1; done
  fail "$1 never became $2: $(call status)"
}
fix() { printf '{"class":"TPV","mode":3,"lat":%s,"lon":%s}\n' "$1" "$2" >> "$check_dir/fixes.txt"; }
for attempt in {1..100}; do call status > /dev/null 2>&1 && break; sleep .1; done
call status > /dev/null || fail "The window's keys IPC never answered"

# No fix yet: the weather location places the view, as without gpsd.
until_field locationSource weather
until_field site KFCX

# A Dallas fix: the map centres on it, GPS is the source, KFWS takes over.
fix 32.99 -96.60
until_field locationSource gps
until_field site KFWS
near "$(field lat)" 32.99 && near "$(field lon)" -96.60 || fail "The map did not centre on the fix: $(field lat) $(field lon)"

# Driving to Oklahoma City hands off to KTLX.
fix 35.47 -97.33
until_field site KTLX
near "$(field lat)" 35.47 || fail "The map did not follow to Oklahoma City: $(field lat)"

# A parked receiver's jitter (well under 100 m) moves nothing.
fix 35.4703 -97.3302
sleep 2
expect 'Jitter left the centre alone' "$(field lat)" "35.47"

# A lock holds the radar while the map keeps following.
call run lock
until_field locked true
fix 33.00 -96.60
sleep 3
expect 'The lock held KTLX through a Dallas fix' KTLX "$(field site)"
near "$(field lat)" 33.00 || fail "The map stopped following under a lock: $(field lat)"
call run lock
until_field locked false
until_field site KFWS

# Losing the fix leaves the view where the receiver last was.
printf '{"class":"TPV","mode":1}\n' >> "$check_dir/fixes.txt"
sleep 3
near "$(field lat)" 33.00 || fail "A lost fix moved the map: $(field lat)"

if rg -q 'TypeError|ReferenceError|Unable to assign|is not a function' "$check_dir/log"; then fail "QML errors in the log"; fi
echo "GPS_PASSED"
