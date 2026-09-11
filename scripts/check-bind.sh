#!/usr/bin/env bash
# Documented global key (DESIGN.md, global keybinding): README names
# SUPER + SHIFT + R and the shell toggle; Omarchy defaults do not use
# that chord; install and launch never write bindings.lua.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { printf '%s\n' "$@" >&2; exit 1; }

bind='o.bind("SUPER + SHIFT + R", "Omastorm", "omarchy shell shell toggle com.omastorm.radar '"'"'{}'"'"'")'
rg -F -- "$bind" README.md >/dev/null \
  || fail "README.md does not name the documented o.bind line"
# shellcheck disable=SC2088 # the literal path as the README prints it
rg -F -- '~/.config/hypr/bindings.lua' README.md >/dev/null \
  || fail 'README.md does not name ~/.config/hypr/bindings.lua'
rg -- 'omarchy plugin add https://github\.com/wesleygrimes/omastorm(\.git)? --enable' README.md >/dev/null \
  || fail 'README.md does not name omarchy plugin add https://github.com/wesleygrimes/omastorm --enable'

# Omarchy never writes this file, and neither do install or launch.
if rg -q 'bindings\.lua|hypr/' run.sh scripts/fetch-engine.sh scripts/write-desktop-entry.sh; then
  fail 'run.sh or an installer references Hyprland bindings'
fi

omarchy_hypr=${OMARCHY_PATH:-/usr/share/omarchy}/default/hypr
if [[ -d $omarchy_hypr ]]; then
  # Exact chord, not SUPER + SHIFT + RIGHT / RETURN.
  if rg -n --glob '*.lua' '"SUPER \+ SHIFT \+ R"' "$omarchy_hypr" >/dev/null; then
    fail "SUPER + SHIFT + R is an Omarchy default: $(rg -n --glob '*.lua' '"SUPER \+ SHIFT \+ R"' "$omarchy_hypr")"
  fi
fi

echo 'Bind: README names SUPER + SHIFT + R shell toggle, free in Omarchy defaults, no install write PASS'
