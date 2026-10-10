#!/usr/bin/env bash
# A 1200×900 presentation take of the current UI and real live radar data.
# Wait for twelve completed scans before recording. Play a complete loop,
# ease into the storm, and show all three treatments without switching to
# an empty station. Frame grabs advance the presentation at 30 fps; this
# is a product demonstration, not a feed-latency measurement.
# Uses an isolated daemon/cache and never changes the installed plugin.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p review docs/media
export OMASTORM_ROOT="$PWD"
site="${SITE:-KJAX}"
scratch=$(mktemp -d /tmp/omastorm-demo-live.XXXXXX)
demo_dir="$scratch/demo-capture"
cleanup() {
  jobs -pr > "$scratch/.jobs"
  xargs -r kill -KILL < "$scratch/.jobs" 2>/dev/null || true
  wait || true
  XDG_RUNTIME_DIR="$scratch/runtime" target/debug/omastorm-engine stop >/dev/null 2>&1 || true
  rm -rf -- "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
export XDG_RUNTIME_DIR="$scratch/runtime" XDG_CACHE_HOME="$scratch/cache"
export OMASTORM_CONFIG="$demo_dir/config.toml"
unset OMASTORM_ARCHIVE
rm -rf "$demo_dir"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME" "$demo_dir/shaders" "$demo_dir/frames"
jq -r --arg id "$site" '.sites[] | select(.id==$id) | "center_lat = \(.lat)\ncenter_lon = \(.lon)\nlocked_radar = \"\(.id)\""' engine/data/sites.json > "$OMASTORM_CONFIG"
cp ui/Theme.qml ui/Engine.qml ui/RadarMark.qml ui/RadarMap.qml ui/TextureCheck.qml ui/Sites.js ui/KeysSheet.qml ui/Keys.js ui/Config.qml ui/Toml.js ui/Location.js ui/Metar.js ui/Metars.qml ui/LocationPicker.qml ui/LocationPrompt.qml ui/Remembered.qml ui/PluginSession.qml ui/qmldir "$demo_dir/"
cp ui/shaders/*.qsb "$demo_dir/shaders/"
ruby - "$demo_dir" <<'RUBY'
dir = ARGV.fetch(0)
s = File.read('ui/RadarWindow.qml').sub('Item {', 'ShellRoot {')
harness = <<'QML'
    // Start once the station is live and the backfill has given it a loop.
    Timer { interval: 500; running: true; repeat: true
        onTriggered: if (app.scan && app.scan.scanTime && app.frames.filter(f => f.status === "complete").length >= 12) { running = false; waitTiles.start(); } }
    Timer { id: waitTiles; interval: 5000; onTriggered: { app.resetView(); demo.homeSpan = map.span; demo.advance(); } }
    QtObject {
        id: demo
        property int frame: 0
        property int step: -1
        property int stepFrame: 0
        property real homeSpan: 0
        // The take: `dur` frames at 30 fps, `enter` once, and
        // `each(t)` per frame with t in [0, 1]. Seeks use real scan IDs
        // so playback advances consistently despite capture overhead.
        property var loopFrames: []
        readonly property var steps: [
            { dur: 75, enter: () => { engine.send({type: "pause"}); demo.loopFrames = app.frames.filter(f => f.status === "complete"); demo.holdComplete(); } },
            { dur: 360, each: t => { if (demo.stepFrame % 24 === 0) { var i = Math.floor(demo.stepFrame / 24) % demo.loopFrames.length; engine.send({type: "seek", id: demo.loopFrames[i].id}); } } },
            { dur: 45, enter: () => demo.holdComplete() },
            { dur: 120, each: t => { var eased = (1 - Math.cos(Math.PI * t)) / 2; map.span = demo.homeSpan * (1 - 0.4 * eased); } },
            { dur: 90, enter: () => app.treatment = "PIXELS" },
            { dur: 90, enter: () => app.treatment = "STIPPLE" },
            { dur: 90, enter: () => app.treatment = "GLYPHS" },
            { dur: 120, each: t => { var eased = (1 - Math.cos(Math.PI * t)) / 2; map.span = demo.homeSpan * (0.6 + 0.4 * eased); } },
            { dur: 60 }
        ]
        function holdComplete() {
            // Stay one completed scan behind the newest, so a new live
            // volume cannot replace the comparison with an unfinished cut.
            engine.send({type: "seek", id: loopFrames[loopFrames.length - 2].id});
        }
        function advance() {
            if (engine.error) throw new Error("Engine: " + engine.error);
            var s = steps[step];
            if (step < 0 || stepFrame >= s.dur) {
                step++; stepFrame = 0;
                if (step === steps.length) { console.log("DEMO_LIVE_PASSED " + frame + " frames; " + loopFrames.length + " completed scans"); Qt.quit(); return; }
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
export OMASTORM_WIDTH=1200 OMASTORM_HEIGHT=900
export OMASTORM_DEMO_FRAMES="$demo_dir/frames"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic
export QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
unset OMASTORM_CAPTURE
timeout 900 bash run.sh > "$demo_dir/capture.log" 2>&1
rg -q DEMO_LIVE_PASSED "$demo_dir/capture.log" || { tail -20 "$demo_dir/capture.log" >&2; echo "The take did not finish" >&2; exit 1; }
rg DEMO_LIVE_PASSED "$demo_dir/capture.log"
ffmpeg -hide_banner -loglevel error -y -framerate 30 -i "$demo_dir/frames/%04d.png" \
  -c:v libx264 -preset slow -crf 19 -pix_fmt yuv420p -movflags +faststart docs/media/omastorm-demo.mp4
ffprobe -v error -show_entries stream=width,height,r_frame_rate,nb_frames:format=duration,size -of json docs/media/omastorm-demo.mp4 > review/demo-validation.json
# Short animated preview of the populated loop.
ffmpeg -hide_banner -loglevel error -y -i docs/media/omastorm-demo.mp4 \
  -filter_complex "[0:v]select='between(t,2.5,10.5)',setpts=N/30/TB,fps=8,scale=640:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff:max_colors=96[p];[b][p]paletteuse=dither=bayer:bayer_scale=5" \
  -loop 0 docs/media/omastorm-preview.gif
grep -h "^Live\|backfilled [0-9]" "$XDG_RUNTIME_DIR/omastorm/engine.log" | sed 's/^/  engine: /' | head -6
echo "docs/media/omastorm-demo.mp4 · omastorm-preview.gif"
