import QtQuick
import Quickshell

ShellRoot {
    id: test
    Panel { id: panel }
    FloatingWindow {
        visible: true; implicitWidth: 380; implicitHeight: 500
        Item {
            id: picture; anchors.fill: parent
            Popover { id: popover; width: 308; session: PluginSession }
        }
    }
    property int stage: 0
    property int ticks: 0
    property bool unverifiable: Quickshell.env("OMASTORM_TEST_UNVERIFIABLE") === "1"
    function cancelPending() {
        if (stage !== 5 || !panel.testMap.testCheck.pending || !popover.testMap.testCheck.pending) return;
        // Both probes have started a grab. Invalidate those callbacks before
        // the next render, then require the replacement to pass its own check.
        stage = 6;
        test.select("current", "current", 7000);
    }
    Connections {
        target: panel.testMap.testCheck
        function onPendingChanged() { Qt.callLater(test.cancelPending); }
    }
    Connections {
        target: popover.testMap.testCheck
        function onPendingChanged() { Qt.callLater(test.cancelPending); }
    }
    function check(ok, message) { if (!ok) throw new Error(message); }
    function broadcast(message) {
        for (var e of [panel.testEngine, popover.testEngine, PluginSession.engine])
            e.receive(JSON.stringify(message));
    }
    function select(id, filename, width) {
        var frame = {kind: "mosaic", id: id, product: "REF", productName: "Test reflectivity",
            units: "dBZ", scanTime: "2026-09-26T12:00:00Z", status: "complete",
            texture: "tex/" + filename + ".png", width: width, height: 1,
            crs: {kind: "geographic", ellipsoid: {semiMajorM: 6378137, inverseFlattening: 298.257223563}},
            geotransform: [-130, 0.01, 0, 55, 0, -0.01], palette: ["#34465f", "#4098a5"], bounds: [-32, 0, 96]};
        broadcast({v: 2, type: "state", mode: "live", frame: frame,
            timeline: [{id: id, scanTime: frame.scanTime, status: "complete"}], playing: false,
            navigation: {follow: false, locked: true},
            selection: {sourceId: "fixture-mosaic", target: {kind: "mosaic"}},
            connection: {status: "ok", ageSeconds: 0}});
    }
    function ready(id) {
        return [panel, popover].every(view => view.testMap.radarReady
            && view.testMap.renderScan.id === id && !view.testMap.error && !view.testError.visible);
    }
    function rejected(message) {
        return [panel, popover].every(view => !view.testMap.radarReady
            && view.testMap.error.indexOf(message) >= 0 && view.testError.visible
            && view.testError.text === view.testMap.error);
    }
    Timer {
        interval: 100; running: true; repeat: true
        onTriggered: {
            try {
                if (++ticks > 120) throw new Error("Texture guard timed out at stage " + stage);
                if (stage === 0) {
                    broadcast({v: 2, type: "hello", sources: [{id: "fixture-mosaic", name: "Mosaic fixture", attribution: "Generated test raster"}], sites: []});
                    panel.open("{}");
                    test.select("native", "native", 7000);
                } else if (stage === 1) {
                    if (test.unverifiable) {
                        if (!test.rejected("Could not verify the radar texture")) return;
                        console.log("TEXTURE_GUARD_PASSED");
                        Qt.quit();
                        return;
                    }
                    if (!test.ready("native")) return;
                    test.select("oversize", "oversize", 131072);
                } else if (stage === 2) {
                    if (!test.rejected("native 131072 × 1")) return;
                    // Exercise the actual visible card error with valid engine state.
                    panel.testSurface.grabToImage(windowResult => {
                        windowResult.saveToFile(Quickshell.env("OMASTORM_REVIEW") + "/texture-guard-window.png");
                        picture.grabToImage(result => {
                            result.saveToFile(Quickshell.env("OMASTORM_REVIEW") + "/texture-guard-popover.png");
                            test.select("native", "native", 7000);
                        });
                    });
                } else if (stage === 3) {
                    if (!test.ready("native")) return;
                    test.select("mismatch", "mismatch", 6999);
                } else if (stage === 4) {
                    if (!test.rejected("dimensions do not match")) return;
                    test.select("cancel", "oversize", 131072);
                } else if (stage === 5) {
                    return; // cancelPending waits for both asynchronous probes.
                } else if (stage === 6) {
                    if (!test.ready("current")) return;
                    panel.close(); popover.visible = false;
                    test.select("hidden", "oversize", 131072);
                } else if (stage === 7) {
                    test.check(!panel.testMap.error && !popover.testMap.error, "Hidden view failed verification");
                    test.select("reopened", "reopened", 7000);
                    panel.open("{}"); popover.visible = true;
                } else if (stage === 8) {
                    if (!test.ready("reopened")) return;
                    console.log("TEXTURE_GUARD_PASSED");
                    Qt.quit();
                }
                stage++;
            } catch (e) { console.error(e); Qt.quit(); }
        }
    }
}
