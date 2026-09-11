#!/usr/bin/env bash
# Opt-in launcher entry (DESIGN.md, launcher entry): writes a desktop
# file under scratch XDG_DATA_HOME, never from install or launch.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { printf '%s\n' "$@" >&2; exit 1; }

scratch=$PWD/target/check-launcher
rm -rf "$scratch"
mkdir -p "$scratch"
trap 'rm -rf "$scratch"' EXIT
export XDG_DATA_HOME="$scratch/data"
apps=$XDG_DATA_HOME/applications
desktop=$apps/omastorm.desktop
mark=$PWD/branding/mark/omastorm-mark-app.svg
[[ -f $mark ]] || fail "Omastorm mark missing: $mark"

if bash scripts/write-desktop-entry.sh --bogus 2>"$scratch/usage.err"; then
  fail 'write-desktop-entry.sh accepted an unknown argument'
fi
rg -q 'usage: write-desktop-entry.sh' "$scratch/usage.err" \
  || fail "Unknown-arg error was unclear: $(cat "$scratch/usage.err")"

# Install and launch never write the desktop file.
if rg -q 'write-desktop-entry' run.sh scripts/fetch-engine.sh; then
  fail 'run.sh or fetch-engine.sh references write-desktop-entry.sh'
fi
[[ ! -e $desktop ]] || fail 'Scratch already had omastorm.desktop'

path=$(bash scripts/write-desktop-entry.sh --print-path)
[[ $path == "$desktop" ]] || fail "--print-path: $path"
[[ -f $desktop ]] || fail 'write-desktop-entry.sh did not write omastorm.desktop'

rg -q '^Type=Application$' "$desktop" || fail 'desktop Type missing'
rg -q '^Name=Omastorm$' "$desktop" || fail 'desktop Name is not Omastorm'
exec_line=$(awk -F= '/^Exec=/{print substr($0,6); exit}' "$desktop")
[[ $exec_line == 'omarchy shell shell toggle com.omastorm.radar "{}"' ]] \
  || fail "desktop Exec is not the shell toggle: $exec_line"
icon_line=$(awk -F= '/^Icon=/{print substr($0,6); exit}' "$desktop")
[[ $icon_line == "$mark" ]] || fail "desktop Icon is not this tree's mark: $icon_line"
rg -q '^TryExec=omarchy$' "$desktop" || fail 'desktop TryExec missing'
rg -q '^Terminal=false$' "$desktop" || fail 'desktop Terminal is not false'
rg -q '^StartupNotify=false$' "$desktop" || fail 'desktop StartupNotify is not false'

if command -v desktop-file-validate >/dev/null 2>&1; then
  desktop-file-validate "$desktop" || fail 'desktop-file-validate rejected omastorm.desktop'
fi

# A second run refreshes the file in place.
printf 'stale' > "$desktop"
bash scripts/write-desktop-entry.sh
rg -q '^Name=Omastorm$' "$desktop" || fail 'second run did not refresh omastorm.desktop'

# The same DesktopEntries list the Omarchy launcher reads.
cat > "$scratch/probe.qml" <<'QML'
import Quickshell
import Quickshell.Io
ShellRoot {
    IpcHandler {
        target: "probe"
        function entry(): string {
            var e = DesktopEntries.byId("omastorm")
            return e ? (e.id + "|" + e.name) : ""
        }
    }
}
QML
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic
quickshell -p "$scratch/probe.qml" > "$scratch/probe.log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; rm -rf "$scratch"' EXIT
expect_entry() {
  local want=$1 actual=""
  for _ in {1..50}; do
    actual=$(quickshell ipc --pid "$pid" call probe entry 2>/dev/null) || true
    if [[ $actual == "$want" ]]; then return; fi
    sleep .1
  done
  printf 'DesktopEntries entry: expected %s, got %s\n' "$want" "$actual" >&2
  cat "$scratch/probe.log" >&2
  exit 1
}
expect_entry 'omastorm|Omastorm'

# Deleting the file is removal.
rm -f -- "$desktop"
[[ ! -e $desktop ]] || fail 'rm did not remove omastorm.desktop'
expect_entry ''

echo 'Launcher: opt-in desktop write, shell-toggle Exec, DesktopEntries list/unlist, no install/launch hook PASS'
