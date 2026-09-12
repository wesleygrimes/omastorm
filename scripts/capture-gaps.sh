#!/usr/bin/env bash
# The tick strip's breaks (DESIGN.md, time) at full and compact widths, as
# review/gaps-full.png and review/gaps-compact.png with strip crops. The
# harness shell lays a KAKQ-shaped timeline over the archived state: 21
# frames at 7 minutes to 09/10 4:56 PM EDT, a 26 h hole, 16 frames at 5.5
# minutes; the playhead steps across the hole three seconds in, so the
# notice is up, and the break marker is focused so its note shows.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p review
export OMASTORM_ARCHIVE=${OMASTORM_ARCHIVE:-$PWD/data/raw/KTLX20130520_201643_V06.gz}
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
export TZ=America/New_York
harness=$(bash scripts/capture-harness.sh)
harness_dir=$(dirname "$harness")
trap 'rm -rf "$harness_dir"' EXIT
timeline=$(jq -cn '
  def iso: todate;
  [range(0; 21) | {id: ("f" + tostring), scanTime: ((1789065360 + . * 420) | iso), status: "complete"}]
  + [range(21; 37) | {id: ("f" + tostring), scanTime: ((1789167060 + (. - 21) * 330) | iso), status: "complete"}]')
before=$(jq -cn --argjson t "$timeline" '{source: "live", connection: {status: "ok", ageSeconds: 130}, timeline: $t, frame: {id: "f20", scanTime: $t[20].scanTime}}')
after=$(jq -cn --argjson t "$timeline" '{source: "live", connection: {status: "ok", ageSeconds: 130}, timeline: $t, frame: {id: "f21", scanTime: $t[21].scanTime}}')
config="$harness_dir/config.toml"
jq -r '.sites[] | select(.id=="KTLX") | "center_lat = \(.lat)\ncenter_lon = \(.lon)\nlocked_radar = \"\(.id)\""' engine/data/sites.json > "$config"
capture() { # name, width, height, note (focus the break marker so its note shows)
  local name=$1 out="$PWD/review/gaps-$1.png"
  rm -f "$out"
  OMASTORM_QML="$harness" OMASTORM_CONFIG="$config" OMASTORM_WIDTH="$2" OMASTORM_HEIGHT="$3" \
    OMASTORM_STATE_OVERRIDE="$before" OMASTORM_STATE_OVERRIDE_THEN="$after" OMASTORM_STATE_OVERRIDE_THEN_MS=3000 \
    OMASTORM_CAPTURE_DELAY=6000 OMASTORM_CAPTURE="$out" bash run.sh > /dev/null 2>&1 &
  local pid=$!
  if [[ ${4:-} == note ]]; then
    sleep 4
    quickshell ipc --pid "$pid" call keys gap 0
  fi
  wait "$pid" || true
  [[ -s $out ]] || { echo "No capture for $name" >&2; exit 1; }
  echo "review/gaps-$name.png"
}
capture full 960 680
capture full-note 960 680 note
capture compact 400 420
capture compact-note 400 420 note
for name in full full-note; do magick "review/gaps-$name.png" -gravity south -crop 960x120+0+0 +repage "review/gaps-$name-strip.png"; done
for name in compact compact-note; do magick "review/gaps-$name.png" -gravity south -crop 400x110+0+0 +repage "review/gaps-$name-strip.png"; done
echo "review/gaps-*-strip.png"
