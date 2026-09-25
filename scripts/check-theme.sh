#!/usr/bin/env bash
# Exercise real FileView events and the shipped IPC hook without desktop edits.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p review
root="$PWD"
tmp=$(mktemp -d "$PWD/review/theme-check.XXXXXX")
trap 'if [[ -n ${pid:-} ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi; rm -rf "$tmp"' EXIT
mkdir -p "$tmp/ui" "$tmp/theme"
cp ui/Theme.qml ui/Toml.js "$tmp/ui/"
cat > "$tmp/ui/shell.qml" <<'QML'
import Quickshell
import Quickshell.Io
ShellRoot {
    Theme { id: theme }
    IpcHandler {
        target: "check"
        function snapshot(): string {
            var s = theme.snapshot;
            return [s.background, s.foreground, s.accent, s.baseSize, s.font].join(" ");
        }
    }
}
QML
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic
export OMASTORM_THEME_DIR="$tmp/theme" OMASTORM_USER_SHELL="$tmp/user.toml"
quickshell -p "$tmp/ui/shell.qml" > "$tmp/log" 2>&1 &
pid=$!
expect() {
    local actual=""
    for _ in {1..50}; do
        actual=$(quickshell ipc --pid "$pid" call check snapshot 2>/dev/null) || true
        if [[ "$actual" == "$1" ]]; then return; fi
        sleep .1
    done
    printf 'Expected: %s\nActual: %s\n' "$1" "$actual" >&2
    cat "$tmp/log" >&2
    exit 1
}
expect '#1a1b26 #a9b1d6 #7aa2f7 12 monospace'
printf 'background = "#112233"\nforeground = "#ddeeff"\naccent = "#123456"\n' > "$tmp/theme/colors.toml"
expect '#112233 #ddeeff #123456 12 monospace'
printf '[popups]\nbackground = "accent" # role\n[font]\nbase-size = 14\n' > "$tmp/theme/shell.toml"
expect '#123456 #ddeeff #123456 14 monospace'
printf '[popups]\ntext = "#abcdef"\n[font]\nbase-size = 18\n' > "$tmp/user.toml"
expect '#123456 #abcdef #123456 18 monospace'
printf 'background = "#445566"\nforeground = "#778899"\naccent = "#aabbcc"\n' > "$tmp/theme/new.toml"
mv "$tmp/theme/new.toml" "$tmp/theme/colors.toml"
expect '#aabbcc #abcdef #aabbcc 18 monospace'
rm "$tmp/user.toml" "$tmp/theme/shell.toml"
expect '#445566 #778899 #aabbcc 12 monospace'
# `omarchy theme set` replaces the directory (rm -rf, then mv) and rewrites
# theme.name beside it; the watchers follow with no hook.
mkdir "$tmp/next-theme"
printf 'background = "#0a0b0c"\naccent = "#0d0e0f"\n' > "$tmp/next-theme/colors.toml"
rm -rf "$tmp/theme"
mv "$tmp/next-theme" "$tmp/theme"
echo next > "$tmp/theme.name"
expect '#0a0b0c #a9b1d6 #0d0e0f 12 monospace'
# The watch lands on the new directory, so edits after a switch still apply.
printf 'background = "#0b0c0d"\naccent = "#0d0e0f"\n' > "$tmp/theme/colors.toml"
expect '#0b0c0d #a9b1d6 #0d0e0f 12 monospace'
# The explicit IPC hook still reloads after a replacement.
mv "$tmp/theme" "$tmp/old-theme"
mkdir "$tmp/theme"
printf 'background = "#102030"\n' > "$tmp/theme/colors.toml"
OMASTORM_ROOT="$tmp" bash "$root/scripts/hooks/omastorm"
expect '#102030 #a9b1d6 #7aa2f7 12 monospace'
# Watchers must remain usable after that explicit reload.
printf 'background = "#203040"\n' > "$tmp/theme/colors.toml"
expect '#203040 #a9b1d6 #7aa2f7 12 monospace'
printf 'Theme watching, overrides, replacement, theme switch, fallback, and IPC hook: PASS\n'
