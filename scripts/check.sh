#!/usr/bin/env bash
# The regression suite in one run: formatting, Clippy with warnings denied,
# the Rust unit and socket tests, and the UI checks, each with a
# pass/fail line. The UI checks launch their windows against a scratch
# daemon under a scratch XDG_RUNTIME_DIR, so the shared daemon and any open
# window are left alone; the socket tests already use their own. `--gpu`
# adds the ignored rendering tests (about 50 s; needed after a shader,
# sampling, or camera change). The capture scripts are not here: run the
# one whose picture the session changed.
set -uo pipefail
cd "$(dirname "$0")/.."
gpu=0
[[ ${1:-} == --gpu ]] && gpu=1
scratch=$(mktemp -d /tmp/omastorm-check.XXXXXX)
logs="$scratch/logs"
mkdir -p "$logs"
failed=0
step() { # name, command...
  local name=$1
  shift
  local started=$SECONDS
  if "$@" > "$logs/$name.log" 2>&1; then
    printf '%-20s PASS  %3ds\n' "$name" $((SECONDS - started))
  else
    printf '%-20s FAIL  %3ds  (%s)\n' "$name" $((SECONDS - started)) "$logs/$name.log"
    failed=1
  fi
}
step fmt bash scripts/cargo.sh fmt --check
step clippy bash scripts/cargo.sh clippy --offline --locked --all-targets -- -D warnings
step test bash scripts/cargo.sh test --offline --locked
if (( gpu )); then
  step rendering env QT_QPA_PLATFORM=offscreen bash scripts/cargo.sh test --offline --locked -- --ignored rendering
fi
# The UI checks assume the archived KTLX scan; a fresh daemon shows it when
# OMASTORM_ARCHIVE names the volume (a shipped daemon starts with no frame).
export OMASTORM_ARCHIVE="$PWD/data/raw/KTLX20130520_201643_V06.gz"
# check-picker and check-keys select stations for real, so they run last.
export XDG_RUNTIME_DIR="$scratch/runtime"
mkdir -p "$XDG_RUNTIME_DIR"
for check in check-engine-ui check-engine-install check-map-tiles check-map-sites check-map-network check-map-home check-theme check-picker check-keys check-popover check-launcher check-bind; do
  step "$check" timeout 180 bash "scripts/$check.sh"
done
target/debug/omastorm-engine stop > /dev/null 2>&1 || true
if (( failed )); then
  echo "Logs under $logs"
  exit 1
fi
rm -rf "$scratch"
echo "All checks passed."
