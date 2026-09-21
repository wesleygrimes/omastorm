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
cd "$(dirname "$0")/.."
scratch=$PWD/target/check-reconnect
rm -rf "$scratch"
mkdir -p "$scratch/r"
export XDG_RUNTIME_DIR="$scratch/r" XDG_CACHE_HOME="$scratch/cache"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
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
  target/debug/omastorm-engine stop >/dev/null 2>&1 || true
  rm -rf "$scratch/r" "$scratch/cache"
}
trap cleanup EXIT
bash run.sh > "$scratch/log" 2>&1 &
pid=$!
call() { quickshell ipc --pid "$pid" call keys "$@"; }
field() { call field "$1"; }
fail() { printf '%s\n' "$@" >&2; cat "$scratch/log" >&2; exit 1; }
until_field() { # name, wanted
  for _ in {1..150}; do [[ $(field "$1" 2>/dev/null) == "$2" ]] && return; sleep .1; done
  fail "$1 never became $2: $(call status)"
}
lock_in_file() {
  jq -r 'if (.lock | type) == "string" then .lock
         elif .lock.target.kind == "site" then .lock.target.siteId
         else "" end' "$OMASTORM_STATE"
}
for _ in {1..100}; do call status > /dev/null 2>&1 && break; sleep .1; done
call status > /dev/null || fail "The window's keys IPC never answered"
until_field site KFCX
until_field locked true
until_field lockSource state

# The other client locks KAMX. This window's next remembered movement must
# not write KFCX back over it.
write_state KAMX
sleep 0.5
call run pan_left
sleep 1
[[ $(lock_in_file) == KAMX ]] || fail "A pan wrote this window's old lock over the other client's: $(cat "$OMASTORM_STATE")"

# The daemon restarts; the window reconnects and asks for the file's
# station, not the one it launched with.
target/debug/omastorm-engine stop
target/debug/omastorm-engine ensure
until_field site KAMX
until_field locked true
[[ $(lock_in_file) == KAMX ]] || fail "The reconnect rewrote the lock: $(cat "$OMASTORM_STATE")"
if rg -q 'TypeError|ReferenceError|Unable to assign|is not a function' "$scratch/log"; then fail "QML errors in the log"; fi
echo "RECONNECT_PASSED"
