#!/usr/bin/env bash
# One remembered lock across clients (DESIGN.md, location and remembered
# state): the window and the bar popover each hold a lock intent and re-send
# it when the engine comes back, so a restart used to land on whichever
# client pushed last. Here a second client is another writer of state.json:
# after it changes the lock, this window's next persist keeps that lock
# instead of writing its own back, and after the daemon restarts the window
# asks for the file's station. Own daemon, state, and cache, so the shared
# daemon and the real catalog are left alone.
set -euo pipefail
# shellcheck source=tests/integration/common.sh
source "$(dirname "$0")/common.sh"
cd "$(dirname "$0")/../.."
scratch=$PWD/target/check-reconnect
rm -rf "$scratch"
mkdir -p "$scratch/r"
stage_ui "$scratch/ui"
cp tests/harnesses/location.qml "$scratch/ui/shell.qml"
export OMASTORM_QML="$scratch/ui/shell.qml"
# Runtime/cache are selected and owned by the shared runner.
export OMASTORM_CONFIG="$scratch/config.toml" OMASTORM_STATE="$scratch/state.json"
: > "$OMASTORM_CONFIG"
# Stokesdale, NC, locked to its nearest radar, as the window left it.
write_state() { # lock
  printf '{"lat":36.23708,"lon":-79.97948,"span":233.5,"lock":"%s","name":"Stokesdale"}\n' "$1" > "$scratch/state.next"
  mv "$scratch/state.next" "$OMASTORM_STATE"
}
write_state KFCX
pid=
cleanup() {
  [[ -z $pid ]] || kill "$pid" 2>/dev/null || true
  "${OMASTORM_ENGINE_BINARY:?selected by mise test integration}" stop >/dev/null 2>&1 || true
  : # Runner removes owned runtime/cache.
}
trap cleanup EXIT
bash run.sh > "$scratch/log" 2>&1 &
pid=$!
call() { quickshell ipc --pid "$pid" call keys "$@"; }
field() { call field "$1"; }
fail_log="$scratch/log"
until_field() { # name, wanted
  for _ in {1..150}; do [[ $(field "$1" 2>/dev/null) == "$2" ]] && return; sleep .1; done
  fail "$1 never became $2: $(call status)"
}
lock_in_file() {
  jq -r 'if (.lock | type) == "string" then .lock
         elif .lock.target.kind == "site" then .lock.target.siteId
         else "" end' "$OMASTORM_STATE"
}
wait_window_ready "$pid" || fail "The window never became ready for navigation"
until_field site KFCX
until_field locked true
until_field lockSource state

# The other client locks KAMX. This window's next remembered movement must
# not write KFCX back over it.
write_state KAMX
adopted=false
for _ in {1..100}; do
  if [[ $(quickshell ipc --pid "$pid" call locationTest lockId) == KAMX ]]; then adopted=true; break; fi
  sleep .1
done
[[ $adopted == true ]] || fail "The other client's lock was not adopted"
wait_window_ready "$pid" || fail "The other client's view never settled"
before_lon=$(field lon)
call run pan_left
want_lat=$(field lat); want_lon=$(field lon); want_span=$(field span)
[[ $want_lon != "$before_lon" ]] || fail "Pan did not move the camera"
wait_saved_view "$OMASTORM_STATE" "$want_lat" "$want_lon" "$want_span" \
  || fail "Pan did not persist the changed camera: $(cat "$OMASTORM_STATE")"
[[ $(lock_in_file) == KAMX ]] || fail "A pan wrote this window's old lock over the other client's: $(cat "$OMASTORM_STATE")"

# The daemon restarts; the window reconnects and asks for the file's
# station, not the one it launched with.
"${OMASTORM_ENGINE_BINARY:?selected by mise test integration}" stop
"${OMASTORM_ENGINE_BINARY:?selected by mise test integration}" ensure
until_field site KAMX
until_field locked true
[[ $(lock_in_file) == KAMX ]] || fail "The reconnect rewrote the lock: $(cat "$OMASTORM_STATE")"

# The same remembered-lock path restores a manual mosaic without centering
# on its domain. Panning while locked and reconnecting preserve this camera.
jq '.lock = {sourceId:"mrms-conus",target:{kind:"mosaic"}}' "$OMASTORM_STATE" > "$scratch/state.next"
mv "$scratch/state.next" "$OMASTORM_STATE"
sleep 0.5
before_lat=$(field lat); before_lon=$(field lon); before_span=$(field span)
"${OMASTORM_ENGINE_BINARY:?selected by mise test integration}" stop
"${OMASTORM_ENGINE_BINARY:?selected by mise test integration}" ensure
until_field source mrms-conus
until_field locked true
until_field lat "$before_lat"
until_field lon "$before_lon"
until_field span "$before_span"
call run pan_left
sleep 0.5
until_field source mrms-conus
until_field locked true
[[ $(jq -r '.lock.sourceId' "$OMASTORM_STATE") == mrms-conus ]] || fail 'MRMS lock was not persisted'
check_qml_log "$scratch/log"
echo "RECONNECT_PASSED"
