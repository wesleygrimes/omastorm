#!/usr/bin/env bash
# The site picker (DESIGN.md, picker as built) in the real window, driven
# through its IPC handler against the fixture daemon: the tiers and the
# distance order, the four-row cut, the empty and the hopeless query, the
# selection keys, and Enter selecting and locking the station for real. Run
# through focused integration runner, whose scratch daemon it leaves on the chosen station; it
# comes last there for that reason.
set -euo pipefail
# shellcheck source=tests/integration/common.sh
source "$(dirname "$0")/common.sh"
cd "$(dirname "$0")/../.."
check_dir="$PWD/target/check-picker"
mkdir -p "$check_dir"
: > "$check_dir/none.toml"
jq -c '.sites[] | select(.id=="KTLX") | {lat, lon, span: 210}' engine/data/sites.json > "$check_dir/state.json"
OMASTORM_CONFIG="$check_dir/none.toml" OMASTORM_STATE="$check_dir/state.json" bash run.sh > "$check_dir/log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
call() { quickshell ipc --pid "$pid" call picker "$@"; }
fail_log="$check_dir/log"
for _ in {1..100}; do call status > /dev/null 2>&1 && break; sleep .1; done
call status > /dev/null || fail "The window's picker IPC never answered"
key() { quickshell ipc --pid "$pid" call keys "$@"; }
call open ""
for _ in {1..50}; do [[ $(call matches) != '[]' ]] && break; sleep .1; done
# The fixture's home view centres north-west of KTLX: KTLX first, the Norman pair next.
# Manual-only MRMS stays out of empty browsing.
m=$(call matches)
[[ $m == '["KTLX","K'* && $m == *KOUN* && $m == *KCRI* && $m != *mrms-conus* ]] || fail "Empty query did not list the nearest stations first without MRMS: $m"
# This IPC field counts the station table; mosaic matches are checked above.
expect 'Empty query counts the station table' '{"open":true,"query":"","selected":0,"total":163,"focused":true}' "$(call status)"
call open mrms
m=$(call matches)
[[ $m == *mrms-conus* ]] || fail "mrms did not list the NOAA mosaic: $m"
call accept
for _ in {1..50}; do
  src=$(quickshell ipc --pid "$pid" call keys field source)
  locked=$(quickshell ipc --pid "$pid" call keys field locked)
  lat=$(quickshell ipc --pid "$pid" call keys field lat)
  lon=$(quickshell ipc --pid "$pid" call keys field lon)
  [[ $src == mrms-conus && $locked == true && $lat == 37.5 && $lon == -95 ]] && break
  sleep .1
done
[[ $src == mrms-conus && $locked == true && $lat == 37.5 && $lon == -95 ]] || fail "MRMS selection did not center and lock: $src $locked $lat $lon"
# n hands back to a station; Shift+N locks the covering mosaic without moving the camera.
key run nearest
for _ in {1..50}; do [[ $(key field source) != mrms-conus && $(key field locked) == false ]] && break; sleep .1; done
[[ $(key field source) != mrms-conus ]] || fail "n did not hand MRMS back to a station"
key run mosaic
for _ in {1..50}; do [[ $(key field source) == mrms-conus && $(key field locked) == true ]] && break; sleep .1; done
expect 'Shift+N locks MRMS in place' 'mrms-conus true 37.5 -95' "$(key field source) $(key field locked) $(key field lat) $(key field lon)"
key run mosaic
for _ in {1..50}; do [[ $(key field source) != mrms-conus && $(key field locked) == false ]] && break; sleep .1; done
[[ $(key field source) != mrms-conus && $(key field locked) == false ]] || fail "Shift+N again did not hand MRMS back to a station"
key run mosaic
for _ in {1..50}; do [[ $(key field source) == mrms-conus ]] && break; sleep .1; done
call open opera
m=$(call matches)
[[ $m == *opera* ]] || fail "opera did not list the EUMETNET mosaic: $m"
call open eumetnet
m=$(call matches)
[[ $m == *opera* ]] || fail "eumetnet did not list the EUMETNET mosaic: $m"
call open tlx
expect 'ID without its leading letter ranks first' '"KTLX"' "$(call matches | cut -d, -f1 | tr -d '[]')"
call open tulsa
for _ in {1..40}; do
  m=$(call matches)
  [[ $m == *Tulsa* || $m == *KINX* ]] && break
  sleep .1
done
[[ $m == *Tulsa* || $m == *KINX* ]] || fail "tulsa did not match a place or KINX: $m"
call open ok
for _ in {1..40}; do
  m=$(call matches)
  [[ $m == *Oklahoma* ]] && break
  sleep .1
done
[[ $m == *Oklahoma* ]] || fail "ok ranked places first: $m"
call open zzzq
expect 'A hopeless query shows nothing' '[]' "$(call matches)"
expect 'A hopeless query counts nothing' '{"open":true,"query":"zzzq","selected":0,"total":0,"focused":true}' "$(call status)"
call close
expect 'Close clears the picker' '{"open":false,"query":"","selected":0,"total":0,"focused":false}' "$(call status)"
call open ok
call move 1; call move 5
expect 'Down stops at the last of four rows' '3' "$(call status | grep -o '"selected":[0-9]*' | cut -d: -f2)"
call move -9
expect 'Up stops at the first row' '0' "$(call status | grep -o '"selected":[0-9]*' | cut -d: -f2)"
call open koun
first=$(call matches | cut -d, -f1 | tr -d '[]"')
expect 'KOUN finds the Norman radar' KOUN "$first"
call accept
expect 'Enter closes the picker' 'false' "$(call status | grep -o '"open":[a-z]*' | cut -d: -f2)"
# The field must let go of the keyboard, or the next `/` types into it instead of reopening.
expect 'Enter hands the keyboard back' 'false' "$(call status | grep -o '"focused":[a-z]*' | cut -d: -f2)"
sock="$XDG_RUNTIME_DIR/omastorm/engine.sock"
for _ in {1..50}; do
  line=$(timeout 2 socat -t0.2 - "UNIX-CONNECT:$sock" < /dev/null | sed -n 2p || true)
  id=$(jq -r '.selection.target.siteId // empty' <<< "$line" 2>/dev/null || true)
  locked=$(jq -r '.navigation.locked // false' <<< "$line" 2>/dev/null || true)
  [[ $id == "$first" && $locked == true ]] && break
  sleep .1
done
[[ $id == "$first" && $locked == true ]] || fail "Enter did not select and lock $first: $line"
want_lat=$(jq -r --arg id "$first" '.sites[] | select(.id==$id) | ((.lat * 1000) | round) / 1000' engine/data/sites.json)
want_lon=$(jq -r --arg id "$first" '.sites[] | select(.id==$id) | ((.lon * 1000) | round) / 1000' engine/data/sites.json)
for _ in {1..50}; do
  lat=$(quickshell ipc --pid "$pid" call keys field lat)
  lon=$(quickshell ipc --pid "$pid" call keys field lon)
  [[ $lat == "$want_lat" && $lon == "$want_lon" ]] && break
  sleep .1
done
[[ $lat == "$want_lat" && $lon == "$want_lon" ]] || fail "Enter did not centre the map on $first" "Expected: $want_lat $want_lon" "Actual:   $lat $lon"
check_qml_log "$check_dir/log"

# Stub wl-paste; keep the field's key/mouse handlers and read limits.
clipboard="$check_dir/clipboard"
mkdir -p "$clipboard/bin"
stage_ui "$clipboard/ui"
python3 - "$clipboard/ui/LocationPicker.qml" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
anchor = '    id: picker\n'
assert anchor in text
path.write_text(text.replace(anchor, anchor + '    property alias testField: field\n    property alias testReader: clipboardReader\n', 1))
PY
cp tests/harnesses/picker-clipboard.qml "$clipboard/ui/Test.qml"
cat > "$clipboard/bin/wl-paste" <<'PY'
#!/usr/bin/env python3
import os
from pathlib import Path
import signal
import sys
import time

root = Path(os.environ['OMASTORM_CLIPBOARD_FIXTURE'])
(root / 'owner.pid').write_text(str(os.getpid()))
with (root / 'calls').open('a') as log:
    print(' '.join(sys.argv[1:]), file=log)
mode = (root / 'mode').read_text().strip()
if mode == 'huge':
    try:
        while True:
            os.write(1, b'x' * 4096)
    except BrokenPipeError:
        sys.exit(1)
if mode == 'stubborn':
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
if mode in ('stall', 'trickle', 'stubborn'):
    while True:
        if mode == 'trickle':
            os.write(1, b'x')
        time.sleep(.2)
if mode == 'delayed':
    time.sleep(.5)
payload = {'coords': '36.23708, -79.97948', 'unicode': 'Łódź',
           'limit': 'x' * 256, 'oversize': 'x' * 257,
           'delayed': 'KTLX', 'error': 'KTLX'}[mode]
os.write(1, payload.encode())
if mode == 'error':
    sys.exit(1)
PY
chmod +x "$clipboard/bin/wl-paste"
: > "$clipboard/calls"
printf 'coords\n' > "$clipboard/mode"
OMASTORM_CLIPBOARD_FIXTURE="$clipboard" PATH="$clipboard/bin:$PATH" \
  quickshell --no-color -p "$clipboard/ui/Test.qml" > "$clipboard/log" 2>&1 &
clipboard_pid=$!
trap 'kill "$clipboard_pid" "$pid" 2>/dev/null || true; wait "$clipboard_pid" "$pid" 2>/dev/null || true' EXIT
clip() { quickshell ipc --pid "$clipboard_pid" call clipboardTest "$@"; }
fail_log="$clipboard/log"
for _ in {1..100}; do clip status > /dev/null 2>&1 && break; sleep .1; done
clip status > /dev/null || fail 'Clipboard harness did not start'
clip_expect() { local state; state=$(clip status); jq -e "$2" <<< "$state" > /dev/null || fail "$1: $state"; }
owner_alive() {
  local state
  state=$(cat "/proc/$1/stat" 2>/dev/null) || return 1
  state=${state##*) }
  # Container PID 1 may leave dead readers as zombies.
  [[ ${state%% *} != Z && ${state%% *} != X ]]
}
clip_ready() {
  for _ in {1..50}; do
    [[ $(clip status | jq -r .focused) == true ]] && return
    sleep .05
  done
  fail 'Picker did not receive focus'
}
clip_done() {
  local before=$1 state
  for _ in {1..100}; do
    state=$(clip status)
    if [[ $(wc -l < "$clipboard/calls") -gt $before ]] && jq -e '.running == false' <<< "$state" > /dev/null; then return; fi
    sleep .05
  done
  fail "Bounded paste did not finish: $state"
}
paste_mode() {
  printf '%s\n' "$1" > "$clipboard/mode"
  local before; before=$(wc -l < "$clipboard/calls")
  clip key "${2:-Ctrl+V}" > /dev/null
  clip_done "$before"
}
clip offer 'NATIVE CLIPBOARD MUST NOT BE PASTED'
clip_expect 'Closed field still accepts paste' '.canPaste == false'
[[ ! -s $clipboard/calls ]] || fail 'Closed picker started clipboard ingestion'
clip open ''
clip_ready
clip type tlx
clip_expect 'Normal typing failed' '.query == "tlx"'
clip key Ctrl+A > /dev/null
paste_mode coords
clip_expect 'Selected-text coordinate paste failed' '.query == "36.23708, -79.97948" and .error == ""'
# Coordinate queries skip place search.
clip open ''
clip_ready
paste_mode unicode Shift+Insert
for _ in {1..30}; do
  [[ $(clip status | jq -r .searched) == 'Łódź' ]] && break
  sleep .05
done
clip_expect 'Shift+Insert/Unicode search failed' '.query == "Łódź" and .searched == "Łódź"'
clip open ''
clip_ready
printf 'coords\n' > "$clipboard/mode"
before=$(wc -l < "$clipboard/calls")
clip middle > /dev/null
clip_done "$before"
clip_expect 'Middle-click paste failed' '.query == "36.23708, -79.97948"'
tail -1 "$clipboard/calls" | rg -q -- '--primary' || fail 'Middle-click did not use primary selection'
clip open ''
clip_ready
paste_mode limit
clip type z
clip_expect 'Query length limit failed' '(.query | length) == 256 and .error == ""'
for mode in oversize huge error stall trickle stubborn; do
  clip open keep
  clip_ready
  paste_mode "$mode"
  clip_expect "Unsafe $mode paste changed the query or exceeded its read cap" '.query == "keep" and .error != "" and .collected <= 1025'
  if [[ $mode == stall || $mode == trickle || $mode == stubborn ]]; then
    owner=$(cat "$clipboard/owner.pid")
    ! owner_alive "$owner" || fail "Timed-out clipboard owner survived: $owner"
  fi
done
printf 'delayed\n' > "$clipboard/mode"
clip open ''
clip_ready
before=$(wc -l < "$clipboard/calls")
clip key Ctrl+V > /dev/null
clip type x
clip_done "$before"
clip_expect 'Late paste overwrote typing' '.query == "x"'
clip open ''
clip_ready
before=$(wc -l < "$clipboard/calls")
clip key Ctrl+V > /dev/null
clip close
clip open fresh
clip_ready
clip_done "$before"
clip_expect 'Late paste overwrote reopened query' '.query == "fresh"'
clip prompt
clip offer 'SECOND CLIPBOARD OFFER'
clip_expect 'Onboarding field still accepts paste' '.canPaste == false'
clip closeBeforeFocus
clip_expect 'Deferred focus survived immediate close' '.open == false and .focused == false and .canPaste == false'
clip open tlx
clip_ready
clip key Escape > /dev/null
clip_expect 'Escape did not release focus' '.open == false and .focused == false and .canPaste == false'
check_qml_log "$clipboard/log"
# The reader must still time out after its UI process exits.
printf 'stall\n' > "$clipboard/mode"
clip open ''
clip_ready
before=$(wc -l < "$clipboard/calls")
clip key Ctrl+V > /dev/null
for _ in {1..50}; do
  [[ $(wc -l < "$clipboard/calls") -gt $before ]] && break
  sleep .05
done
[[ $(wc -l < "$clipboard/calls") -gt $before ]] || fail 'Stalled clipboard owner did not start'
owner=$(cat "$clipboard/owner.pid")
owner_alive "$owner" || fail 'Stalled owner exited before UI shutdown'
kill "$clipboard_pid"
wait "$clipboard_pid" 2>/dev/null || true
for _ in {1..60}; do
  ! owner_alive "$owner" && break
  sleep .05
done
! owner_alive "$owner" || fail 'Clipboard reader survived its deadline after UI shutdown'
echo 'Picker clipboard: PASS'
echo "PICKER_PASSED"
