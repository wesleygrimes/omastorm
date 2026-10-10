#!/usr/bin/env bash
# The harness shell for captures of states the real feed cannot be asked
# for: the real UI files, and an Engine that lays OMASTORM_STATE_OVERRIDE (a
# JSON object) over every state it receives, one level deep, so a `frame`
# or `connection` field can change while the rest stays real. Prints the
# shell's path for OMASTORM_QML.
set -euo pipefail
cd "$(dirname "$0")/.."
# Harness outside the checkout: Omarchy rejects a shaders symlink in the plugin folder.
harness=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-capture-harness.XXXXXX")
trap 'rm -rf -- "$harness"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cp ui/shell.qml ui/RadarWindow.qml ui/RadarMark.qml ui/RadarMap.qml ui/TextureCheck.qml ui/Sites.js ui/KeysSheet.qml ui/Keys.js ui/Theme.qml ui/Config.qml ui/Toml.js ui/Location.js ui/Metar.js ui/Metars.qml ui/LocationPicker.qml ui/LocationPrompt.qml ui/Remembered.qml ui/PluginSession.qml ui/qmldir "$harness/"
ln -sfn "$PWD/ui/shaders" "$harness/shaders"
perl -pe 's/^(\s+)state = message;$/$1var override = JSON.parse(Quickshell.env("OMASTORM_STATE_OVERRIDE") || "{}");\n$1for (var key in override) message[key] = override[key] && typeof override[key] === "object" && !Array.isArray(override[key]) && message[key] ? Object.assign(message[key], override[key]) : override[key];\n$1state = message;/' ui/Engine.qml > "$harness/Engine.qml"
grep -q OMASTORM_STATE_OVERRIDE "$harness/Engine.qml"
# The caller owns the completed harness and removes it with its scratch tree.
trap - EXIT INT TERM
echo "$harness/shell.qml"
