#!/usr/bin/env bash
# Real card + embedded window, isolated from the desktop shell and daemon.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p review
scratch=$PWD/target/check-popover
rm -rf "$scratch"
mkdir -p "$scratch"
export XDG_RUNTIME_DIR="$scratch/r" XDG_CACHE_HOME="$scratch/cache"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
export OMASTORM_CONFIG="$scratch/config.toml"
export OMASTORM_STATE="$scratch/state.json"
export OMASTORM_ROOT="$PWD"
mkdir -p "$XDG_RUNTIME_DIR"
: > "$OMASTORM_CONFIG"
jq -c '.sites[] | select(.id=="KTLX") | {lat, lon, span: 210}' engine/data/sites.json > "$OMASTORM_STATE"
pid=
cleanup() {
  [[ -z $pid ]] || kill "$pid" 2>/dev/null || true
  target/debug/omastorm-engine stop >/dev/null 2>&1 || true
  # The textures and the seeded frame ring go; ui.log stays for reading.
  rm -rf "$scratch/r" "$scratch/cache"
}
trap cleanup EXIT
quickshell -p ui/PopoverHarness.qml > "$scratch/ui.log" 2>&1 &
pid=$!
call() { quickshell ipc --pid "$pid" call popover "$@"; }
status() { call status; }
fail() { echo "$*" >&2; cat "$scratch/ui.log" >&2; exit 1; }
until_status() {
  local filter=$1
  for _ in {1..100}; do
    status 2>/dev/null | jq -e "$filter" >/dev/null 2>&1 && return 0
    sleep .1
  done
  fail "Timed out: $filter"
}
until_status '.site == "KTLX" and .windowSite == "KTLX"'
before=$(status | jq -r .frame)
call expand
until_status '.window and .expanded'
[[ $(status | jq -r .windowFrame) == "$before" ]] || fail 'Expand changed the archived frame'
call treatment STIPPLE
until_status '.treatment == "STIPPLE" and .windowTreatment == "STIPPLE"'
call closeWindow
until_status '.window == false'
call reopen
call expand
until_status '.window and .treatment == "STIPPLE"'
# Archived provenance and timestamp must never turn into a live badge.
until_status '.condition == "archived" and .text == "ARCHIVED"'
call closeWindow
call reopen
call capture "$PWD/review/popover-archived.png"
for _ in {1..50}; do [[ -s review/popover-archived.png ]] && break; sleep .1; done
sock="$XDG_RUNTIME_DIR/omastorm/engine.sock"
tell() { printf '%s\n' "$@" | socat -t0.2 - "UNIX-CONNECT:$sock" >/dev/null; }
# Two deterministic complete frames, using the archived fixture's metadata
# and PNGs, exercise the real catalog/transport without waiting two volumes.
# Their start times are minutes ago: the catalog evicts frames older than
# two hours when a station is selected.
timeout 2 socat -t0.2 - "UNIX-CONNECT:$sock" < /dev/null | jq -c 'select(.type == "state")' | head -n1 > "$scratch/engine-state.json"
ruby - "$scratch" <<'RUBY_SEED'
require 'json'
require 'fileutils'
require 'open3'
scratch = ARGV.fetch(0)
frame = JSON.parse(File.read("#{scratch}/engine-state.json")).fetch('frame')
dir = "#{scratch}/cache/omastorm/frames"
FileUtils.mkdir_p("#{dir}/KTLX")
sql = []
now_ms = (Time.now.to_f * 1000).to_i
2.times do |i|
  f = Marshal.load(Marshal.dump(frame))
  f['id'] = "popover-test-#{i}"
  f['scanTime'] = "2013-05-20T20:#{10+i*5}:00Z"
  f['sweepEnd'] = f['scanTime']
  tex = "KTLX/test-#{i}-sweep.png"; lut = "KTLX/test-#{i}-lut.png"
  FileUtils.cp("#{scratch}/r/omastorm/#{frame['texture']}", "#{dir}/#{tex}")
  FileUtils.cp("#{scratch}/r/omastorm/#{frame['azimuthLut']}", "#{dir}/#{lut}")
  f['texture'] = ''; f['azimuthLut'] = ''
  values = [f['id'], 'KTLX', 'REF', f['elevationDeg'], now_ms - 600000 + i*300000,
            f['scanTime'], f['sweepEnd'], 'synthetic popover lifecycle test', 0, JSON.generate(f), tex, lut]
  sql << "INSERT INTO frames VALUES (#{values.map { |v| v.is_a?(Numeric) ? v.to_s : "'" + v.gsub("'", "''") + "'" }.join(',')});"
end
_, err, result = Open3.capture3('sqlite3', "#{dir}/catalog.sqlite", stdin_data: sql.join("\n"))
abort err unless result.success?
RUBY_SEED
tell '{"type":"select_site","id":"KTLX"}' '{"type":"seek","id":"popover-test-0"}'
until_status '.frame == "popover-test-0"'
call expand
until_status '.window and .windowFrame == "popover-test-0" and (.windowPlaying | not)'
# A configured lock applies immediately; subsequently selecting another
# station must survive expansion, and closing the window leaves it there.
printf 'locked_radar = "KFCX"\n' > "$OMASTORM_CONFIG"
until_status '.site == "KFCX"'
tell '{"type":"select_site","id":"KTLX"}' '{"type":"lock","enabled":true}' '{"type":"seek","id":"popover-test-0"}'
until_status '.frame == "popover-test-0"'
call step 1
until_status '.frame == "popover-test-1" and .windowFrame == "popover-test-1"'
call play
until_status '.playing and .windowPlaying'
# Expand while playing forwards the shared position instead of seek/pause.
call reopen
call expand
until_status '.playing and .windowPlaying and .site == "KTLX"'
call closeWindow
until_status '.window == false and .site == "KTLX"'
# A stopped daemon followed by ensure must reconnect all surviving clients.
# Reconnect keeps the session lock and camera; this process never unlocked,
# so KFCX is selected again.
target/debug/omastorm-engine stop
until_status '.connected == false'
target/debug/omastorm-engine ensure
until_status '.site == "KFCX" and .connected'
call quit
wait "$pid"
pid=
if rg 'Binding loop|ReferenceError|TypeError|Unable to assign|Failed to load' "$scratch/ui.log"; then fail 'QML runtime errors'; fi
echo 'Popover: archived provenance, expand preservation, treatment sharing, close/reopen, playback, lock, daemon restart PASS'
