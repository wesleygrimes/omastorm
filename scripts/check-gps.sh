#!/usr/bin/env bash
# gpsd follow (DESIGN.md, gpsd follow as built) in the real window against
# the fixture daemon: with `gpsd = true` a stand-in `gpspipe` on PATH
# streams gpsd JSON, and the map follows each fix — a Dallas fix centres
# the map there, names GPS as the source and hands the radar off to KFWS;
# a fix in Oklahoma City hands off to KTLX; a parked receiver's jitter
# moves nothing; a user pan pauses follow; the crosshair chip pauses and
# resumes; a lock holds the radar while the map still follows; a lost fix
# leaves the view where it was; an explicit config centre outranks the
# remembered fix; turning the key off hides the chip; turning it back on
# clears any pause that carried over. Run through check.sh's window lane,
# whose scratch daemon it leaves near Oklahoma City.
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
  for _ in {1..150}; do [[ $(field "$1") == "$2" ]] && return; sleep .1; done
  fail "$1 never became $2: $(call status)"
}
fix() { printf '{"class":"TPV","mode":3,"lat":%s,"lon":%s}\n' "$1" "$2" >> "$check_dir/fixes.txt"; }
nofix() { printf '{"class":"TPV","mode":1}\n' >> "$check_dir/fixes.txt"; }
for _ in {1..100}; do call status > /dev/null 2>&1 && break; sleep .1; done
call status > /dev/null || fail "The window's keys IPC never answered"

# gpsd = true with no fix yet: the crosshair chip is visible and dimmed
# with NO FIX — the receiver is silent, the weather location still places
# the view, the nearest radar at Stokesdale is still KFCX.
until_field gpsEnabled true
until_field gpsNoFix true
until_field locationSource weather
until_field site KFCX

# A Dallas fix: the chip fills, GPS is the source, KFWS takes over.
fix 32.99 -96.60
until_field gpsNoFix false
until_field gpsFollowing true
until_field locationSource gps
until_field site KFWS
if ! near "$(field lat)" 32.99 || ! near "$(field lon)" -96.60; then
  fail "The map did not centre on the fix: $(field lat) $(field lon)"
fi

# Driving to Oklahoma City hands off to KTLX.
fix 35.47 -97.33
until_field site KTLX
near "$(field lat)" 35.47 || fail "The map did not follow to Oklahoma City: $(field lat)"

# A parked receiver's jitter (well under 100 m) moves nothing.
fix 35.4703 -97.3302
sleep 2
expect 'Jitter left the centre alone' "$(field lat)" "35.47"

# A user pan pauses follow: the camera stays where the pan left it, and
# the next fix that has moved does not snap it back.
call run pan_left
until_field gpsPaused true
fix 33.00 -96.60
sleep 3
near "$(field lat)" 35.47 || fail "A panned follow still snapped to a fix: $(field lat)"

# Clicking the chip resumes: the held fix re-centres right away.
call run follow
until_field gpsPaused false
until_field gpsFollowing true
sleep 3
near "$(field lat)" 33.00 || fail "A resumed follow did not re-centre: $(field lat)"

# Back to Oklahoma City for the lock: it must hold through a Dallas fix.
fix 35.47 -97.33
until_field site KTLX
call run lock
until_field locked true
fix 33.00 -96.60
sleep 3
expect 'The lock held KTLX through a Dallas fix' KTLX "$(field site)"
near "$(field lat)" 33.00 || fail "The map stopped following under a lock: $(field lat)"
call run lock
until_field locked false
until_field site KFWS

# Click the crosshair chip: follow is paused, the next fix that has moved
# does not snap back. The lat / lon stay where the user paused them.
call run follow
until_field gpsPaused true
until_field gpsFollowing false
fix 36.00 -97.50
sleep 3
near "$(field lat)" 33.00 || fail "A paused follow still snapped to a fix: $(field lat)"

# Click the chip again: the next fix that has moved re-centres.
call run follow
until_field gpsPaused false
until_field gpsFollowing true
fix 36.20 -97.70
sleep 3
near "$(field lat)" 36.20 || fail "A resumed follow did not re-centre on a fix: $(field lat)"

# Loss of fix: NO FIX stands beside the chip; the view stays put.
nofix
sleep 3
until_field gpsNoFix true
near "$(field lat)" 36.20 || fail "A lost fix moved the map: $(field lat)"
# A fix that has not moved enough while NO FIX stands does not snap.
fix 36.20 -97.7001
sleep 2
expect 'NO FIX held the view until a fix moved past 100 m' "$(field lat)" "36.2"

# An explicit centre in config.toml wins: the view re-centres there, GPS
# names no source, and a small fix beside it does not drag the camera.
printf 'gpsd = true\ncenter_lat = 32.99\ncenter_lon = -96.60\n' > "$check_dir/config.toml"
until_field locationSource config
near "$(field lat)" 32.99 || fail "The explicit centre did not win: $(field lat)"
until_field site KFWS
fix 32.9901 -96.6001
sleep 3
expect 'A small fix did not drag an explicit centre' "$(field lat)" "32.99"

# Turning gpsd off in the config file hides the chip, clears the fix, and
# leaves the view where it was.
printf 'gpsd = false\n' > "$check_dir/config.toml"
for _ in {1..100}; do [[ $(field gpsEnabled) == "false" ]] && break; sleep .1; done
expect 'gpsd = false hid the chip' "$(field gpsEnabled)" "false"
expect 'gpsd = false leaves no fix to track' "$(field gpsNoFix)" "false"

# gpsd back on with no fix yet: the chip returns in its NO FIX shape.
printf 'gpsd = true\n' > "$check_dir/config.toml"
until_field gpsEnabled true
until_field gpsNoFix true

# A fix arrives again — far enough to matter: the chip fills, no pause
# carried over, GPS names the source.
fix 36.30 -97.80
until_field gpsFollowing true
until_field locationSource gps
near "$(field lat)" 36.30 || fail "A resumed receiver did not follow its fix: $(field lat)"

if rg -q 'TypeError|ReferenceError|Unable to assign|is not a function' "$check_dir/log"; then fail "QML errors in the log"; fi
echo "GPS_PASSED"
