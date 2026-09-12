#!/usr/bin/env bash
# The regression suite in one run: formatting, Clippy with warnings denied,
# the Rust unit and socket tests, and the UI checks, each with a
# pass/fail line. Cargo goes first and alone, since every Cargo command
# takes the build directory lock; once the binaries exist the tests run
# while the UI checks proceed in two lanes: the windows that share one
# scratch daemon, in order, and the checks that bring their own daemon or
# need none. Each lane's daemon lives under its own scratch
# XDG_RUNTIME_DIR, so the shared daemon and any open window are left
# alone; the socket tests already use their own. Scratch and logs live
# under target/check/, never /tmp (a tmpfs, which two leftover trees fill),
# and the daemons and runtime files go on every exit, a failure or Ctrl-C
# included; the logs stay until the next run. `--gpu` adds the ignored
# rendering tests (about 50 s; needed after a shader, sampling, or camera
# change). The capture scripts are not here: run the one whose picture the
# session changed.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
gpu=0
[[ ${1:-} == --gpu ]] && gpu=1
# run.sh and the UI checks read target/debug/omastorm-engine. Cargo output
# elsewhere leaves that engine stale or missing, and every window then
# reports that it never answered; a target directory on a tmpfs also
# rebuilds the native crates once the tmpfs fills.
if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
  echo "CARGO_TARGET_DIR is set ($CARGO_TARGET_DIR); the checks build and read target/. Unset it." >&2
  exit 1
fi
scratch=$PWD/target/check
logs=$scratch/logs
rm -rf "$scratch"
mkdir -p "$logs" "$scratch/tmp"
# The check scripts' mktemp calls and the engine installer's work dir land here.
export TMPDIR=$scratch/tmp
failed=0
lanes=()
child=
cleanup() {
  for pid in "${lanes[@]}"; do kill "$pid" 2> /dev/null; done
  wait 2> /dev/null
  for runtime in "$scratch"/r-*; do
    [[ -d $runtime ]] && XDG_RUNTIME_DIR=$runtime target/debug/omastorm-engine stop > /dev/null 2>&1
  done
  rm -rf "$scratch"/r-* "$scratch/tmp"
}
trap cleanup EXIT
trap 'kill "${child:-}" 2> /dev/null; exit 130' INT TERM
step() { # name, command...: one line per step; the output goes to its log
  local name=$1 started=$SECONDS
  shift
  # In the background so a signal ends the step at once instead of after it.
  "$@" > "$logs/$name.log" 2>&1 &
  child=$!
  if wait "$child"; then
    child=
    printf '%-20s PASS  %3ds\n' "$name" $((SECONDS - started))
  else
    child=
    printf '%-20s FAIL  %3ds  (%s)\n' "$name" $((SECONDS - started)) "$logs/$name.log"
    return 1
  fi
}
lane() { # name, checks...: the checks in order, sharing one scratch daemon
  local rc=0
  # Leave room for Quickshell’s socket suffix in nested worktrees.
  export XDG_RUNTIME_DIR=$scratch/r-$1
  shift
  mkdir -p "$XDG_RUNTIME_DIR"
  trap 'kill "${child:-}" 2> /dev/null; exit 143' TERM
  for check in "$@"; do
    step "$check" timeout 180 bash "scripts/$check.sh" || rc=1
  done
  target/debug/omastorm-engine stop > /dev/null 2>&1
  return $rc
}
tests() { # the compiled tests; the cap ends a hung test and its daemons
  local rc=0
  trap 'kill "${child:-}" 2> /dev/null; exit 143' TERM
  step test timeout 600 bash scripts/cargo.sh test --offline --locked || rc=1
  if (( gpu )); then
    step rendering timeout 600 env QT_QPA_PLATFORM=offscreen bash scripts/cargo.sh test --offline --locked -- --ignored rendering || rc=1
  fi
  return $rc
}
step fmt bash scripts/cargo.sh fmt --check || failed=1
step clippy bash scripts/cargo.sh clippy --offline --locked --all-targets -- -D warnings || failed=1
# The engine and the test binaries, so the tests and the windows below
# start without compiling under each other. Without them there is nothing
# to run, and each window would only wait out its timeout.
if step build bash scripts/cargo.sh test --offline --locked --no-run; then
  # The UI checks assume the archived KTLX scan; a fresh daemon shows it when
  # OMASTORM_ARCHIVE names the volume (a shipped daemon starts with no frame).
  export OMASTORM_ARCHIVE="$PWD/data/raw/KTLX20130520_201643_V06.gz"
  tests & lanes+=($!)
  # check-picker and check-keys select stations for real, so they run last.
  lane window check-engine-ui check-map-sites check-map-network check-location check-picker check-keys & lanes+=($!)
  lane alone check-timeline check-ip-location check-bind check-link-plugin check-launcher check-theme check-map-tiles test-engine-pin check-engine-release check-popover & lanes+=($!)
  for pid in "${lanes[@]}"; do wait "$pid" || failed=1; done
  lanes=()
else
  failed=1
  echo 'The engine did not build; the tests and the UI checks did not run.'
fi
if (( failed )); then
  echo "Logs under $logs"
  exit 1
fi
echo "All checks passed."
