import QtQuick
import Quickshell

ShellRoot {
    Engine { id: engine }
    FloatingWindow {
        implicitWidth: 960; implicitHeight: 680; color: "#101820"
        RadarMap {
            id: map
            anchors.fill: parent
            theme: ({font:"monospace", foreground:"#eeeeee", accent:"#5599ee", background:"#101820"})
            scan: engine.state ? engine.state.frame : null
            texture: engine.texture
            azimuthLut: engine.azimuthLut
            sites: engine.sites
            siteId: engine.state ? engine.state.site.id : ""
        }
    }
    function check(ok, message) { if (!ok) throw new Error(message); }
    function distance(a, b) {
        var r = Math.PI/180, dp = (a.lat-b.lat)*r, dl = (a.lon-b.lon)*r;
        var h = Math.sin(dp/2)**2 + Math.cos(a.lat*r)*Math.cos(b.lat*r)*Math.sin(dl/2)**2;
        return 12742*Math.atan2(Math.sqrt(h), Math.sqrt(1-h));
    }
    property int stage: 0
    property var heldLabels
    function checkCoverageOrigin() {
        check(map.coverageForTest.count === 1, "Only the active radar should have a coverage delegate");
        var item = map.coverageForTest.itemAt(0);
        var origin = item.mapToItem(map, 0, 0);
        check(Math.abs(origin.x-map.sx(map.siteMx))<1e-7
            && Math.abs(origin.y-map.sy(map.siteMy))<1e-7, "Coverage is not attached to the camera origin");
    }
    Timer {
        interval: 500; repeat: true; running: true
        onTriggered: {
            try {
                if (stage === 0) {
                    check(map.sites.length === 180, "Missing engine station table");
                    checkCoverageOrigin();
                    check(map.siteLabels.some(s => s.name === "KOUN"), "Nearby station ID hidden by collision layout");
                    check(!map.siteLabels.some(s => s.name === "KTLX"), "Duplicate active marker label");
                    var active = map.coverageSites.filter(s => s.id === "KTLX");
                    check(active.length === 1 && active[0].lat === map.scan.site.lat, "Active coverage did not use measured location");
                    // Independent inverse-distance check at ordinary, Alaskan,
                    // and date-line coordinates; every destination is 460 km.
                    for (var site of [{lat:35,lon:-97}, {lat:65,lon:-165}, {lat:60,lon:179.8}]) {
                        var points = map.coveragePoints(site);
                        check(points.length === 361, "Incomplete circle");
                        check(points[0].x === points[360].x && points[0].y === points[360].y, "Circle not closed");
                        for (var i=0; i<points.length; i++) {
                            var p = points[i];
                            var at = {lat:map.latitude(p.y+map.siteMy), lon:map.longitude(p.x+map.siteMx)};
                            check(Math.abs(distance(site, at)-460) < 1e-7, "Coverage distance drift");
                            if (i) check(Math.abs(p.x-points[i-1].x)<.01, "Date-line discontinuity");
                        }
                    }
                    var rx = map.overlayHalfX, ry = map.overlayHalfY;
                    var cx = map.overlayX-map.siteMx, cy = map.overlayY-map.siteMy;
                    var clipped = map.clippedCoverage([Qt.point(cx-2*rx,cy), Qt.point(cx+2*rx,cy),
                        Qt.point(cx+2*rx,cy+2*ry), Qt.point(cx,cy+2*ry), Qt.point(cx,cy-2*ry)]);
                    check(clipped.length === 2, "Disconnected arcs joined across the map");
                    check(Math.abs(clipped[0][0].x-(cx-rx)*100000)<1e-7
                        && Math.abs(clipped[0][1].x-(cx+rx)*100000)<1e-7, "Clipping moved the crossing");
                    for (var line of clipped) for (var p of line)
                        check(Math.abs(p.x-cx*100000)<=rx*100000+1e-7 && Math.abs(p.y-cy*100000)<=ry*100000+1e-7, "Unbounded coverage geometry");
                    check(!map.coverageInReach({lat:35,lon:-160})
                        && map.coverageInReach({lat:35,lon:-92}), "Coverage culling lost a nearby arc");
                    heldLabels = map.siteLabels;
                    map.look(map.siteMx+.0001, map.siteMy);
                } else if (stage === 1) {
                    checkCoverageOrigin();
                    check(map.siteLabels === heldLabels, "Pan rebuilt station label layout");
                    map.zoom(60);
                } else if (stage === 2) {
                    checkCoverageOrigin();
                    check(map.siteLabels !== heldLabels && map.siteLabels.some(s => s.name === "KOUN"), "Zoom failed to lay out station IDs");
                    // Reconnecting with no frame must clear both label sets.
                    map.scan = null;
                } else if (stage === 3) {
                    check(map.siteLabels.length === 0 && map.coverageSites.length === 0, "Disconnected overlay retained");
                    map.scan = engine.state.frame;
                    map.reset();
                } else if (stage === 4) {
                    map.grabToImage(result => {
                        check(result.saveToFile(Quickshell.env("OMASTORM_REVIEW")+"/site-overlay.png"), "Capture failed");
                        console.log("MAP_SITES_PASSED"); Qt.quit();
                    });
                }
                stage++;
            } catch (e) { console.error(e); Qt.quit(); }
        }
    }
    Timer { interval: 10000; running: true; onTriggered: Qt.quit() }
}
