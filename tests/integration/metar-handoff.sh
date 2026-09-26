#!/usr/bin/env bash
# Replay selection changes through both production surfaces without a feed.
set -euo pipefail
# shellcheck source=tests/integration/common.sh
source "$(dirname "$0")/common.sh"
cd "$(dirname "$0")/../.."
scratch=$PWD/target/check-metar-handoff
rm -rf "$scratch"
mkdir -p "$scratch/root"
stage_ui "$scratch/ui"
cp manifest.json "$scratch/root/manifest.json"
printf 'exit 0\n' > "$scratch/root/run.sh"
printf '[metar]\nshow = true\n' > "$scratch/config.toml"
printf '{"lat":37.5,"lon":-95,"span":4000}\n' > "$scratch/state.json"
# Keep the actual protocol decoder and property bindings. Only replace the
# socket transport with an outbound-command counter and expose each engine
# to the test driver. No installed daemon or remote service participates.
ruby - "$scratch/ui" <<'RUBY'
dir = ARGV.fetch(0)
path = "#{dir}/Engine.qml"
stub = lambda { |t, from, to| t.include?(from) ? t.sub(from, to) : abort("Engine.qml changed: #{from}") }
text = stub.(File.read(path), 'connected: true', 'connected: false')
text = stub.(text, 'running: engine.socket && !engine.socket.connected && !engine.incompatible', 'running: false')
text = stub.(text, '    function send(command) {', <<~'QML')
    property int metarQueries: 0
    function send(command) {
        if (command.type === "metar_query") metarQueries++;
        return;
QML
File.write(path, text)
RUBY
cat > "$scratch/ui/Handoff.qml" <<'QML'
import QtQuick
import Quickshell
ShellRoot {
    id: test
    Panel { id: panel }
    FloatingWindow {
        visible: true; implicitWidth: 380; implicitHeight: 500
        Popover { id: popover; width: 308; session: PluginSession }
    }
    function check(ok, message) { if (!ok) throw new Error(message); }
    function broadcast(message) {
        for (var e of [panel.connection, popover.engine, PluginSession.engine])
            e.receive(JSON.stringify(message));
    }
    function select(source, site, condition) {
        broadcast({v: 2, type: "state", mode: "live", frame: null, timeline: [], playing: false,
            navigation: {follow: true, locked: true},
            selection: {sourceId: source, target: site ? {kind: "site", siteId: site} : {kind: "mosaic"}},
            connection: {status: condition || "ok", ageSeconds: 0}});
    }
    Timer {
        interval: 500; running: true
        onTriggered: {
            broadcast({v: 2, type: "hello", sources: [
                {id: "nexrad", name: "NEXRAD", attribution: "NOAA"},
                {id: "mrms-conus", name: "NOAA MRMS", attribution: "NOAA/NSSL MRMS"}],
                sites: [{id: "KTLX", name: "Oklahoma City", lat: 35.333, lon: -97.277}]});
            test.select("mrms-conus", "");
            station.start();
        }
    }
    Timer {
        id: station; interval: 100
        onTriggered: {
            test.check(panel.connection.metarQueries === 0 && popover.engine.metarQueries === 0,
                "MRMS requested airport observations");
            test.select("nexrad", "KTLX");
            returned.start();
        }
    }
    Timer {
        id: returned; interval: 100
        onTriggered: {
            test.check(panel.connection.metarQueries === 1 && popover.engine.metarQueries === 1,
                "Station handoff did not query both surfaces: " + panel.connection.metarQueries + "/" + popover.engine.metarQueries);
            var report = {id: "KOKC", lat: 35.393, lon: -97.601, category: "VFR", raw: "KOKC fixture"};
            panel.metarState.metars = [report]; popover.metarState.metars = [report];
            panel.metarState.selected = report; popover.metarState.selected = report;
            test.select("mrms-conus", "", "stale");
            mosaic.start();
        }
    }
    Timer {
        id: mosaic; interval: 100
        onTriggered: {
            test.check(panel.metarState.metars.length === 0 && popover.metarState.metars.length === 0
                && !panel.metarState.selected && !popover.metarState.selected, "Mosaic retained station observations");
            test.check(panel.connection.metarQueries === 1 && popover.engine.metarQueries === 1,
                "Mosaic handoff queried airport observations");
            test.check(panel.condition === "stale" && popover.condition === "stale", "Stale mosaic condition lost");
            test.select("mrms-conus", "", "offline");
            offline.start();
        }
    }
    Timer {
        id: offline; interval: 100
        onTriggered: {
            test.check(panel.condition === "offline" && popover.condition === "offline", "Offline mosaic condition lost");
            test.check(popover.statusText.indexOf("OFFLINE") >= 0, "Offline mosaic label lost");
            console.log("METAR_HANDOFF_PASSED");
            Qt.quit();
        }
    }
    Timer { interval: 5000; running: true; onTriggered: Qt.quit() }
}
QML
# Runtime/cache are selected and owned by the shared runner.
export OMASTORM_ROOT="$scratch/root" OMASTORM_CONFIG="$scratch/config.toml" OMASTORM_STATE="$scratch/state.json" OMASTORM_LOCATION=/dev/null
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
timeout 15 quickshell --no-color -p "$scratch/ui/Handoff.qml" > "$scratch/ui.log" 2>&1
rg -q METAR_HANDOFF_PASSED "$scratch/ui.log" || { cat "$scratch/ui.log"; fail 'METAR handoff replay did not pass'; }
check_qml_log "$scratch/ui.log"
echo 'METAR: station handoff in both surfaces, mosaic clearing/eligibility, stale/offline labels PASS'
