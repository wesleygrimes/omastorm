import QtQuick
import Quickshell

ShellRoot {
    RadarWindow { id: app }
    property var original
    property var geometry
    property var heldFrame
    property int stage: 0
    function check(ok, message) { if (!ok) throw new Error(message); }
    function copy(value) { return JSON.parse(JSON.stringify(value)); }
    function position() {
        var m = app.testMap;
        var p = m.mapToItem(app.testSurface, 0, 0);
        return [p.x, p.y, m.width, m.height, m.viewCenterX, m.viewCenterY, m.worldPixels];
    }
    function stable() {
        var next = position();
        check(next.every((v, i) => Math.abs(v - geometry[i]) < 1e-6),
              "Loading or hand-off moved the map: " + geometry + " -> " + next);
    }
    function capture(name, after) {
        app.testSurface.grabToImage(result => {
            result.saveToFile(Quickshell.env("OMASTORM_REVIEW") + "/handoff-" + name + ".png");
            if (after) after();
        });
    }
    Timer {
        interval: 500; repeat: true; running: true
        onTriggered: {
            try {
                var engine = app.testEngine, map = app.testMap;
                if (stage === 0) {
                    if (!map.radarReady) return;
                    original = copy(engine.state);
                    // Freeze the socket so deliberately delayed images cannot
                    // be overwritten by a daemon heartbeat during the check.
                    engine.reconnect.running = false;
                    engine.socket.connected = false;
                    engine.state = copy(original);
                    engine.error = "";
                } else if (stage === 1) {
                    geometry = position();
                    heldFrame = map.renderScan;
                    capture("ready");
                    var next = copy(original);
                    next.frame.texture = "tex/delayed-handoff.png";
                    next.frame.azimuthLut = "tex/delayed-lookup.png";
                    next.frame.rays = 17;
                    next.frame.gates = 23;
                    engine.state = next;
                } else if (stage === 2) {
                    stable();
                    check(map.radarReady && map.renderScan === heldFrame,
                          "An undecoded sweep replaced the visible geometry");
                    // Pixels ready but lookup still delayed: retain the full
                    // previous frame, including its original rays and gates.
                    var next = copy(original);
                    next.frame.azimuthLut = "tex/delayed-lookup.png";
                    next.frame.rays = 17;
                    engine.state = next;
                } else if (stage === 3) {
                    stable();
                    check(map.renderScan === heldFrame, "Sweep presented without its azimuth lookup");
                    var loading = copy(original);
                    loading.frame.scanTime = "";
                    loading.timeline = [];
                    loading.connection.status = "loading";
                    engine.state = loading;
                } else if (stage === 4) {
                    stable();
                    check(!map.radarReady, "Loading placeholder drew old radar");
                    capture("loading", () => { engine.state = copy(original); });
                } else if (stage === 5) {
                    stable();
                    check(map.radarReady, "The first scan failed to appear");
                    var other = copy(original);
                    other.selection.target.siteId = "KAMX";
                    other.frame.site.id = "KAMX";
                    other.frame.site.lat = 25.611;
                    other.frame.site.lon = -80.413;
                    other.frame.texture = "tex/delayed-handoff.png";
                    engine.state = other;
                } else if (stage === 6) {
                    stable();
                    check(!map.radarReady, "Previous station's texture leaked into the new station");
                    var idle = copy(original);
                    idle.frame = null;
                    idle.selection = null;
                    idle.timeline = [];
                    idle.connection.status = "idle";
                    engine.state = idle;
                } else if (stage === 7) {
                    stable();
                    check(!map.radarReady, "Idle view retained radar");
                    engine.state = copy(original);
                } else if (stage === 8) {
                    stable();
                    check(map.radarReady, "Radar failed to recover after hand-off");
                    capture("recovered");
                } else {
                    console.log("RADAR_HANDOFF_PASSED");
                    Qt.quit();
                }
                stage++;
            } catch (e) { console.error(e); Qt.quit(); }
        }
    }
    Timer { interval: 12000; running: true; onTriggered: { console.error("Hand-off check timed out"); Qt.quit(); } }
}
