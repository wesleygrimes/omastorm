import QtQuick
import Quickshell

// The home view (docs/protocol.md, configuration). Without a home point the
// camera sits at the station's own offset, 5 km west and 15 km north of the
// site; with one it sits on the point itself, and stays there across a pan,
// a reset, and a hand-off to the home station. Choosing another station by
// hand still centres that station.
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
    function centre() { return {lat: map.centerLat, lon: map.centerLon}; }
    // The station's own home view: 5 km west and 15 km north of the site.
    // The offset is applied in Mercator units taken at the site's latitude,
    // so the ground distance drifts a few metres over the 15 km; 100 m of
    // tolerance still separates it from the home point and from the site.
    readonly property real stationOffsetKm: Math.sqrt(5*5 + 15*15)
    function checkStationHome(site, message) {
        check(Math.abs(distance(centre(), site) - stationOffsetKm) < .1, message + ": wrong distance from the site");
        check(centre().lat > site.lat && centre().lon < site.lon, message + ": the offset is not north-west of the site");
    }
    function checkOnPoint(point, message) {
        check(Math.abs(centre().lat - point.lat) < 1e-9 && Math.abs(centre().lon - point.lon) < 1e-9, message);
    }
    property int stage: 0
    property var site: null
    property var point: null
    Timer {
        interval: 500; repeat: true; running: true
        onTriggered: {
            try {
                if (!map.scan) return;
                if (stage === 0) {
                    site = map.scan.site;
                    // A point well inside the station's range, the case the
                    // station offset cannot reach: a home that is not the site.
                    point = {lat: site.lat + .35, lon: site.lon + .45};
                    check(map.homePoint === null, "The map starts with no home point");
                    checkStationHome(site, "No home point");
                    map.homePoint = point;
                } else if (stage === 1) {
                    checkOnPoint(point, "A home point did not take the home view");
                    check(distance(centre(), site) > 40, "The home view stayed on the station");
                    map.pan(1, 0); map.pan(0, 1);
                } else if (stage === 2) {
                    check(distance(centre(), point) > 1, "The pan did not leave the home point");
                    map.reset();
                } else if (stage === 3) {
                    checkOnPoint(point, "Reset did not return to the home point");
                    // The hand-off to the home station keeps the point.
                    map.jumpHome(site.lat, site.lon);
                } else if (stage === 4) {
                    checkOnPoint(point, "The home hand-off left the home point");
                    // A station chosen by hand is still centred on that station.
                    var other = map.sites.find(s => s.id !== map.siteId && distance(s, site) > 200);
                    check(!!other, "No second station to choose");
                    map.jumpTo(other.lat, other.lon);
                    checkStationHome(other, "Choosing a station ignored its own home view");
                    map.homePoint = null;
                    map.reset();
                } else if (stage === 5) {
                    checkStationHome(site, "Clearing the home point did not restore the station offset");
                    console.log("MAP_HOME_PASSED"); Qt.quit();
                }
                stage++;
            } catch (e) { console.error(e); Qt.quit(); }
        }
    }
    Timer { interval: 15000; running: true; onTriggered: Qt.quit() }
}
