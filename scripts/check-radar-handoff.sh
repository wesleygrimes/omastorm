#!/usr/bin/env bash
# Drive loading and hand-offs in the real window without live network feeds.
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-handoff.XXXXXX")
trap 'rm -rf "$check_dir"' EXIT
cp ui/*.qml ui/*.js ui/qmldir "$check_dir/"
ln -s "$PWD/ui/shaders" "$check_dir/shaders"
sed -i '/id: app$/a\    property alias testEngine: engine\n    property alias testMap: map\n    property alias testSurface: surface' "$check_dir/RadarWindow.qml"
cp tests/radar-handoff.qml "$check_dir/shell.qml"
printf 'center_lat = 35.333\ncenter_lon = -97.278\n' > "$check_dir/config.toml"
mkdir -p review
OMASTORM_QML="$check_dir/shell.qml" OMASTORM_REVIEW="$PWD/review" \
  OMASTORM_CONFIG="$check_dir/config.toml" OMASTORM_STATE="$check_dir/state.json" OMASTORM_LOCATION=/dev/null \
  QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl \
  timeout 20 bash run.sh > "$check_dir/result.log" 2>&1
cat "$check_dir/result.log"
rg -q RADAR_HANDOFF_PASSED "$check_dir/result.log"
if rg -q 'TypeError|ReferenceError|Unable to assign|Binding loop|Failed to create.*context' "$check_dir/result.log"; then exit 1; fi
