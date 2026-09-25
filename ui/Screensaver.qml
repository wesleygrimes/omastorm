import QtQuick
import QtQuick.Layouts
import Quickshell
import "Metar.js" as Metar

// Fullscreen picture for an idle screen (DESIGN.md, screensaver). Not the
// lock screen. One window per monitor, the remembered view, live frames.
// [metar] show draws aviation chips. A key or a click exits, so chips are
// not buttons and the raw METAR card is never opened.
Item {
    id: root
    readonly property var store: PluginSession
    Theme { id: themeInputs }
    readonly property var theme: themeInputs.snapshot

    function exitSaver() { Qt.quit(); }
    function boot() {
        if (store.engine.state && store.ready) store.initialize();
    }
    Component.onCompleted: {
        store.applyTreatment();
        boot();
    }
    Connections {
        target: store.engine
        function onStateChanged() { root.boot(); }
    }
    Connections {
        target: store.config
        function onReadyChanged() { root.boot(); }
    }
    Connections { target: Quickshell; function onLastWindowClosed() { Qt.quit(); } }

    Variants {
        model: Quickshell.screens
        FloatingWindow {
            id: win
            required property var modelData
            screen: modelData
            title: "Omastorm"
            visible: true
            fullscreen: true
            color: root.theme.background
            implicitWidth: modelData.width || 960
            implicitHeight: modelData.height || 680

            Engine { id: engine }
            property var metars: []
            readonly property var state: engine.state
            readonly property var scan: state ? state.frame : null
            readonly property string siteId: engine.selectedSiteId
            readonly property string siteName: engine.site ? engine.site.name.toUpperCase() : (engine.source && !engine.site ? engine.source.name.toUpperCase() : "")
            readonly property string sourceBadge: state ? state.mode.toUpperCase() : ""
            readonly property var frames: state ? state.timeline : []
            readonly property var newestComplete: {
                var done = frames.filter(function(f) { return f.status === "complete"; });
                return done.length ? done[done.length - 1] : null;
            }
            readonly property string condition: state && state.mode === "live" ? state.connection.status : ""
            readonly property bool alert: condition !== "" && condition !== "ok" && condition !== "idle"
            readonly property color statusLightColor: {
                if (!state) return root.theme.foreground;
                if (state.mode === "archived") return root.theme.accent;
                if (condition === "stale") return root.theme.yellow;
                if (condition === "unavailable" || condition === "offline") return root.theme.red;
                return root.theme.accent;
            }
            readonly property int shownAge: !state || !scan || !scan.scanTime || !newestComplete ? -1
                : Math.max(0, state.connection.ageSeconds + Math.round((Date.parse(newestComplete.scanTime) - Date.parse(scan.scanTime)) / 1000))
            function ago(seconds) {
                var m = Math.floor(seconds / 60);
                if (m < 1) return "Now";
                if (m < 60) return m + " min ago";
                var h = Math.floor(m / 60);
                return h < 24 ? h + "h " + (m % 60) + "m ago" : Math.floor(h / 24) + "d " + (h % 24) + "h ago";
            }
            readonly property string ageText: condition && shownAge >= 0 ? ago(shownAge) : ""
            readonly property int bands: scan ? scan.palette.length : 0
            readonly property bool floorActive: !!scan && root.store.weakFloor !== null && scan.scale > 0
            readonly property real floorFraction: {
                if (!floorActive || root.store.weakFloor <= scan.bounds[0]) return 0;
                for (var i = 0; i < bands; i++)
                    if (root.store.weakFloor < scan.bounds[i + 1])
                        return (i + (root.store.weakFloor - scan.bounds[i]) / (scan.bounds[i + 1] - scan.bounds[i])) / bands;
                return 1;
            }
            property bool viewApplied: false
            function applyView() {
                if (!root.store.hasView) return;
                map.holdSpan = true;
                map.lookAt(root.store.centerLat, root.store.centerLon);
                map.span = root.store.span;
                Qt.callLater(function() { map.holdSpan = false; });
            }
            onStateChanged: {
                if (!state) viewApplied = false;
                else if (!viewApplied) { applyView(); viewApplied = true; }
                requestMetars();
            }
            function requestMetars() {
                if (!Metar.shouldQuery(state, root.store.metarEnabled, engine.site, engine.source)) {
                    metars = [];
                    return;
                }
                engine.send(Metar.command(engine.site, map.viewBbox(), root.store.config.values));
            }
            onSiteIdChanged: requestMetars()
            Connections {
                target: root.store
                function onViewChanged() { win.applyView(); }
                function onMetarEnabledChanged() { win.requestMetars(); }
            }
            Connections {
                target: engine
                function onMetarsReady(message) { win.metars = message.results || []; }
                function onTileReady(tile) { map.tileReady(tile); }
            }
            Timer {
                interval: 600000
                repeat: true
                running: root.store.metarEnabled && Metar.available(win.state, engine.site, engine.source)
                onTriggered: win.requestMetars()
            }

            component LabelText: Text {
                color: root.theme.foreground
                font.family: root.theme.font
                font.pixelSize: root.theme.baseSize
                elide: Text.ElideRight
            }

            ColumnLayout {
                anchors.fill: parent
                spacing: 0
                Item {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    RadarMap {
                        id: map
                        anchors.fill: parent
                        scan: win.scan
                        texture: engine.texture
                        azimuthLut: engine.azimuthLut
                        siteId: win.siteId
                        sourceId: engine.source ? engine.source.id : ""
                        sites: engine.sites
                        coverage: engine.site && engine.site.coverage ? engine.site.coverage
                            : (engine.source && engine.source.coverage ? engine.source.coverage : null)
                        tileRoot: "file://" + engine.runtime
                        theme: root.theme
                        treatment: root.store.treatment
                        weakFloor: root.store.weakFloor
                        labelSize: 12
                        locked: win.state && win.state.navigation ? win.state.navigation.locked : false
                        interactive: false
                        metars: win.metars
                        metarMark: Metar.markFromConfig(root.store.config.values) || "pin"
                        metarMode: root.store.metarEnabled && Metar.available(win.state, engine.site, engine.source) && win.metars.length > 0
                        onTilesNeeded: function(z, x0, y0, x1, y1) {
                            engine.send({type: "tiles_needed", z: z, x0: x0, y0: y0, x1: x1, y1: y1});
                        }
                        onWidthChanged: if (width > 1) win.requestMetars()
                        Component.onCompleted: win.applyView()
                    }
                    Row {
                        anchors.top: parent.top
                        anchors.left: parent.left
                        anchors.margins: 16
                        spacing: 10
                        visible: !!win.state
                        Rectangle {
                            width: 8; height: 8; radius: 4
                            anchors.verticalCenter: parent.verticalCenter
                            color: win.statusLightColor
                        }
                        LabelText {
                            text: win.sourceBadge
                            color: root.theme.accent
                            font.letterSpacing: 1.5
                            anchors.verticalCenter: parent.verticalCenter
                        }
                        LabelText {
                            text: win.siteId || (engine.source ? engine.source.id : "")
                            font.bold: true
                            anchors.verticalCenter: parent.verticalCenter
                        }
                        LabelText {
                            text: win.siteName
                            opacity: .65
                            anchors.verticalCenter: parent.verticalCenter
                        }
                        LabelText {
                            text: win.ageText
                            opacity: .75
                            anchors.verticalCenter: parent.verticalCenter
                        }
                    }
                    Rectangle {
                        id: scaleBar
                        anchors.bottom: parent.bottom
                        anchors.left: parent.left
                        anchors.margins: 12
                        visible: !!win.scan && map.pixelsPerKm > 0
                        readonly property bool metric: Qt.locale().measurementSystem === Locale.MetricSystem
                        readonly property real kmPerMile: 1.609344
                        readonly property var steps: metric
                            ? [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000]
                            : [0.5, 1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000]
                        readonly property real barPx: 72
                        property real nice: metric ? 25 : 10
                        property bool wasMetric: metric
                        width: barPx + 16
                        height: 28
                        color: Qt.alpha(root.theme.background, .9)
                        function nearest(raw) {
                            var best = steps[0], err = Math.abs(steps[0] - raw);
                            for (var i = 1; i < steps.length; i++) {
                                var e = Math.abs(steps[i] - raw);
                                if (e < err) { err = e; best = steps[i]; }
                            }
                            return best;
                        }
                        function stabilize() {
                            if (map.pixelsPerKm <= 0) return;
                            if (wasMetric !== metric) { wasMetric = metric; nice = metric ? 25 : 10; }
                            var exactKm = barPx / map.pixelsPerKm;
                            var exact = metric ? exactKm : exactKm / kmPerMile;
                            if (Math.abs(exact - nice) <= nice * 0.35) return;
                            nice = nearest(exact);
                        }
                        Connections {
                            target: map
                            function onPixelsPerKmChanged() { scaleBar.stabilize(); }
                            function onSpanChanged() { scaleBar.stabilize(); }
                        }
                        Component.onCompleted: stabilize()
                        onVisibleChanged: if (visible) stabilize()
                        LabelText {
                            anchors.horizontalCenter: parent.horizontalCenter
                            anchors.top: parent.top
                            anchors.topMargin: 2
                            font.pixelSize: 10
                            opacity: .75
                            text: {
                                var u = scaleBar.metric ? "km" : "mi";
                                var n = scaleBar.nice;
                                if (scaleBar.metric && n >= 1000) return (n / 1000) + "k " + u;
                                return n + " " + u;
                            }
                        }
                        Rectangle {
                            anchors.left: parent.left
                            anchors.right: parent.right
                            anchors.bottom: parent.bottom
                            anchors.margins: 8
                            anchors.bottomMargin: 6
                            height: 1
                            color: Qt.alpha(root.theme.foreground, .65)
                        }
                    }
                    LabelText {
                        anchors.bottom: parent.bottom
                        anchors.right: parent.right
                        anchors.margins: 14
                        text: "© OpenStreetMap"
                        visible: !!win.scan
                        font.pixelSize: 10
                        opacity: .55
                    }
                    LabelText {
                        anchors.centerIn: parent
                        width: parent.width - 48
                        wrapMode: Text.Wrap
                        horizontalAlignment: Text.AlignHCenter
                        text: map.error || engine.error
                        visible: text.length > 0
                    }
                }
                Item {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 22
                    Layout.minimumHeight: 22
                    Layout.maximumHeight: 22
                    visible: win.bands > 0
                    Row {
                        id: legendRow
                        anchors.fill: parent
                        spacing: 0
                        Repeater {
                            model: win.scan ? win.scan.palette : []
                            Rectangle {
                                required property string modelData
                                required property int index
                                width: legendRow.width / Math.max(1, win.bands)
                                height: 6
                                color: modelData
                            }
                        }
                    }
                    Rectangle {
                        visible: win.floorFraction > 0
                        width: Math.round(legendRow.width * win.floorFraction)
                        height: 6
                        anchors.verticalCenter: parent.verticalCenter
                        color: Qt.alpha(root.theme.background, .8)
                    }
                    LabelText {
                        anchors.right: parent.right
                        anchors.bottom: parent.bottom
                        font.pixelSize: 10
                        opacity: .7
                        text: win.scan ? win.scan.units : ""
                    }
                }
            }
            MouseArea {
                anchors.fill: parent
                z: 10
                acceptedButtons: Qt.AllButtons
                onPressed: function(mouse) { root.exitSaver(); }
                onWheel: function(wheel) { root.exitSaver(); }
            }
            Item {
                anchors.fill: parent
                focus: true
                Keys.onPressed: function(event) { root.exitSaver(); event.accepted = true; }
                Component.onCompleted: forceActiveFocus()
            }
        }
    }
}
