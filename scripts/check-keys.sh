#!/usr/bin/env bash
# The keyboard map (DESIGN.md, keyboard map as built) in the real window,
# driven through its IPC handler against the fixture daemon: the `[keys]`
# table laid over the defaults with every kind of mistake reported in the
# status slot and the defaults kept, the treatment and weak-floor settings,
# each action's effect on the camera, the treatment, the sheet, the menu,
# and the picker, the fix applied live through the file watch, and the
# current-location home from a weather.json with the header naming it. Run
# through check.sh, whose scratch daemon it leaves on the location's
# station; it comes last there for that reason.
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir="$PWD/target/check-keys"
mkdir -p "$check_dir"
rm -f "$check_dir/state.json"
# Stokesdale, NC, as Omarchy's weather panel writes it: the camera sits on
# the place; KFCX (Roanoke) is the nearest radar.
printf '{\n  "name": "Stokesdale",\n  "latitude": 36.23708,\n  "longitude": -79.97948\n}\n' > "$check_dir/weather.json"
cat > "$check_dir/config.toml" <<'TOML'
treatment = "neon"
weak_floor = true
[keys]
pan_left = "a Left"
zoom_in = "foo"
nearest = "s"
bogus = "x"
reset = 0
TOML
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
OMASTORM_CONFIG="$check_dir/config.toml" OMASTORM_LOCATION="$check_dir/weather.json" OMASTORM_STATE="$check_dir/state.json" bash run.sh > "$check_dir/log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
call() { quickshell ipc --pid "$pid" call keys "$@"; }
field() { call field "$1"; }
fail() { printf '%s\n' "$@" >&2; cat "$check_dir/log" >&2; exit 1; }
expect() { [[ "$3" == "$2" ]] || fail "$1" "Expected: $2" "Actual:   $3"; }
less() { awk -v a="$1" -v b="$2" 'BEGIN { exit !(a + 0 < b + 0) }'; }
until_field() { # name, wanted
  for _ in {1..100}; do [[ $(field "$1") == "$2" ]] && return; sleep .1; done
  fail "$1 never became $2: $(call status)"
}
# The map reports its settled centre back through projection math, so a
# picked 35.4 may be on disk as 35.39999999999999 moments later; compare
# the numbers, not the text.
state_at() { # file, lat, lon
  jq -e --argjson lat "$2" --argjson lon "$3" \
    '(.lat - $lat | fabs) < 1e-6 and (.lon - $lon | fabs) < 1e-6' "$1" > /dev/null 2>&1
}
for _ in {1..100}; do call status > /dev/null 2>&1 && break; sleep .1; done
call status > /dev/null || fail "The window's keys IPC never answered"

# The table over the defaults: the good line applies, every mistake is
# reported once and leaves its default in place, a key bound twice stays
# with the first action.
until_field site KFCX
b=$(call bindings)
[[ $b == *'"pan_left":["A","Left"]'* ]] || fail "pan_left was not rebound: $b"
[[ $b == *'"zoom_in":["+","="]'* ]] || fail "A bad zoom_in did not keep its default: $b"
[[ $b == *'"reset":["0"]'* ]] || fail "A numeric reset did not keep its default: $b"
[[ $b == *'"search":["/","S"]'* && $b == *'"nearest":["N"]'* ]] || fail "The conflict on s did not stay with search: $b"
e=$(call errors)
for wanted in 'treatment = \"neon\": not PIXELS, GLYPHS, or STIPPLE' 'weak_floor = true: a dBZ number or false' '[keys] zoom_in = \"foo\": foo is not a key' "[keys] nearest = \\\"s\\\": S is search's key" '[keys] bogus is not an action' '[keys] reset must be a quoted string'; do
  [[ $e == *"$wanted"* ]] || fail "Missing report: $wanted" "Reported: $e"
done
expect 'Six mistakes' 6 "$(grep -o '\[keys\]\|treatment =\|weak_floor =' <<< "$e" | wc -l)"
expect 'The status slot names the first and counts the rest' 'TREATMENT = "NEON": NOT PIXELS, GLYPHS, OR STIPPLE (+5 MORE)' "$(field error)"
expect 'A bad treatment leaves Glyphs' GLYPHS "$(field treatment)"
expect 'A bad weak_floor leaves the default floor' 5 "$(field weakFloor)"
expect 'The header names the weather location' weather "$(field locationSource)"

# Each action's effect, through the same function the shortcuts call.
until_field site KFCX
call run reset
span=$(field span); lon=$(field lon); lat=$(field lat)
call run pan_left
less "$(field lon)" "$lon" || fail "pan_left did not move the centre west: $lon -> $(field lon)"
call run pan_up
less "$lat" "$(field lat)" || fail "pan_up did not move the centre north: $lat -> $(field lat)"
call run zoom_in
less "$(field span)" "$span" || fail "zoom_in did not narrow the span: $span -> $(field span)"
call run reset
expect 'reset returns the span' "$span" "$(field span)"
expect 'reset returns the centre' "$lat $lon" "$(field lat) $(field lon)"
call run pixels
expect '1 picks Pixels' PIXELS "$(field treatment)"
call run weak
expect 'w shows every measured return' off "$(field weakFloor)"
call run weak
expect 'w again restores the floor' 5 "$(field weakFloor)"
call run help
expect '? opens the sheet' true "$(field sheet)"
call run help
expect '? again closes it' false "$(field sheet)"
call menu true
expect 'The chip opens the menu' true "$(field menu)"
call run stipple
expect 'A treatment key closes the menu' false "$(field menu)"
expect 'and picks the treatment' STIPPLE "$(field treatment)"
call run search
expect 'The search key opens the picker' true "$(quickshell ipc --pid "$pid" call picker status | grep -o '"open":[a-z]*' | cut -d: -f2)"
quickshell ipc --pid "$pid" call picker close

# Shift+H opens the location picker; a chosen point writes state, never config.
call run home
for _ in {1..50}; do [[ $(quickshell ipc --pid "$pid" call location status | grep -o '"open":[a-z]*' | cut -d: -f2) == true ]] && break; sleep .1; done
expect 'Shift+H opens the location picker' true "$(quickshell ipc --pid "$pid" call location status | grep -o '"open":[a-z]*' | cut -d: -f2)"
quickshell ipc --pid "$pid" call location go 35.4 -97.5 "Moore"
until_field lat 35.4
until_field lon -97.5
until_field locationSource state
state_at "$check_dir/state.json" 35.4 -97.5 || fail "Shift+H location did not write state.json" "$(cat "$check_dir/state.json")"
grep -q home_site "$check_dir/config.toml" && fail "Shift+H wrote home_site into config.toml"

# The fix applies through the file watch: no report, the new key in force,
# and an explicit centre outranking the weather location.
cat > "$check_dir/config.toml" <<'TOML'
center_lat = 35.333
center_lon = -97.277
treatment = "pixels"
weak_floor = 10
[keys]
zoom_in = "z"
search = ""
TOML
until_field error ''
expect 'No report once the file is fixed' '[]' "$(call errors)"
b=$(call bindings)
[[ $b == *'"zoom_in":["Z"]'* && $b == *'"search":[]'* ]] || fail "The fixed table did not apply: $b"
expect 'The treatment setting applies' PIXELS "$(field treatment)"
expect 'The weak_floor setting applies' 10 "$(field weakFloor)"
until_field locationSource config
until_field lat 35.333
until_field lon -97.277
if rg -q 'TypeError|ReferenceError|Unable to assign|Failed to create.*context|is not a function' "$check_dir/log"; then fail "QML errors in the log"; fi
echo "KEYS_PASSED"
