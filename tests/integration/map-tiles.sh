#!/usr/bin/env bash
set -euo pipefail
# shellcheck source=tests/integration/common.sh
source "$(dirname "$0")/common.sh"
cd "$(dirname "$0")/../.."
# Harness outside the checkout: Omarchy rejects a shaders symlink in the plugin folder.
check_dir=$(mktemp -d "${TMPDIR:?}/omastorm-check-map-tiles.XXXXXX")
trap 'rm -rf "$check_dir"' EXIT
mkdir -p review
rm -f review/zoom-held-before.png review/zoom-held-partial.png review/zoom-ready.png
cp ui/RadarMap.qml ui/TextureCheck.qml ui/Metar.js "$check_dir/"
cp tests/map-tiles.qml "$check_dir/shell.qml"
ln -sfn "$PWD/ui/shaders" "$check_dir/shaders"
magick -size 512x512 xc:'rgba(255,0,0,1)' "$check_dir/old.png"
magick -size 512x512 xc:'rgba(0,255,0,1)' "$check_dir/new.png"
OMASTORM_REVIEW="$PWD/review" timeout 15 quickshell -p "$check_dir/shell.qml" > "$check_dir/result.log" 2>&1
cat "$check_dir/result.log"
rg -q MAP_TILES_PASSED "$check_dir/result.log"
check_qml_log "$check_dir/result.log"
compare -metric AE review/zoom-held-before.png review/zoom-held-partial.png null: 2> "$check_dir/difference.txt"
echo "Zoom hold pixel difference: $(cat "$check_dir/difference.txt")"
# Solid synthetic masks make a missing layer unambiguous, including the swap.
held=$(magick review/zoom-held-partial.png -format '%[pixel:p{599,419}]' info:)
ready=$(magick review/zoom-ready.png -format '%[pixel:p{599,419}]' info:)
[[ "$held" == *'(75,75,75'* && "$ready" == *'(47,84,131'* ]] || {
  echo "Unexpected held/ready map pixels: $held / $ready" >&2; exit 1;
}
