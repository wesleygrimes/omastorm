import QtQuick
import Quickshell

ShellRoot {
    Engine { id: engine; onTileReady: tile => map.tileReady(tile) }
    FloatingWindow {
        id: window
        implicitWidth: 1920; implicitHeight: 1200; color: "#101820"
        Rectangle {
            id: capture
            width: 960; height: 680
            color: map.theme.background
            RadarMap {
                id: map
                anchors.fill: parent
                theme: ({font:"monospace", foreground:"#eeeeee", accent:"#5599ee", background:"#101820"})
                scan: engine.state ? engine.state.frame : null
                texture: engine.texture; azimuthLut: engine.azimuthLut
                sites: engine.sites
                siteId: engine.selectedSiteId
                tileRoot: "file://" + engine.runtime
                onTilesNeeded: (z,x0,y0,x1,y1) => engine.send({type:"tiles_needed",z:z,x0:x0,y0:y0,x1:x1,y1:y1})
            }
            Text {
                x: 12; y: 8; color: "#eeeeee"; font.family: "monospace"; font.pixelSize: 12
                text: (views[index] ? views[index].name.toUpperCase() : "") + "\nARCHIVED KTLX 2013-05-20"
            }
            Text {
                x: 12; anchors.bottom: parent.bottom; anchors.bottomMargin: 8
                color: "#eeeeee"; font.family: "monospace"; font.pixelSize: 10
                text: "Active radar: nominal 460 km · NOAA NEXRAD\nNatural Earth · " + (map.osmOnScreen && engine.state ? engine.state.basemap.osm.attribution : "© OpenStreetMap contributors (ODbL)")
            }
        }
    }
    property var views: [
        {name:"continental", lon:-100, lat:39, span:4600, width:960, height:680},
        {name:"continental-minimum", lon:-100, lat:39, span:4600, width:360, height:360},
        {name:"continental-full", lon:-100, lat:39, span:4600, width:1920, height:1200},
        {name:"alaska", lon:-153, lat:64, span:3800, width:960, height:680},
        {name:"hawaii", lon:-157, lat:20.5, span:1400, width:960, height:680},
        {name:"puerto-rico", lon:-66.5, lat:18, span:1000, width:960, height:680},
        {name:"guam", lon:145, lat:14, span:1400, width:960, height:680},
        {name:"date-line-west", lon:-179, lat:60, span:3800, width:960, height:680},
        {name:"date-line-east", lon:179, lat:52, span:3800, width:960, height:680},
        {name:"world", lon:0, lat:0, span:1e9, width:960, height:680}
    ]
    property int index: -1
    property int ticks: 0
    property bool saving: false
    function check(ok, message) { if (!ok) throw new Error(message); }
    function next() {
        index++; ticks = 0;
        if (index === views.length) { console.log("MAP_NETWORK_PASSED"); Qt.quit(); return; }
        var v = views[index];
        capture.width = v.width; capture.height = v.height;
        map.center = Qt.point(v.lon,v.lat); map.zoom(v.span);
    }
    Timer {
        interval: 250; repeat: true; running: true
        onTriggered: {
            if (saving || !map.scan) return;
            try {
                if (index < 0) { next(); return; }
                ticks++;
                check(!engine.rejection && !engine.error && !map.error, engine.rejection || engine.error || map.error);
                if (ticks < 4) return;
                check(map.coverageSites.length === 1 && map.coverageSites[0].id === map.siteId,
                    "Inactive radar coverage appeared at network zoom");
                check(map.width === views[index].width && map.height === views[index].height, "Capture size not applied");
                var halfX = map.width/2*map.unitsPerPixel, wantedX = map.mercatorX(views[index].lon);
                var expectedX = Math.max(halfX, Math.min(1-halfX, wantedX));
                check(Math.abs(map.viewCenterX-expectedX)<1e-9, "Camera longitude is not the requested or edge-clamped position");
                check(map.viewCenterX-halfX>=-1e-9 && map.viewCenterX+halfX<=1+1e-9, "Longitude exposed outside pyramid");
                if (views[index].name.startsWith("date-line")) check(Math.abs(expectedX-wantedX)>1e-3, "Date-line view did not reach the clamp");
                check(map.viewCenterY-map.height/2*map.unitsPerPixel>=-1e-9
                    && map.viewCenterY+map.height/2*map.unitsPerPixel<=1+1e-9, "Latitude exposed outside pyramid");
                var ready = map.displayedLevel === map.request.z && Object.keys(map.tiles).length > 0
                    && Object.keys(map.tiles).every(k => map.tiles[k].ready);
                if (!ready) { check(ticks < 100, "Tile loading timeout"); return; }
                check(Object.keys(map.tiles).length <= 64, "Unbounded tile residency");
                var n = Math.pow(2, map.request.z);
                if (views[index].name.startsWith("date-line")) check(map.request.x0 === 0 || map.request.x1 === n-1, "Clamped view does not end at the pyramid edge");
                console.log("NETWORK_VIEW", views[index].name, "tiles", Object.keys(map.tiles).length,
                    "labels", map.siteLabels.length, "zoom", map.request.z);
                saving = true;
                capture.grabToImage(result => {
                    check(result.saveToFile(Quickshell.env("OMASTORM_REVIEW")+"/network-"+views[index].name+".png"), "Capture failed");
                    saving = false; next();
                });
            } catch (e) { console.error(e); Qt.quit(); }
        }
    }
}
