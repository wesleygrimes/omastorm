#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
export OMASTORM_ROOT="$PWD"
# Plugin bootstrap: no build and no second Quickshell process. A checkout
# with a debug engine stays offline. Otherwise the pinned release installer
# fetches once, verifies the committed sha256, and installs under
# $XDG_DATA_HOME/omastorm/bin (DESIGN.md, distribution).
if [[ ${1:-} == --ensure ]]; then
  # The plugin bootstrap runs detached; its stderr goes to the log it names.
  if [[ -n ${OMASTORM_BOOTSTRAP_LOG:-} ]]; then
    mkdir -p "$(dirname "$OMASTORM_BOOTSTRAP_LOG")"
    exec 2> "$OMASTORM_BOOTSTRAP_LOG"
  fi
  if [[ -x target/debug/omastorm-engine ]]; then
    exec target/debug/omastorm-engine ensure
  fi
  engine=$(bash scripts/fetch-engine.sh --print-path)
  exec "$engine" ensure
fi
if [[ ! -f ui/shaders/radar.frag.qsb || ! -f ui/shaders/tile.frag.qsb ]]; then
  echo 'Missing shader packages. Run bash scripts/build-shader.sh (see data/README.md).' >&2
  exit 1
fi
# Launch is strictly offline. Fetch build dependencies explicitly during setup.
bash scripts/cargo.sh build --offline --locked --quiet
target/debug/omastorm-engine ensure
# A tty launch names this checkout and which files apply, so a leftover
# archive daemon or the installed plugin is obvious. Captures are not a tty.
if [[ -t 1 ]]; then
  mode=live
  [[ -n ${OMASTORM_ARCHIVE:-} ]] && mode="archive $OMASTORM_ARCHIVE"
  config=${OMASTORM_CONFIG:-$HOME/.config/omastorm/config.toml}
  if [[ -n ${OMASTORM_STATE:-} ]]; then
    state=$OMASTORM_STATE
  elif [[ -n ${OMASTORM_CONFIG:-} ]]; then
    state="(not read; OMASTORM_CONFIG is set)"
  else
    state=${XDG_STATE_HOME:-$HOME/.local/state}/omastorm/state.json
  fi
  if [[ -n ${OMASTORM_LOCATION:-} ]]; then
    location=$OMASTORM_LOCATION
  elif [[ -n ${OMASTORM_CONFIG:-} ]]; then
    location="(not read; OMASTORM_CONFIG is set)"
  else
    location=$HOME/.local/state/omarchy/settings/weather.json
  fi
  bar=$(bash scripts/link-plugin.sh --status)
  printf 'Omastorm %s\n  qml    %s\n  engine %s\n  bar    %s\n  config %s\n  state  %s\n  place  %s\n' \
    "$PWD" "${OMASTORM_QML:-ui/shell.qml}" "$mode" "$bar" "$config" "$state" "$location"
fi
# mise start / restart / onboard restart the Omarchy shell when this checkout
# is linked, so the bar popover matches. Captures and checks leave it alone.
if [[ -n ${OMASTORM_RESCAN_PLUGIN:-} ]]; then
  bash scripts/link-plugin.sh --rescan
fi
exec quickshell -p "${OMASTORM_QML:-ui/shell.qml}" "$@"
