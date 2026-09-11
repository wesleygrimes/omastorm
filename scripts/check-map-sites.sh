#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Harness outside the checkout: Omarchy rejects a shaders symlink in the plugin folder.
check_dir=$(mktemp -d /tmp/omastorm-check-map-sites.XXXXXX)
trap 'rm -rf "$check_dir"' EXIT
mkdir -p review
rm -f review/site-overlay.png
cp ui/RadarMap.qml ui/Engine.qml "$check_dir/"
# Expose the real delegate only in the harness, to check its scene transform.
sed -i '/id: map$/a\    property alias coverageForTest: coverageRepeater' "$check_dir/RadarMap.qml"
cp tests/map-sites.qml "$check_dir/shell.qml"
ln -sfn "$PWD/ui/shaders" "$check_dir/shaders"
OMASTORM_QML="$check_dir/shell.qml" OMASTORM_REVIEW="$PWD/review" \
 QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl \
 timeout 20 bash run.sh > "$check_dir/result.log" 2>&1
cat "$check_dir/result.log"
rg -q MAP_SITES_PASSED "$check_dir/result.log"
test -s review/site-overlay.png
if rg -q 'TypeError|ReferenceError|Unable to assign|Failed to create.*context' "$check_dir/result.log"; then exit 1; fi
