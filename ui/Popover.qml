import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import "Keys.js" as KeyMap
import "Metar.js" as Metar

FocusScope {
    id: card
    required property var session
    property var theme: session.theme.snapshot
    property alias engine: connection
    readonly property var state: connection.state
    readonly property var scan: state ? state.frame : null
    readonly property var frames: state ? state.timeline : []
    // One tick per frame, no gap stubs, pixel-snapped: the same strip the
    // window draws, denser.
    readonly property var slots: {
        var result = [];
        for (var j = 0; j < frames.length; j++)
            result.push({id: frames[j].id, partial: frames[j].status === "partial"});
        return result;
    }
    readonly property int currentSlot: scan ? slots.findIndex(s => s.id === scan.id) : -1
    readonly property string condition: state ? state.mode === "archived" ? "archived" : state.connection.status : "offline"
    readonly property color statusColor: condition === "stale" ? theme.yellow
        : condition === "offline" || condition === "unavailable" ? theme.red : theme.accent
    readonly property string statusText: {
        if (!state) return "OFFLINE";
        if (condition === "archived") return "ARCHIVED";
        var label = condition === "ok" ? "LIVE" : condition.toUpperCase();
        var complete = frames.filter(f => f.status === "complete");
        if (!scan || !scan.scanTime || !complete.length) return label;
        var age = Math.max(0, state.connection.ageSeconds +
            Math.round((Date.parse(complete[complete.length - 1].scanTime) - Date.parse(scan.scanTime)) / 1000));
        var minutes = Math.floor(age / 60);
        return label + " · " + (minutes < 1 ? "just now" : minutes < 60 ? minutes + " min ago"
            : minutes < 1440 ? Math.floor(minutes / 60) + "h ago" : Math.floor(minutes / 1440) + "d ago");
    }
    signal expandRequested()
    signal closeRequested()
    // config.toml popover_scale. Every length below goes through px(), not
    // a scale transform, so text, controls and the map draw at full
    // resolution. Hairline borders stay 1 px.
    readonly property real uiScale: session.popoverScale
    function px(length) { return KeyMap.scaled(length, uiScale); }
    implicitWidth: px(KeyMap.POPOVER_WIDTH)
    // Match RadarBar's fixed KeyboardPanel contentHeight (card + 28 inset).
    // Do not track layout.implicitHeight — time labels and status text
    // settling after open made the panel shrink and grow.
    implicitHeight: px(KeyMap.POPOVER_HEIGHT)
    Engine { id: connection }
    function step(delta) { if (state) connection.send({type: "step", delta: delta}); }
    function play() { if (state) connection.send({type: state.playing ? "pause" : "play"}); }
    // Harnesses read this surface's METAR state.
    readonly property alias metarState: metar
    // Harnesses read the laid-out geometry: the map's height and the bottom
    // of the last row, which must stay inside implicitHeight at every scale.
    readonly property real mapHeight: mapFrame.height
    readonly property real contentBottom: attribution.y + attribution.height
    Metars {
        id: metar
        engine: connection
        session: card.session
        view: map
    }
    // Respect the same config keys as the window; Enter always expands.
    Shortcut { id: probe; enabled: false }
    function canon(sequence) { probe.sequence = sequence; return probe.portableText; }
    property var bindings: ({})
    function applyKeys() { bindings = KeyMap.resolve(session.config.keys, canon).bindings; }
    Component.onCompleted: applyKeys()
    Connections { target: card.session.config; function onKeysChanged() { card.applyKeys(); } }
    Instantiator {
        model: ["previous_frame", "next_frame", "play", "aviation", "close"]
        delegate: Shortcut {
            required property string modelData
            sequences: card.bindings[modelData] || []
            enabled: card.visible
            onActivated: {
                if (modelData === "close") {
                    if (metar.selected) metar.selected = null;
                    else card.closeRequested();
                } else if (modelData === "play") card.play();
                else if (modelData === "aviation") metar.toggle();
                else card.step(modelData === "previous_frame" ? -1 : 1);
            }
        }
    }
    Keys.onReturnPressed: expandRequested()
    Keys.onEnterPressed: expandRequested()
    component Label: Text {
        textFormat: Text.PlainText
        color: card.theme.foreground
        font.family: card.theme.font
        font.pixelSize: card.px(12)
        elide: Text.ElideRight
    }
    component Control: Button {
        id: button
        implicitHeight: card.px(28)
        implicitWidth: Math.max(card.px(28), contentItem.implicitWidth + card.px(12))
        padding: card.px(5)
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
        anchors.top: parent.top
        height: card.implicitHeight
        spacing: card.px(10)
        RowLayout {
            Layout.fillWidth: true
            spacing: card.px(6)
            Label { text: connection.selectedSiteId || (connection.source ? connection.source.id : "—"); font.bold: true; font.pixelSize: card.px(14) }
            Label { Layout.fillWidth: true; text: connection.site ? connection.site.name : (connection.source ? connection.source.name : ""); opacity: .65 }
            Rectangle { width: card.px(5); height: card.px(5); radius: card.px(3); color: card.statusColor }
            Label { text: card.statusText; color: card.statusColor; font.pixelSize: card.px(11) }
        }
        Rectangle {
            id: mapFrame
            Layout.fillWidth: true
            // px(280) unless rounding at an odd scale leaves the rows below
            // short of room; then the map gives up those pixels, never the card.
            Layout.fillHeight: true
            Layout.preferredHeight: card.px(280)
            Layout.maximumHeight: card.px(280)
            color: card.theme.background
            border.color: Qt.alpha(card.theme.foreground, .17)
            clip: true
            RadarMap {
                id: map
                anchors.fill: parent
                scan: card.scan
                texture: connection.texture
                azimuthLut: connection.azimuthLut
                siteId: connection.selectedSiteId
                sourceId: connection.source ? connection.source.id : ""
                sites: connection.sites
                coverage: connection.site && connection.site.coverage ? connection.site.coverage
                    : (connection.source && connection.source.coverage ? connection.source.coverage : null)
                tileRoot: "file://" + connection.runtime
                theme: card.theme
                metarMode: metar.shown
                metars: metar.metars
                metarMark: Metar.markFromConfig(card.session.config.values) || "pin"
                treatment: card.session.treatment
                weakFloor: card.session.weakFloor
                labelSize: card.px(10)
                chromeScale: card.uiScale
                radarOpacity: card.condition === "unavailable" ? .6 : 1
                // The expand overlay above takes every click and wheel.
                interactive: false
                onViewSettled: (lat, lon) => metar.request()
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
            Rectangle {
                visible: !!metar.selected
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                anchors.rightMargin: card.px(8)
                anchors.bottomMargin: card.px(32)
                width: Math.round(parent.width * 0.8)
                height: popoverMetarText.implicitHeight + card.px(20)
                color: Qt.alpha(card.theme.background, .95)
                border.width: 1
                border.color: Qt.alpha(card.theme.foreground, .22)
                Label {
                    id: popoverMetarText
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.margins: card.px(6)
                    wrapMode: Text.Wrap
                    font.family: "monospace"
                    font.pixelSize: card.px(10)
                    text: metar.selected ? metar.selected.raw : ""
                }
            }
            Connections {
                target: card.session
                function onViewChanged() { map.applyView(); }
            }
            Label { anchors.top: parent.top; anchors.right: parent.right; anchors.margins: card.px(8); text: "⤢"; font.pixelSize: card.px(20); opacity: .65 }
            RowLayout {
                anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; anchors.margins: card.px(8)
                Rectangle {
                    implicitWidth: product.implicitWidth + card.px(10); implicitHeight: card.px(20)
                    color: Qt.alpha(card.theme.background, .92)
                    Label { id: product; anchors.centerIn: parent; font.pixelSize: card.px(10); opacity: .8
                        text: card.scan ? card.scan.productName.toUpperCase()
                            + (card.scan.kind !== "mosaic" && card.scan.scanTime ? " " + card.scan.elevationDeg.toFixed(1) + "°" : "") : "" }
                }
                Item { Layout.fillWidth: true }
                Rectangle {
                    implicitWidth: time.implicitWidth + card.px(10); implicitHeight: card.px(20)
                    color: Qt.alpha(card.theme.background, .92)
                    Label { id: time; anchors.centerIn: parent; font.pixelSize: card.px(10); opacity: .8
                        text: {
                            if (!card.scan || !card.scan.scanTime) return "";
                            var loc = Qt.locale(), d = new Date(card.scan.scanTime);
                            var timeFmt = loc.timeFormat(Locale.ShortFormat) + " t";
                            return card.condition === "archived"
                                ? Qt.formatDateTime(d, loc.dateFormat(Locale.ShortFormat) + " " + timeFmt)
                                : Qt.formatTime(d, timeFmt);
                        }
                    }
                }
            }
            // A METAR chip opens its report; anywhere else expands. The wheel
            // stops here so the popover never zooms the remembered view.
            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: mouse => {
                    var report = map.metarAt(mouse.x, mouse.y);
                    if (report) metar.selected = metar.selected === report ? null : report;
                    else card.expandRequested();
                }
                onWheel: wheel => { wheel.accepted = true }
            }
            // An update is on disk but this shell still runs the old plugin;
            // the click restarts the shell (PluginSession.updatePending).
            Rectangle {
                anchors.top: parent.top; anchors.left: parent.left; anchors.margins: card.px(8)
                visible: card.session.updatePending
                implicitWidth: updated.implicitWidth + card.px(10); implicitHeight: card.px(20)
                color: Qt.alpha(card.theme.background, .92)
                border.color: card.theme.accent
                Label { id: updated; anchors.centerIn: parent; font.pixelSize: card.px(10); color: card.theme.accent; text: card.session.updateNotice }
                MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: card.session.restartShell() }
            }
            Rectangle {
                anchors.centerIn: parent
                visible: card.condition === "loading" && (!card.scan || !card.scan.scanTime) && !!card.state
                implicitWidth: popLoad.implicitWidth + card.px(20)
                implicitHeight: card.px(24)
                color: Qt.alpha(card.theme.background, .88)
                border.width: 1
                border.color: Qt.alpha(card.theme.foreground, .22)
                Label {
                    id: popLoad
                    anchors.centerIn: parent
                    text: "Loading..."
                    color: card.theme.accent
                    font.pixelSize: card.px(10)
                }
            }
            Rectangle {
                anchors.centerIn: parent; width: parent.width - card.px(24)
                height: errorNotice.implicitHeight + card.px(16)
                color: card.theme.background
                visible: errorNotice.text.length > 0
                Label {
                    id: errorNotice
                    anchors.centerIn: parent; width: parent.width - card.px(16); wrapMode: Text.Wrap
                    horizontalAlignment: Text.AlignHCenter
                    text: map.error || card.session.startupError || connection.error
                }
            }
            Rectangle {
                anchors.fill: parent
                visible: card.session.needsLocation
                color: Qt.alpha(card.theme.background, .82)
                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: { card.session.requestLocationPicker(); card.expandRequested(); }
                    onWheel: wheel => { wheel.accepted = true }
                }
                LocationPrompt {
                    anchors.centerIn: parent
                    width: parent.width - card.px(32)
                    session: card.session
                    theme: card.theme
                    uiScale: card.uiScale
                    onManualChosen: { card.session.requestLocationPicker(); card.expandRequested(); }
                }
            }
        }
        Label {
            Layout.fillWidth: true
            visible: !!connection.rejection
            text: connection.rejection; color: card.theme.accent; wrapMode: Text.Wrap
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: card.px(6)
            Control { text: "‹"; Accessible.name: "Previous frame"; enabled: card.frames.length > 1; onClicked: card.step(-1) }
            Control { text: card.state && card.state.playing ? "Ⅱ" : "▷"; Accessible.name: "Play or pause"; enabled: card.frames.filter(f => f.status === "complete").length > 1; onClicked: card.play() }
            Control { text: "›"; Accessible.name: "Next frame"; enabled: card.frames.length > 1; onClicked: card.step(1) }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: card.px(3)
                Item {
                    id: strip
                    Layout.fillWidth: true
                    implicitHeight: card.px(14)
                    Repeater {
                        model: card.slots
                        Rectangle {
                            required property var modelData
                            required property int index
                            readonly property bool current: index === card.currentSlot
                            x: card.slots.length > 1 ? Math.round(index * (strip.width - width) / (card.slots.length - 1)) : Math.round((strip.width - width) / 2)
                            y: Math.round((strip.height - height) / 2)
                            width: card.px(current || modelData.partial ? 3 : 2)
                            height: card.px(current || modelData.partial ? 14 : 10)
                            color: current ? card.theme.accent : modelData.partial ? "transparent"
                                : Qt.alpha(card.theme.foreground, .40)
                            border.width: modelData.partial && !current ? 1 : 0
                            border.color: card.theme.accent
                        }
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    Layout.preferredHeight: card.px(12)
                    Label { font.pixelSize: card.px(10); opacity: .55; text: card.frames.length ? Qt.formatTime(new Date(card.frames[0].scanTime), Qt.locale().timeFormat(Locale.ShortFormat)) : " " }
                    Item { Layout.fillWidth: true }
                    Label { font.pixelSize: card.px(10); opacity: .55; text: card.condition === "ok" ? "now" : card.frames.length ? Qt.formatTime(new Date(card.frames[card.frames.length - 1].scanTime), Qt.locale().timeFormat(Locale.ShortFormat)) : " " }
                }
            }
        }
        Label {
            id: attribution
            Layout.fillWidth: true
            font.pixelSize: card.px(8)
            opacity: .5
            elide: Text.ElideRight
            text: map.osmOnScreen ? (connection.source ? connection.source.attribution : "NOAA") + " · © OpenStreetMap"
                : (connection.source ? connection.source.attribution : "NOAA") + " · Natural Earth"
        }
    }
}
