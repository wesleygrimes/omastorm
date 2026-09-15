#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Harness outside the checkout: Omarchy rejects a shaders symlink in the plugin folder.
check_dir=$(mktemp -d /tmp/omastorm-check-map-overlay.XXXXXX)
trap 'rm -rf "$check_dir"' EXIT
mkdir -p review
rm -f review/overlay-crop.png
cp ui/RadarMap.qml "$check_dir/"
# Expose the real strip delegate only in the harness, to check what is drawn.
sed -i '/id: map$/a\    property alias overlayForTest: overlayRepeater' "$check_dir/RadarMap.qml"
cp tests/map-overlay.qml "$check_dir/shell.qml"
ln -sfn "$PWD/ui/shaders" "$check_dir/shaders"
# A synthetic published file: the map area red, the legend panel beside it
# blue. A capture that keeps the panel out shows red where the panel would be.
magick -size 577x400 xc:'rgba(0,0,255,1)' -fill 'rgba(255,0,0,1)' \
  -draw 'rectangle 0,0 397,399' "$check_dir/overlay.png"
QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl \
 OMASTORM_REVIEW="$PWD/review" timeout 20 quickshell -p "$check_dir/shell.qml" > "$check_dir/result.log" 2>&1
cat "$check_dir/result.log"
rg -q MAP_OVERLAY_PASSED "$check_dir/result.log"
test -s review/overlay-crop.png
if rg -q 'TypeError|ReferenceError|Unable to assign|Failed to create.*context' "$check_dir/result.log"; then exit 1; fi
# The harness zooms to a 500 km span before the capture, which puts the box at
# x 91..809 across a 900 px window. Sample inside the box near its east edge,
# where an uncropped picture would show the blue panel, and outside the box,
# where the map draws nothing at all.
inside=$(magick review/overlay-crop.png -format '%[pixel:p{789,350}]' info:)
outside=$(magick review/overlay-crop.png -format '%[pixel:p{50,350}]' info:)
echo "box-east pixel: $inside   off-box pixel: $outside"
[[ "$inside" == *'(255,0,0'* ]] || {
  echo "The picture is missing, misplaced, or uncropped at its east edge: $inside" >&2; exit 1;
}
[[ "$outside" == *'(0,0,0,0'* ]] || {
  echo "The picture is drawn outside its published box: $outside" >&2; exit 1;
}
