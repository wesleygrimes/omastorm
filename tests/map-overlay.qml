import QtQuick
import Quickshell

// A rendered product's picture on the map (`docs/protocol.md`, frames): the
// strips follow the camera's Mercator rectangle for the publisher's ground
// box, carry the map area of the published file rather than its legend panel,
// and the polar sweep stays away. The picture itself is a synthetic two-tone
// file — the map area red, the panel beside it blue — so the capture proves
// both that it is drawn and that the crop keeps the panel out.
ShellRoot {
    FloatingWindow {
        implicitWidth: 900; implicitHeight: 700; color: "#101820"
        RadarMap {
            id: map
            anchors.fill: parent
            theme: ({font: "monospace", foreground: "#eeeeee", accent: "#5599ee", background: "#101820"})
            texture: Qt.resolvedUrl(".").toString() + "/overlay.png"
            span: 900
            sites: [{id: "HKO", name: "Hong Kong", state: "HK", lat: 22.3579, lon: 114.2177, altM: 583}]
            siteId: "HKO"
            scan: overlayScan
        }
    }
    readonly property var box: ({north: 24.60560, south: 20.00107, east: 116.66013, west: 111.68321})
    readonly property var overlayScan: ({
        id: "HKO-20260915T0630Z", kind: "overlay", source: "HKO 256 KM",
        product: "RATE", productName: "Rain rate (3 km)", units: "mm/h", status: "complete",
        scanTime: "2026-09-15T06:30:00Z", sweepEnd: "2026-09-15T06:30:00Z",
        elevationDeg: 0, rays: 0, gates: 0, firstGateM: 0, gateSpacingM: 0, scale: 0, offset: 0,
        site: {lat: 22.3579, lon: 114.2177, altM: 583},
        palette: ["#00c9fc", "#c5000b"], bounds: [],
        overlay: {north: 24.60560, south: 20.00107, east: 116.66013, west: 111.68321,
                  crop: {x: 0, y: 0, width: 398, height: 395},
                  levels: [0.15, 0.5, 300]}
    })
    property int stage: 0
    function check(ok, message) { if (!ok) throw new Error(message); }
    // Each strip must sit where the camera puts its slice of the box, and
    // carry the matching slice of the published file's map area.
    function checkStrip(index) {
        var rect = map.overlayStripRect(index);
        var rows = map.overlayStripRows(index);
        var n = map.overlayStrips;
        var top = box.north + (box.south - box.north) * (index / n);
        var bottom = box.north + (box.south - box.north) * ((index + 1) / n);
        var left = map.sx(map.mercatorX(box.west));
        var right = map.sx(map.mercatorX(box.east));
        check(Math.abs(rect.x - left) < 1e-6 && Math.abs(rect.width - (right - left)) < 1e-6,
            "Strip " + index + " does not span the box's west and east edges");
        check(Math.abs(rect.y - map.sy(map.mercatorY(top))) < 1e-6,
            "Strip " + index + " does not start at its latitude");
        check(Math.abs((rect.y + rect.height) - map.sy(map.mercatorY(bottom))) < 1e-6,
            "Strip " + index + " does not end at its latitude");
        check(rows.x === map.overlayBox.crop.x && rows.width === map.overlayBox.crop.width,
            "Strip " + index + " does not take the map area's columns");
        check(rows.y >= map.overlayBox.crop.y
            && rows.y + rows.height <= map.overlayBox.crop.y + map.overlayBox.crop.height,
            "Strip " + index + " takes rows outside the map area");
        var item = map.overlayForTest.itemAt(index);
        check(!!item, "Strip " + index + " has no image");
        check(Math.abs(item.x - rect.x) < .5 && Math.abs(item.y - rect.y) < .5
            && Math.abs(item.width - rect.width) < .5 && Math.abs(item.height - rect.height) < .5,
            "Strip " + index + " is not drawn where the camera puts it");
        check(item.sourceClipRect.x === rows.x && item.sourceClipRect.y === rows.y
            && item.sourceClipRect.width === rows.width && item.sourceClipRect.height === rows.height,
            "Strip " + index + " does not clip to the map area");
    }
    Timer {
        interval: 400; repeat: true; running: true
        onTriggered: {
            try {
                if (stage === 0) {
                    check(map.overlayFrame, "The frame was not taken for a rendered product");
                    check(map.overlayBox.crop.width === 398 && map.overlayBox.crop.height === 395,
                        "The map area did not arrive with the frame");
                    check(map.overlayBox.levels.length === map.scan.palette.length + 1,
                        "Band edges do not match the palette");
                    check(map.coverageSites.length === 0, "A nominal footprint is drawn over a rendered product");
                    check(map.overlayForTest.count === map.overlayStrips, "Not every strip is drawn");
                    for (var i = 0; i < map.overlayStrips; i++) checkStrip(i);
                    // The strips tile the box and the file's map area.
                    var first = map.overlayStripRect(0), last = map.overlayStripRect(map.overlayStrips - 1);
                    check(Math.abs(first.y - map.sy(map.mercatorY(box.north))) < 1e-6
                        && Math.abs((last.y + last.height) - map.sy(map.mercatorY(box.south))) < 1e-6,
                        "The strips do not cover the box");
                    check(map.overlayStripRows(0).y === map.overlayBox.crop.y
                        && map.overlayStripRows(map.overlayStrips - 1).y
                            + map.overlayStripRows(map.overlayStrips - 1).height
                            === map.overlayBox.crop.y + map.overlayBox.crop.height,
                        "The strips do not cover the map area");
                    // A sweep is not a rendered product: the polar path returns.
                    map.scan = ({site: {lat: 35.33306, lon: -97.27748}, kind: "sweep", palette: ["#34465f"],
                        bounds: [-32, 0], rays: 1, gates: 1, firstGateM: 0, gateSpacingM: 250,
                        elevationDeg: 0.5, scale: 2, offset: 66, scanTime: "2026-09-15T06:30:00Z"});
                } else if (stage === 1) {
                    check(!map.overlayFrame && map.overlayForTest.count === 0,
                        "A sweep frame still draws the overlay");
                    map.scan = overlayScan;
                    map.zoom(500);
                } else if (stage === 2) {
                    // The camera moved: the strips follow it, still tiling.
                    for (var j = 0; j < map.overlayStrips; j++) checkStrip(j);
                    map.grabToImage(r => r.saveToFile(Quickshell.env("OMASTORM_REVIEW") + "/overlay-crop.png"));
                } else if (stage === 3) {
                    console.log("MAP_OVERLAY_PASSED");
                    Qt.quit();
                }
                stage++;
            } catch (e) { console.error(e); Qt.quit(); }
        }
    }
}
