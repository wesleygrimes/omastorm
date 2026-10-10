#!/usr/bin/env bash
# Real GPU upload checks in both surfaces; generated rasters, no weather feed.
set -euo pipefail
# shellcheck source=tests/integration/common.sh
source "$(dirname "$0")/common.sh"
cd "$(dirname "$0")/../.."
scratch=$PWD/target/check-texture-guard
rm -rf "$scratch"
# Runtime/cache are selected and owned by the shared runner; the surfaces
# read the generated rasters from its texture directory.
tex="${XDG_RUNTIME_DIR:?selected by mise test integration}/omastorm/tex"
mkdir -p "$scratch/root" "$tex" review
stage_ui "$scratch/ui"
cp manifest.json "$scratch/root/manifest.json"
printf 'exit 0\n' > "$scratch/root/run.sh"
printf 'center_lat = 37.5\ncenter_lon = -95\n' > "$scratch/config.toml"
printf '{"lat":37.5,"lon":-95,"span":4000}\n' > "$scratch/state.json"
ruby - "$scratch" "$tex" <<'RUBY'
require 'zlib'
require 'fileutils'
dir = ARGV.fetch(0)
tex = ARGV.fetch(1)
# Each anchor must exist; a silent no-op would test unpatched sources.
def patch(path, from, to)
  text = File.read(path)
  abort("#{path} changed: #{from.strip}") unless text.include?(from)
  File.write(path, text.sub(from, to))
end
def chunk(kind, data)
  [data.bytesize].pack('N') + kind + data + [Zlib.crc32(kind + data)].pack('N')
end
# 131072 exceeds the reference GPU's 16384 limit without a huge allocation.
# The PNG remains tiny; Qt really downsizes its GPU upload in this test.
{ 'native' => 7000, 'oversize' => 131072 }.each do |name, width|
  header = [width, 1, 8, 6, 0, 0, 0].pack('NNC5')
  pixels = "\x00".b + "\x01\x00\x00\xff".b * width
  png = "\x89PNG\r\n\x1a\n".b + chunk('IHDR', header) + chunk('IDAT', Zlib.deflate(pixels)) + chunk('IEND', ''.b)
  File.binwrite("#{tex}/#{name}.png", png)
end
%w[mismatch current reopened].each { |name| FileUtils.cp("#{tex}/native.png", "#{tex}/#{name}.png") }
[['RadarWindow.qml', 'app', 'engine'], ['Popover.qml', 'card', 'connection']].each do |file, id, engine|
  patch("#{dir}/ui/#{file}", "    id: #{id}\n", "    id: #{id}\n    property alias testEngine: #{engine}\n    property alias testMap: map\n    property alias testError: errorNotice\n")
end
patch("#{dir}/ui/RadarWindow.qml", "    id: app\n", "    id: app\n    property alias testSurface: surface\n")
patch("#{dir}/ui/RadarMap.qml", "    id: map\n", "    id: map\n    property alias testCheck: textureCheck\n")
engine = "#{dir}/ui/Engine.qml"
patch(engine, 'connected: true', 'connected: false')
patch(engine, 'running: engine.socket && !engine.socket.connected && !engine.incompatible', 'running: false')
patch(engine, '    function send(command) {', "    function send(command) {\n        return;")
RUBY
cp tests/texture-guard.qml "$scratch/ui/Check.qml"
export OMASTORM_ROOT="$scratch/root" OMASTORM_CONFIG="$scratch/config.toml" OMASTORM_STATE="$scratch/state.json" OMASTORM_LOCATION=/dev/null
export OMASTORM_REVIEW="$PWD/review"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
check() {
  local log=$1
  timeout 20 quickshell --no-color -p "$scratch/ui/Check.qml" > "$log" 2>&1
  rg -q TEXTURE_GUARD_PASSED "$log" || { cat "$log"; fail "Texture guard did not pass: $log"; }
  check_qml_log "$log"
}
check "$scratch/ui.log"
# A missing checker simulates a backend without the required shader support.
# A decoded native-size image must still be withheld with a visible error.
rm "$scratch/ui/shaders/texture-check.frag.qsb"
export OMASTORM_TEST_UNVERIFIABLE=1
check "$scratch/unverifiable.log"
echo 'Texture guard: native GPU dimensions, real downscaling, metadata mismatch, pending cancellation, hidden views, recovery, failed verification PASS'
