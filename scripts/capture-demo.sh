#!/usr/bin/env bash
# One continuous live take of the window, the README and announcement demo,
# 1280×720 at
# 30 fps, on a scratch daemon that goes live on SITE (default KJAX) and
# backfills its loop first. The take: the live home view, the loop playing,
# a pan and zoom to the coast, the three treatments, weak returns shown and
# hidden, the picker typing a city and choosing its station (the map
# without radar until the first sweep lands, then live), and the keys sheet.
# Frames are grabbed as the scene settles, so it is not a latency
# measurement. The machine's own theme is used. Requires desktop OpenGL,
# Ruby, and FFmpeg; no system edits.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p review docs/media
export OMASTORM_ROOT="$PWD"
site="${SITE:-KJAX}"
demo_dir="$PWD/target/demo-capture"
scratch=$(mktemp -d /tmp/omastorm-demo-live.XXXXXX)
export XDG_RUNTIME_DIR="$scratch/runtime" XDG_CACHE_HOME="$scratch/cache"
export OMASTORM_CONFIG="$demo_dir/config.toml"
unset OMASTORM_ARCHIVE
rm -rf "$demo_dir"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME" "$demo_dir/shaders" "$demo_dir/frames"
jq -r --arg id "$site" '.sites[] | select(.id==$id) | "center_lat = \(.lat)\ncenter_lon = \(.lon)\nlocked_radar = \"\(.id)\""' engine/data/sites.json > "$OMASTORM_CONFIG"
cleanup() { target/debug/omastorm-engine stop >/dev/null 2>&1 || true; }
trap cleanup EXIT
cp ui/Theme.qml ui/Engine.qml ui/RadarMark.qml ui/RadarMap.qml ui/SitePicker.qml ui/Sites.js ui/KeysSheet.qml ui/Keys.js ui/Timeline.js ui/Config.qml ui/Toml.js ui/Location.js ui/LocationPicker.qml ui/LocationPrompt.qml ui/Remembered.qml ui/PluginSession.qml ui/qmldir "$demo_dir/"
cp ui/shaders/*.qsb "$demo_dir/shaders/"
ruby - "$demo_dir" <<'RUBY'
dir = ARGV.fetch(0)
s = File.read('ui/RadarWindow.qml').sub('Item {', 'ShellRoot {')
harness = <<'QML'
    // Start once the station is live and the backfill has given it a loop.
    Timer { interval: 500; running: true; repeat: true
        onTriggered: if (app.scan && app.scan.scanTime && app.frames.filter(f => f.status === "complete").length >= 8) { running = false; waitTiles.start(); } }
    Timer { id: waitTiles; interval: 5000; onTriggered: { app.resetView(); demo.homeSpan = map.span; demo.advance(); } }
    QtObject {
        id: demo
        property int frame: 0
        property int step: -1
        property int stepFrame: 0
        property real homeSpan: 0
        // The take, step by step: `dur` frames at 30 fps, `enter` once,
        // `each(t)` per frame with t in [0, 1], `until` ending the step
        // early once true (the switched station's first sweep, say).
        readonly property var steps: [
            { dur: 90 },
            { dur: 210, enter: () => engine.send({type: "play"}) },
            { dur: 20, enter: () => { engine.send({type: "pause"}); app.run("newest"); } },
            { dur: 200, each: t => { var tr = Math.pow(Math.sin(Math.PI * t), 2);
                                     map.span = demo.homeSpan * (1 - 0.6 * tr);
                                     map.look(map.siteMx + (28 * tr) / map.kmPerUnit, map.siteMy - (6 * tr) / map.kmPerUnit); } },
            { dur: 60, enter: () => app.treatment = "PIXELS" },
            { dur: 60, enter: () => app.treatment = "STIPPLE" },
            { dur: 45, enter: () => app.treatment = "GLYPHS" },
            { dur: 60, enter: () => app.run("weak") },
            { dur: 30, enter: () => app.run("weak") },
            { dur: 30, enter: () => picker.show("") },
            { dur: 18, enter: () => picker.query = "t" },
            { dur: 18, enter: () => picker.query = "ta" },
            { dur: 50, enter: () => picker.query = "tal" },
            { dur: 360, enter: () => picker.accept(),
              until: () => app.siteId !== Quickshell.env("OMASTORM_DEMO_HOME") && app.scan && app.scan.scanTime !== "" && app.scan.status === "complete" },
            { dur: 75 },
            { dur: 90, enter: () => app.run("help") },
            { dur: 30, enter: () => sheet.close() }
        ]
        function advance() {
            if (engine.error) throw new Error("Engine: " + engine.error);
            var s = steps[step];
            if (step < 0 || stepFrame >= s.dur || (s.until && stepFrame > 30 && s.until())) {
                step++; stepFrame = 0;
                if (step === steps.length) { console.log("DEMO_LIVE_PASSED " + frame + " frames"); Qt.quit(); return; }
                s = steps[step];
                if (s.enter) s.enter();
            }
            if (s.each) s.each(stepFrame / Math.max(1, s.dur - 1));
            stepFrame++;
            settle.start();
        }
        function capture() {
            surface.grabToImage(result => {
                var path = Quickshell.env("OMASTORM_DEMO_FRAMES") + "/" + String(frame).padStart(4, "0") + ".png";
                if (!result.saveToFile(path)) throw new Error("Failed to save " + path);
                frame++;
                advance();
            });
        }
    }
    Timer { id: settle; interval: 32; onTriggered: demo.capture() }
QML
s.sub!('    id: app', "    id: app\n" + harness)
File.write(File.join(dir, 'shell.qml'), s)
RUBY
export OMASTORM_QML="$demo_dir/shell.qml"
export OMASTORM_WIDTH=1280 OMASTORM_HEIGHT=720
export OMASTORM_DEMO_FRAMES="$demo_dir/frames" OMASTORM_DEMO_HOME="$site"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic
export QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
unset OMASTORM_CAPTURE
timeout 900 bash run.sh > "$demo_dir/capture.log" 2>&1
rg -q DEMO_LIVE_PASSED "$demo_dir/capture.log" || { tail -20 "$demo_dir/capture.log" >&2; echo "The take did not finish" >&2; exit 1; }
rg DEMO_LIVE_PASSED "$demo_dir/capture.log"
ffmpeg -hide_banner -loglevel error -y -framerate 30 -i "$demo_dir/frames/%04d.png" \
  -c:v libx264 -preset slow -crf 19 -pix_fmt yuv420p -movflags +faststart docs/media/omastorm-demo.mp4
ffprobe -v error -show_entries stream=width,height,r_frame_rate,nb_frames:format=duration,size -of json docs/media/omastorm-demo.mp4 > review/demo-validation.json
# Short animated README preview: the live home view and the zoom to the coast.
ffmpeg -hide_banner -loglevel error -y -i docs/media/omastorm-demo.mp4 \
  -filter_complex "[0:v]select='lt(t,4)+between(t,13.5,17.5)',setpts=N/30/TB,fps=8,scale=640:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff:max_colors=96[p];[b][p]paletteuse=dither=bayer:bayer_scale=5" \
  -loop 0 docs/media/omastorm-preview.gif
grep -h "^Live\|backfilled [0-9]" "$XDG_RUNTIME_DIR/omastorm/engine.log" | sed 's/^/  engine: /' | head -6
echo "docs/media/omastorm-demo.mp4 · omastorm-preview.gif"
