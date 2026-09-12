#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir="$PWD/target/check-engine-ui"
mkdir -p "$check_dir"
cp ui/Engine.qml "$check_dir/Engine.qml"
cat > "$check_dir/shell.qml" <<'QML'
import QtQuick
import Quickshell
ShellRoot {
    Engine { id: engine }
    function check(ok, message) { if (!ok) throw new Error(message); }
    property string good: ""
    property var tiles: []
    Connections { target: engine; function onTileReady(tile) { tiles.push(tile); } }
    Timer {
        interval: 1000; running: true
        onTriggered: {
            check(!!engine.state, "No socket state received");
            check(engine.sites.length === 180, "No site table received");
            check(engine.texture.indexOf("file://") === 0, "No engine texture");
            check(engine.azimuthLut.indexOf("file://") === 0 && engine.azimuthLut !== engine.texture, "No engine azimuth lookup");
            check(engine.state.frame.rays > 0 && engine.state.frame.gates > 0 && engine.state.frame.gateSpacingM > 0, "No sweep geometry in state");
            var basemap = engine.state.basemap;
            check(!!basemap && basemap.ne.version.length > 0 && ["ok", "offline", "unavailable"].indexOf(basemap.osm.status) >= 0
                  && basemap.osm.source.length > 0 && typeof basemap.osm.version === "string" && basemap.osm.attribution.indexOf("OpenStreetMap") >= 0,
                  "No basemap sources in state: " + JSON.stringify(basemap));
            good = JSON.stringify(engine.state);
            engine.receive("bad json");
            check(engine.state === null && engine.texture === "" && engine.azimuthLut === "", "Malformed state still renders");
            engine.receive(good);
            check(!!engine.state && engine.error === "", "Valid state did not recover");
            var traversal = JSON.parse(good);
            traversal.frame.texture = "tex/../x.png";
            engine.receive(JSON.stringify(traversal));
            check(engine.state === null && engine.texture === "", "Parent segment in texture path still renders");
            var lutTraversal = JSON.parse(good);
            lutTraversal.frame.azimuthLut = "tex/a/b.png";
            engine.receive(JSON.stringify(lutTraversal));
            check(engine.state === null && engine.azimuthLut === "", "Extra segment in azimuth lookup path still renders");
            var renamed = JSON.parse(good);
            renamed.frame.texture = "tex/sweep-TEST-r1.png";
            renamed.frame.azimuthLut = "tex/azlut-TEST-r1.png";
            engine.receive(JSON.stringify(renamed));
            check(!!engine.state && engine.texture.indexOf("/omastorm/tex/sweep-TEST-r1.png") > 0 && engine.azimuthLut.indexOf("/omastorm/tex/azlut-TEST-r1.png") > 0, "Free one-segment texture names were rejected");
            engine.receive(good);
            check(!!engine.state && engine.error === "", "Fixture state did not recover after renamed textures");
            // A rejection is this client's own: it sits beside state, survives
            // a state broadcast, and clears when this client sends again.
            engine.receive('{"type":"error","v":1,"command":"select_site","message":"Not here"}');
            check(!!engine.state && engine.rejection === "Not here" && engine.error === "", "Rejection did not sit beside state");
            engine.receive(good);
            check(!!engine.state && engine.rejection === "Not here", "A state broadcast cleared this client's rejection");
            engine.send({type: "pause"});
            check(engine.rejection === "", "Sending a command did not clear the previous rejection");
            // The tile path rule: a parent segment or a fifth level is transport
            // trouble like a bad texture path; a well-formed reply reaches the map.
            engine.receive('{"type":"tile_ready","v":1,"set":"ne","z":5,"x":7,"y":12,"path":"tiles/ne/5/../12-a.png","labels":[]}');
            check(engine.state === null && engine.error.indexOf("Invalid engine message") === 0 && tiles.length === 0, "Parent segment in tile path was accepted");
            engine.receive(good);
            engine.receive('{"type":"tile_ready","v":1,"set":"ne","z":5,"x":7,"y":12,"path":"tiles/ne/5/7/12/a.png","labels":[]}');
            check(engine.state === null && tiles.length === 0, "Extra segment in tile path was accepted");
            engine.receive(good);
            engine.receive('{"type":"tile_ready","v":1,"set":"foo","z":5,"x":7,"y":12,"path":"tiles/foo/5/7/12-a.png","labels":[]}');
            check(engine.state === null && tiles.length === 0, "Unknown tile set was accepted");
            engine.receive(good);
            engine.receive('{"type":"tile_ready","v":1,"set":"osm","z":11,"x":470,"y":808,"path":"tiles/osm/11/470/808-3f9a1c2e.png","labels":[]}');
            check(!!engine.state && engine.error === "" && tiles.length === 1 && tiles[0].path === "tiles/osm/11/470/808-3f9a1c2e.png", "Valid tile_ready did not reach the map");
            // Round trips through the real daemon: a station outside the
            // table is rejected (a table station would go live and reach the
            // network), and the four z5 tiles around KTLX come back as ne masks.
            engine.send({type: "select_site", id: "XXXX"});
            engine.send({type: "tiles_needed", z: 5, x0: 7, y0: 12, x1: 8, y1: 13});
        }
    }
    Timer {
        interval: 2000; running: true
        onTriggered: {
            check(!!engine.state && engine.state.site.id === "KTLX", "Rejected select_site changed state");
            check(engine.rejection.indexOf("XXXX") >= 0, "Daemon did not answer the rejected command to this client: " + JSON.stringify(engine.rejection));
            var served = tiles.filter(t => t.set === "ne" && t.z === 5 && t.path.indexOf("tiles/ne/5/") === 0);
            check(served.length === 4, "Daemon did not answer tiles_needed with four ne tiles: " + tiles.length);
            engine.receive('{"type":"state","v":99}');
            check(engine.incompatible && engine.state === null && engine.texture === "", "Version mismatch still renders");
            engine.receive(good);
            check(engine.state === null, "Version mismatch was not latched");
            console.log("ENGINE_UI_PASSED");
            Qt.quit();
        }
    }
    Timer { interval: 5000; running: true; onTriggered: Qt.quit() }
}
QML
OMASTORM_QML="$check_dir/shell.qml" QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic \
  bash run.sh > "$check_dir/result.log" 2>&1
cat "$check_dir/result.log"
rg -q ENGINE_UI_PASSED "$check_dir/result.log"
