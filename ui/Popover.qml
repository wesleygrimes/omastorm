import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import "Keys.js" as KeyMap
import "Timeline.js" as Timeline
import "Status.js" as Status

FocusScope {
    id: card
    required property var session
    property var theme: session.theme.snapshot
    property alias engine: connection
    readonly property var state: connection.state
    readonly property var scan: state ? state.frame : null
    readonly property var frames: state ? state.timeline : []
    readonly property var slots: Timeline.slots(frames)
    // The feed condition (Status.js): the engine's judgement, "offline"
    // while there is no engine. The headline says LIVE only under ok; a
    // feed that is not delivering says NOT UPDATING with the last sweep's
    // time, and `detail` under the map names the condition and what to do.
    readonly property string condition: state ? Status.condition(state) : "offline"
    readonly property bool cached: Status.notUpdating(condition)
    readonly property color statusColor: condition === "stale" ? theme.yellow
        : condition === "offline" || condition === "unavailable" ? theme.red : theme.accent
    readonly property string statusText: Status.headline(state, scan)
    readonly property string detail: Status.detail(state)
    // What stands in for the radar: no engine yet (the bootstrap's word
    // while it has one; otherwise the engine is starting, since the session
    // relaunches it while the socket stays quiet, and a transport fault
    // that will not clear by itself is shown as such), or a GPU shader the
    // driver refused, which would otherwise read as a clear night under a
    // LIVE badge. The driver log stays out of the card; the window shows it.
    readonly property string notice: !state ? (session.startupError
            || (connection.error && !/Reconnecting/.test(connection.error) ? connection.error : "Starting radar engine…"))
        : map.error ? "Radar overlay failed to draw" : ""
    // The legend in one line, as the window's legend says it: what blank
    // means, the hidden floor when one is in force, and the folded marker.
    readonly property bool floorActive: !!scan && session.weakFloor !== null && scan.scale > 0
    readonly property string legend: !scan || !scan.scanTime ? ""
        : scan.units + " · blank: no return / outside" + (floorActive ? " / <" + session.weakFloor + " " + scan.units + " hidden" : "") + " · X: folded"
    signal expandRequested()
    signal closeRequested()
    implicitWidth: 308
    implicitHeight: layout.implicitHeight
    Engine { id: connection }
    function step(delta) { if (state) connection.send({type: "step", delta: delta}); }
    function play() { if (state) connection.send({type: state.playing ? "pause" : "play"}); }
    // Respect the same config keys as the window; Enter always expands.
    Shortcut { id: probe; enabled: false }
    function canon(sequence) { probe.sequence = sequence; return probe.portableText; }
    property var bindings: ({})
    function applyKeys() { bindings = KeyMap.resolve(session.config.keys, canon).bindings; }
    Component.onCompleted: applyKeys()
    Connections { target: card.session.config; function onKeysChanged() { card.applyKeys(); } }
    Instantiator {
        model: ["previous_frame", "next_frame", "play", "close"]
        delegate: Shortcut {
            required property string modelData
            sequences: card.bindings[modelData] || []
            enabled: card.visible
            onActivated: {
                if (modelData === "close") card.closeRequested();
                else if (modelData === "play") card.play();
                else card.step(modelData === "previous_frame" ? -1 : 1);
            }
        }
    }
    Keys.onReturnPressed: expandRequested()
    Keys.onEnterPressed: expandRequested()
    component Label: Text {
        color: card.theme.foreground
        font.family: card.theme.font
        font.pixelSize: 12
        elide: Text.ElideRight
    }
    component Control: Button {
        id: button
        implicitHeight: 28
        implicitWidth: Math.max(28, contentItem.implicitWidth + 12)
        padding: 5
        contentItem: Label {
            text: button.text
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            opacity: button.enabled ? 1 : .35
        }
        background: Rectangle {
            color: button.hovered || button.activeFocus ? Qt.alpha(card.theme.accent, .16) : "transparent"
            border.width: 1
            border.color: button.activeFocus ? card.theme.accent : Qt.alpha(card.theme.foreground, .22)
        }
    }
    ColumnLayout {
        id: layout
        anchors.left: parent.left
        anchors.right: parent.right
        spacing: 10
        RowLayout {
            Layout.fillWidth: true
            spacing: 6
            Label { text: card.state ? card.state.site.id : "—"; font.bold: true; font.pixelSize: 14 }
            Label { Layout.fillWidth: true; text: connection.site ? connection.site.name : ""; opacity: .65 }
            Rectangle { width: 5; height: 5; radius: 3; color: card.statusColor }
            Label { text: card.statusText; color: card.statusColor; font.pixelSize: 11 }
        }
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 280
            color: card.theme.background
            border.color: Qt.alpha(card.theme.foreground, .17)
            clip: true
            RadarMap {
                id: map
                anchors.fill: parent
                scan: card.scan
                texture: connection.texture
                azimuthLut: connection.azimuthLut
                siteId: card.state ? card.state.site.id : ""
                sites: connection.sites
                tileRoot: "file://" + connection.runtime
                theme: card.theme
                treatment: card.session.treatment
                weakFloor: card.session.weakFloor
                labelSize: 10
                // Cached frames under a feed the engine cannot reach or
                // that has gone quiet sit back the same way; the basemap keeps its strength.
                radarOpacity: card.condition === "unavailable" || card.condition === "offline" ? .6 : 1
                onTilesNeeded: (z, x0, y0, x1, y1) => connection.send({type: "tiles_needed", z: z, x0: x0, y0: y0, x1: x1, y1: y1})
                function applyView() {
                    if (!card.session.hasView) return;
                    holdSpan = true;
                    lookAt(card.session.centerLat, card.session.centerLon);
                    span = card.session.span;
                    Qt.callLater(() => { holdSpan = false; });
                }
                Component.onCompleted: applyView()
            }
            Connections { target: connection; function onTileReady(tile) { map.tileReady(tile); } }
            Connections {
                target: card.session
                function onViewChanged() { map.applyView(); }
            }
            Label { anchors.top: parent.top; anchors.right: parent.right; anchors.margins: 8; text: "⤢"; font.pixelSize: 20; opacity: .65 }
            RowLayout {
                anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; anchors.margins: 8
                Rectangle {
                    implicitWidth: product.implicitWidth + 10; implicitHeight: 20
                    color: Qt.alpha(card.theme.background, .92)
                    Label { id: product; anchors.centerIn: parent; font.pixelSize: 10; opacity: .8
                        text: card.scan ? card.scan.productName.toUpperCase() + " " + card.scan.elevationDeg.toFixed(1) + "°" : "" }
                }
                Item { Layout.fillWidth: true }
                Rectangle {
                    implicitWidth: time.implicitWidth + 10; implicitHeight: 20
                    color: Qt.alpha(card.theme.background, .92)
                    Label { id: time; anchors.centerIn: parent; font.pixelSize: 10; opacity: .8
                        color: card.cached ? card.statusColor : card.theme.foreground
                        text: !card.scan || !card.scan.scanTime ? "" : (card.cached ? "CACHED " : "")
                            + Qt.formatDateTime(new Date(card.scan.scanTime), card.condition === "archived" ? "yyyy-MM-dd HH:mm t" : "HH:mm t") }
                }
            }
            MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: card.expandRequested() }
            Label {
                anchors.centerIn: parent; width: parent.width - 24; wrapMode: Text.Wrap
                horizontalAlignment: Text.AlignHCenter
                visible: card.notice !== ""
                text: card.notice
            }
            Rectangle {
                anchors.fill: parent
                visible: card.session.needsLocation
                color: Qt.alpha(card.theme.background, .82)
                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: { card.session.requestLocationPicker(); card.expandRequested(); }
                }
                ColumnLayout {
                    anchors.centerIn: parent
                    width: parent.width - 32
                    spacing: 10
                    Label { Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.Wrap; opacity: .7; font.pixelSize: 11
                        text: "No location yet. Search a town of 5,000+ people, or enter latitude and longitude in the window." }
                    Control { Layout.alignment: Qt.AlignHCenter; text: "CHOOSE A LOCATION"; onClicked: { card.session.requestLocationPicker(); card.expandRequested(); } }
                }
            }
        }
        // The engine's answer to this card's last command stands first
        // (docs/protocol.md, error); otherwise the feed condition's
        // explanation while frames are not arriving.
        Label {
            Layout.fillWidth: true
            visible: text !== ""
            text: connection.rejection || card.detail
            color: connection.rejection ? card.theme.accent : card.statusColor
            opacity: connection.rejection ? 1 : .85
            font.pixelSize: 10
            wrapMode: Text.Wrap
            elide: Text.ElideNone
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: 6
            Control { text: "‹"; Accessible.name: "Previous frame"; enabled: card.frames.length > 1; onClicked: card.step(-1) }
            Control { text: card.state && card.state.playing ? "Ⅱ" : "▷"; Accessible.name: "Play or pause"; enabled: card.frames.filter(f => f.status === "complete").length > 1; onClicked: card.play() }
            Control { text: "›"; Accessible.name: "Next frame"; enabled: card.frames.length > 1; onClicked: card.step(1) }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 3
                Item {
                    Layout.fillWidth: true
                    implicitHeight: 16
                    Repeater {
                        model: card.slots
                        Rectangle {
                            required property var modelData
                            required property int index
                            readonly property bool current: card.scan && modelData.id === card.scan.id
                            x: card.slots.length < 2 ? parent.width - width : index * (parent.width - width) / (card.slots.length - 1)
                            anchors.bottom: parent.bottom
                            width: current ? 3 : 2
                            height: current ? 16 : modelData.stub ? 3 : 10
                            color: modelData.partial ? "transparent" : current ? card.theme.accent : Qt.alpha(card.theme.foreground, .35)
                            border.width: modelData.partial ? 1 : 0
                            border.color: card.theme.accent
                        }
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    Label { font.pixelSize: 10; opacity: .55; text: card.frames.length ? Qt.formatTime(new Date(card.frames[0].scanTime), "HH:mm") : "" }
                    Item { Layout.fillWidth: true }
                    Label { font.pixelSize: 10; opacity: .55; text: card.condition === "ok" ? "now" : card.frames.length ? Qt.formatTime(new Date(card.frames[card.frames.length - 1].scanTime), "HH:mm") : "" }
                }
            }
        }
        Label {
            Layout.fillWidth: true; font.pixelSize: 9; opacity: .55
            visible: card.legend !== ""
            text: card.legend
            wrapMode: Text.Wrap; elide: Text.ElideNone
        }
        RowLayout {
            Layout.fillWidth: true
            Label {
                Layout.fillWidth: true; font.pixelSize: 8; opacity: .5; elide: Text.ElideRight
                text: map.osmOnScreen ? "NOAA · © OpenStreetMap" : "NOAA · Natural Earth"
            }
            Control { text: "EXPAND"; onClicked: card.expandRequested() }
        }
    }
}
