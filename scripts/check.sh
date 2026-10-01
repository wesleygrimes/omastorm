#!/usr/bin/env bash
# The regression suite in one run: formatting, Clippy with warnings denied,
# the Rust unit and socket tests, and the UI checks, each with a
# pass/fail line. Cargo goes first and alone, since every Cargo command
# takes the build directory lock; once the binaries exist the tests run
# while the UI checks proceed sequentially in two groups: the windows that share one
# scratch daemon, in order, and the checks that bring their own daemon or
# need none. Each lane's daemon lives under its own scratch
# XDG_RUNTIME_DIR, so the shared daemon and any open window are left
# alone; the socket tests already use their own. Scratch and logs live
# under target/check/, never /tmp (a tmpfs, which two leftover trees fill),
# and the daemons and runtime files go on every exit, a failure or Ctrl-C
# included; the logs stay until the next run. `--gpu` adds the ignored
# rendering tests (desktop context required; needed after a shader, sampling, or camera
# change). The capture scripts are not here: run the one whose picture the
# session changed.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
gpu=0
scope=all
prebuilt=0
for arg in "$@"; do
  case $arg in
    --gpu) gpu=1 ;;
    --prebuilt) prebuilt=1 ;;
    all|engine|protocol|ui|installer|rendering) scope=$arg ;;
    *) echo 'Usage: check.sh [all|engine|protocol|ui|installer|rendering] [--gpu] [--prebuilt]' >&2; exit 2 ;;
  esac
done
[[ $scope == rendering ]] && gpu=1
# CI supplies a disposable tree with a verified pin or compiled candidate.
# Its cargo wrapper refuses compilation; never use this to bypass engine tests.
if (( prebuilt )) && { [[ $scope != ui && $scope != installer ]] || (( gpu )); }; then
  echo '--prebuilt requires ui or installer, without --gpu.' >&2
  exit 2
fi
# Fixed paths in the UI scripts are shared by runs in this checkout.
# Refuse overlap before deleting scratch files or stopping any test daemon.
mkdir -p target
exec 9>target/check.lock
flock -n 9 || { echo 'Another check is running in this checkout.' >&2; exit 1; }
run_started=$SECONDS
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
# Unix sockets have a 108-byte path limit. Keep their visible prefix short
# even in CI workspaces and nested disposable trees; contents stay on disk.
runtime_alias=$(mktemp -d /tmp/omastorm-runtime.XXXXXX)
ln -s "$scratch" "$runtime_alias/tree"
# The check scripts' mktemp calls and the engine installer's work dir land here.
export TMPDIR=$scratch/tmp
printf 'step\tstatus\tseconds\n' > "$logs/timings.tsv"
failed=0
lanes=()
child=
plugin_alias=
cleanup() {
  for pid in "${lanes[@]}"; do kill "$pid" 2> /dev/null; done
  wait 2> /dev/null
  for runtime in "$runtime_alias/tree"/r-*; do
    [[ -d $runtime ]] && XDG_RUNTIME_DIR=$runtime target/debug/omastorm-engine stop > /dev/null 2>&1
  done
  rm -rf "$scratch"/r-* "$scratch/tmp"
  [[ -z $plugin_alias ]] || rm -rf "$plugin_alias"
  rm -rf "$runtime_alias"
}
trap cleanup EXIT
trap 'kill "${child:-}" 2> /dev/null; exit 130' INT TERM
step() { # name, command...: one line per step; the output goes to its log
  local name=$1 started=$SECONDS
  shift
  # In the background so a signal ends the step at once instead of after it.
  "$@" 9>&- > "$logs/$name.log" 2>&1 &
  child=$!
  if wait "$child"; then
    child=
    printf '%s\tPASS\t%d\n' "$name" "$((SECONDS - started))" >> "$logs/timings.tsv"
    printf '%-20s PASS  %3ds\n' "$name" $((SECONDS - started))
  else
    child=
    printf '%s\tFAIL\t%d\n' "$name" "$((SECONDS - started))" >> "$logs/timings.tsv"
    printf '%-20s FAIL  %3ds  (%s)\n' "$name" $((SECONDS - started)) "$logs/$name.log"
    return 1
  fi
}
lane() { # name, checks...: the checks in order, sharing one scratch daemon
  local rc=0
  # Leave room for Quickshell’s socket suffix in nested worktrees.
  unset WAYLAND_DISPLAY
  export XDG_RUNTIME_DIR=$runtime_alias/tree/r-$1
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
  case $scope in
    engine) step test timeout 600 bash scripts/cargo.sh test --offline --locked --bin omastorm-engine || rc=1 ;;
    protocol) step protocol timeout 600 bash scripts/cargo.sh test --offline --locked --test protocol || rc=1 ;;
    all) step test timeout 600 bash scripts/cargo.sh test --offline --locked || rc=1 ;;
  esac
  return $rc
}
if ! protocol=$(bash scripts/ci-protocol.sh); then exit 1; fi
engine_protocol=$(sed -n 's/^engine=//p' <<< "$protocol")
ui_protocol=$(sed -n 's/^ui=//p' <<< "$protocol")
if (( gpu )) && [[ $engine_protocol != "$ui_protocol" ]]; then
  echo 'Rendering checks require matching engine/UI protocols.' >&2
  exit 1
fi
if [[ $scope == all || $scope == engine ]]; then
  step fmt bash scripts/cargo.sh fmt --check || failed=1
  step clippy bash scripts/cargo.sh clippy --offline --locked --all-targets -- -D warnings || failed=1
fi
# The engine and the test binaries, so the tests and the windows below
# start without compiling under each other. Without them there is nothing
# to run, and each window would only wait out its timeout.
built=0
build=(test --offline --locked --no-run)
case $scope in
  engine) build+=(--bin omastorm-engine) ;;
  protocol) build+=(--test protocol) ;;
  rendering) build+=(--test rendering) ;;
  ui|installer) build=(build --offline --locked) ;;
esac
if (( prebuilt )); then
  build_command=(test -x target/debug/omastorm-engine)
else
  build_command=(bash scripts/cargo.sh "${build[@]}")
fi
if step build "${build_command[@]}"; then
  built=1
  # The UI checks assume the archived KTLX scan; a fresh daemon shows it when
  # OMASTORM_ARCHIVE names the volume (a shipped daemon starts with no frame).
  export OMASTORM_ARCHIVE="$PWD/data/raw/KTLX20130520_201643_V06.gz"
  tests & lanes+=($!)
  if [[ $scope == all || $scope == ui || $scope == installer ]]; then
    # Validate the candidate independently of the currently shipped client.
    if (( ! prebuilt )); then
      step engine-binary bash scripts/check-engine-binary.sh target/debug/omastorm-engine || failed=1
      step engine-release bash scripts/check-engine-release.sh || failed=1
    fi
    # A split protocol release keeps the old UI and its published pin together.
    # Test that pair in a disposable tree; never overwrite the candidate binary.
    if (( ! prebuilt )) && [[ $engine_protocol != "$ui_protocol" ]]; then
      if ! step pinned-ui-setup bash scripts/prepare-pinned-ui-check.sh "$scratch/plugin"; then
        exit 1
      fi
      # Keep runtime socket names below sockaddr_un's limit even in worktrees.
      # Only the alias lives in /tmp; fixture copies and logs stay under target/.
      plugin_alias=$(mktemp -d /tmp/omastorm-ui.XXXXXX)
      ln -s "$scratch/plugin" "$plugin_alias/tree"
      cd "$plugin_alias/tree" || exit 1
      echo "UI checks: published pin (protocol v$ui_protocol); candidate engine: v$engine_protocol"
    fi
    # check-picker and check-keys select stations for real, so they run last.
    if [[ $scope != installer ]]; then
      lane window check-engine-ui check-radar-handoff check-map-sites check-map-network check-location check-gps check-picker check-keys & lanes+=($!)
      wait "${lanes[-1]}" || failed=1
      unset 'lanes[-1]'
    fi
    if [[ $scope == installer ]]; then
      lane alone check-bind check-link-plugin check-launcher test-engine-pin & lanes+=($!)
    else
      lane alone check-ip-location check-bind check-link-plugin check-launcher check-theme check-map-tiles test-engine-pin check-popover check-reconnect & lanes+=($!)
    fi
    wait "${lanes[-1]}" || failed=1
    unset 'lanes[-1]'
  fi
  for pid in "${lanes[@]}"; do wait "$pid" || failed=1; done
  lanes=()
else
  failed=1
  echo 'The engine did not build; the tests and the UI checks did not run.'
fi
# Rendering creates Quickshell windows too; run after both Rust and UI groups.
if (( gpu && built )); then
  step rendering timeout 600 env -u WAYLAND_DISPLAY QT_QPA_PLATFORM=offscreen bash scripts/cargo.sh test --offline --locked --test rendering -- --ignored || failed=1
fi
printf 'total\t%s\t%d\n' "$([[ $failed == 0 ]] && echo PASS || echo FAIL)" "$((SECONDS - run_started))" >> "$logs/timings.tsv"
echo "Timing report: $logs/timings.tsv (wall time $((SECONDS - run_started))s)"
if (( failed )); then
  echo "Logs under $logs"
  exit 1
fi
echo "All checks passed."
