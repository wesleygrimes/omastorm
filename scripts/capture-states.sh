#!/usr/bin/env bash
# The connection states side by side (DESIGN.md), as
# review/states-*.png and one sheet, review/states-sheet.png.
#
# ARCHIVED and LIVE are the shared daemon as it is (LIVE needs a station
# with data on the feed; SITE, default KJAX). OFFLINE and SILENT
# are real engine runs against scratch daemons over a copy of the cache:
# OFFLINE in a network namespace with no interfaces, so the poller reports
# the bucket unreachable and the cached frames show with their age; SILENT
# on a table station the bucket holds nothing for (SILENT_SITE,
# default KCRI), which is UNAVAILABLE with nothing cached. STALE, an
# UNAVAILABLE station with cached frames, and LOADING cannot be scheduled
# on the real feed, so a harness copy of the shell receives the live state
# with `connection` replaced, over the real textures.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p review
export OMASTORM_ARCHIVE=${OMASTORM_ARCHIVE:-$PWD/data/raw/KTLX20130520_201643_V06.gz} # the archived scan the checks assume
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
site="${SITE:-KJAX}"
silent_site="${SILENT_SITE:-KCRI}"
review="$PWD/review"
rm -f "$review"/states-*.png
# Scratch daemons never touch the shared daemon's runtime directory or the
# real cache; ensure in run.sh finds them by build under XDG_RUNTIME_DIR.
# The windows read a scratch config.toml naming the station, or none.
scratch=$(mktemp -d /tmp/omastorm-states.XXXXXX)
harness_dir=
trap 'rm -rf "$scratch" ${harness_dir:+"$harness_dir"}' EXIT
mkdir -p "$scratch/offline" "$scratch/silent" "$scratch/cache"
config_for() { # station id, or nothing for no configured radar
  local file="$scratch/config-${1:-none}.toml"
  if [[ -n ${1:-} ]]; then
    jq -r --arg id "$1" '.sites[] | select(.id==$id) | "center_lat = \(.lat)\ncenter_lon = \(.lon)\nlocked_radar = \"\(.id)\""' engine/data/sites.json > "$file"
  else
    : > "$file"
  fi
  echo "$file"
}

capture() { # name, delay ms, env...
  local name=$1 delay=$2
  shift 2
  env "$@" OMASTORM_WIDTH=960 OMASTORM_HEIGHT=680 OMASTORM_CAPTURE_DELAY="$delay" OMASTORM_CAPTURE="$review/states-$name.png" bash run.sh > /dev/null 2>&1
  [[ -s "$review/states-$name.png" ]] || { echo "No capture for $name" >&2; exit 1; }
  echo "captured $name"
}

capture archived 2500 OMASTORM_CONFIG="$(config_for)"
capture live 15000 OMASTORM_CONFIG="$(config_for "$site")"

cp -r "${XDG_CACHE_HOME:-$HOME/.cache}/omastorm" "$scratch/cache/"
daemon() { # runtime dir, wrapper...
  local rt=$1
  shift
  XDG_RUNTIME_DIR="$rt" XDG_CACHE_HOME="$scratch/cache" "$@" target/debug/omastorm-engine serve > "$rt/engine.log" 2>&1 &
  for _ in $(seq 100); do [[ -S "$rt/omastorm/engine.sock" ]] && return; sleep .1; done
  echo "Scratch daemon in $rt did not start" >&2; cat "$rt/engine.log" >&2; exit 1
}
daemon "$scratch/offline" unshare -rn
capture offline 8000 XDG_RUNTIME_DIR="$scratch/offline" XDG_CACHE_HOME="$scratch/cache" OMASTORM_CONFIG="$(config_for "$site")"
daemon "$scratch/silent" env
capture silent 75000 XDG_RUNTIME_DIR="$scratch/silent" XDG_CACHE_HOME="$scratch/cache" OMASTORM_CONFIG="$(config_for "$silent_site")"
grep -h "^Live" "$scratch/offline/engine.log" "$scratch/silent/engine.log" | sed 's/^/  engine: /' | head -4
pkill -f "^target/debug/omastorm-engine serve" -P $$ 2>/dev/null || true
jobs -p | xargs -r kill 2>/dev/null || true

# The harness shell (scripts/capture-harness.sh): the real UI files with
# OMASTORM_STATE_OVERRIDE laid over every state.
harness=$(bash scripts/capture-harness.sh)
harness_dir=$(dirname "$harness")
synthetic() { # name, override
  capture "$1" 6000 OMASTORM_QML="$harness" OMASTORM_CONFIG="$(config_for "$site")" OMASTORM_STATE_OVERRIDE="$2"
}
synthetic stale '{"connection":{"status":"stale","ageSeconds":1380}}'
synthetic unavailable '{"connection":{"status":"unavailable","ageSeconds":2820}}'
synthetic loading '{"connection":{"status":"loading","ageSeconds":40}}'

cd "$review"
magick montage -label '%t' states-archived.png states-live.png states-stale.png states-loading.png states-unavailable.png states-offline.png states-silent.png \
  -tile 2x -geometry 960x680+10+14 -background '#181414' -fill '#e6d9db' -pointsize 22 states-sheet.png
echo "review/states-sheet.png"
