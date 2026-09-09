import QtQuick
import QtQuick.Layouts
import "Location.js" as Location
import "Sites.js" as Sites

// The location picker (DESIGN.md, location): engine search_places plus
// labeled lat/lon; Enter writes state, never config. The keys are handled
// here while it is open; the window's own shortcuts stand down.
Item {
    id: picker
    property var theme
    property var engine
    property real centerLat: 0
    property real centerLon: 0
    property bool compact: false
    property real cardTop: 20
    property bool open: false
    property bool closeOnScrim: true
    // Whether the map centre lies outside the gazetteer's envelope
    // (Location.inGazetteer): search finds no towns there, and the empty
    // list says so instead of looking like a miss.
    property bool outsideGazetteer: false
    property alias query: field.text
    property alias latText: latField.text
    property alias lonText: lonField.text
    readonly property bool fieldFocused: field.activeFocus || latField.activeFocus || lonField.activeFocus
    property int selected: 0
    property var results: []
    property string pendingQuery: ""
    // Every result the engine sends is shown; it ranks and cuts to eight
    // (docs/protocol.md, search_places), so nothing is asked for and dropped.
    readonly property int limit: 8
    readonly property var coordEntry: Location.parseCoordFields(latText, lonText)
    readonly property string coordError: coordEntry.error || ""
    component Word: Text {
        color: picker.theme.foreground
        font.family: picker.theme.font
        font.pixelSize: 12
        elide: Text.ElideRight
        verticalAlignment: Text.AlignVCenter
    }
    signal chosen(real lat, real lon, string name)
    readonly property var rows: {
        var out = [];
        for (var p of results) {
            var bits = [];
            if (p.region) bits.push(p.region);
            if (p.country && p.country !== "-99") bits.push(p.country);
            var where = bits.join(", ");
            if (!where) {
                var km = Location.validPair(centerLat, centerLon) ? Location.distanceKm(centerLat, centerLon, p.lat, p.lon) : 0;
                where = Location.validPair(centerLat, centerLon) ? Sites.where(km, Sites.bearingDeg(centerLat, centerLon, p.lat, p.lon)) : (p.class || "").toUpperCase();
            }
            out.push({ name: p.name, lat: p.lat, lon: p.lon, kind: p.class || "place",
                       where: where, label: p.region ? p.name + ", " + p.region : p.name });
            if (out.length >= limit) break;
        }
        return out;
    }
    visible: open
    function show(text) {
        field.text = text || "";
        latField.text = "";
        lonField.text = "";
        selected = 0;
        results = [];
        open = true;
        Qt.callLater(() => { field.cursorPosition = field.length; field.forceActiveFocus(); });
        search.restart();
    }
    function close() {
        open = false;
        field.text = "";
        latField.text = "";
        lonField.text = "";
        selected = 0;
        results = [];
        field.focus = false;
        latField.focus = false;
        lonField.focus = false;
    }
    function accept() {
        if (!open) return;
        if (coordEntry.lat !== undefined) { var lat = coordEntry.lat, lon = coordEntry.lon; close(); chosen(lat, lon, ""); return; }
        if (coordError || !rows.length) return;
        var row = rows[Math.min(selected, rows.length - 1)];
        close();
        chosen(row.lat, row.lon, row.label);
    }
    function move(delta) { selected = Math.max(0, Math.min(rows.length - 1, selected + delta)); }
    function tab(back) {
        var order = [field, latField, lonField], i = 0;
        for (; i < order.length; i++) if (order[i].activeFocus) break;
        if (i >= order.length) i = 0;
        order[back ? (i + order.length - 1) % order.length : (i + 1) % order.length].forceActiveFocus();
    }
    function go(lat, lon, name) { close(); chosen(lat, lon, name || ""); }
    onQueryChanged: { selected = 0; search.restart(); }
    Timer {
        id: search
        interval: 120
        onTriggered: {
            if (!picker.open || !picker.engine) return;
            var q = picker.query.trim();
            if (!q) { picker.results = []; return; }
            picker.pendingQuery = q;
            var cmd = {type: "search_places", query: q};
            if (Location.validPair(picker.centerLat, picker.centerLon)) {
                cmd.lat = picker.centerLat;
                cmd.lon = picker.centerLon;
            }
            picker.engine.send(cmd);
        }
    }
    Connections {
        target: picker.engine
        function onPlacesReady(message) {
            if (!picker.open || message.query !== picker.pendingQuery) return;
            picker.results = message.results || [];
        }
    }
    Rectangle {
        anchors.fill: parent
        color: Qt.alpha(picker.theme.background, .5)
        MouseArea { anchors.fill: parent; onClicked: if (picker.closeOnScrim) picker.close() }
    }
    Rectangle {
        id: card
        width: Math.min(520, picker.width - 40)
        x: Math.round((picker.width - width) / 2)
        y: Math.round(Math.max(20, Math.min(picker.cardTop, picker.height - height - 20)))
        height: column.implicitHeight + 28
        color: Qt.alpha(picker.theme.background, .95)
        border.width: 1
        border.color: picker.theme.foreground
        MouseArea { anchors.fill: parent }
        ColumnLayout {
            id: column
            anchors.fill: parent
            anchors.margins: 14
            spacing: 10
            Rectangle {
                Layout.fillWidth: true
                implicitHeight: 36
                color: Qt.alpha(picker.theme.foreground, .04)
                border.width: 1
                border.color: Qt.alpha(picker.theme.foreground, .4)
                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 10
                    anchors.rightMargin: 10
                    spacing: 10
                    Canvas {
                        implicitWidth: 16; implicitHeight: 16
                        property color ink: picker.theme.foreground
                        onInkChanged: requestPaint()
                        onPaint: {
                            var ctx = getContext("2d");
                            ctx.reset(); ctx.clearRect(0, 0, width, height);
                            ctx.strokeStyle = ink; ctx.lineWidth = 1.5;
                            ctx.beginPath(); ctx.arc(6.5, 6.5, 4.5, 0, 2 * Math.PI); ctx.stroke();
                            ctx.beginPath(); ctx.moveTo(10, 10); ctx.lineTo(14, 14); ctx.stroke();
                        }
                    }
                    TextInput {
                        id: field
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        color: picker.theme.foreground
                        font.family: picker.theme.font
                        font.pixelSize: 13
                        verticalAlignment: TextInput.AlignVCenter
                        clip: true
                        leftPadding: 0; rightPadding: 0
                        cursorVisible: false
                        cursorDelegate: Item {}
                        Keys.onPressed: event => {
                            if (event.key === Qt.Key_Escape) { picker.close(); event.accepted = true; }
                            else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) { picker.accept(); event.accepted = true; }
                            else if (event.key === Qt.Key_Up) { picker.move(-1); event.accepted = true; }
                            else if (event.key === Qt.Key_Down) { picker.move(1); event.accepted = true; }
                            else if (event.key === Qt.Key_Tab || event.key === Qt.Key_Backtab) {
                                picker.tab(event.key === Qt.Key_Backtab || !!(event.modifiers & Qt.ShiftModifier));
                                event.accepted = true;
                            }
                            else if (event.key === Qt.Key_U && event.modifiers === Qt.ControlModifier) { field.text = ""; event.accepted = true; }
                        }
                        Rectangle {
                            width: 7; height: 15
                            x: field.cursorRectangle.x
                            y: Math.round(field.cursorRectangle.y + (field.cursorRectangle.height - height) / 2)
                            color: picker.theme.foreground
                            opacity: .9
                        }
                    }
                    Word { text: "place"; font.pixelSize: 10; opacity: .45; visible: !picker.compact }
                }
            }
            RowLayout {
                Layout.fillWidth: true
                spacing: 10
                Word { text: "LAT"; font.pixelSize: 10; opacity: .55; Layout.preferredWidth: 28 }
                Rectangle {
                    Layout.fillWidth: true
                    implicitHeight: 28
                    color: Qt.alpha(picker.theme.foreground, .04)
                    border.width: 1
                    border.color: Qt.alpha(picker.theme.foreground, .4)
                    TextInput {
                        id: latField
                        anchors.fill: parent
                        anchors.leftMargin: 8
                        anchors.rightMargin: 8
                        color: picker.theme.foreground
                        font.family: picker.theme.font
                        font.pixelSize: 12
                        verticalAlignment: TextInput.AlignVCenter
                        clip: true
                        Keys.onPressed: event => {
                            if (event.key === Qt.Key_Escape) { picker.close(); event.accepted = true; }
                            else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) { picker.accept(); event.accepted = true; }
                            else if (event.key === Qt.Key_Tab || event.key === Qt.Key_Backtab) {
                                picker.tab(event.key === Qt.Key_Backtab || !!(event.modifiers & Qt.ShiftModifier));
                                event.accepted = true;
                            }
                        }
                    }
                }
                Word { text: "LON"; font.pixelSize: 10; opacity: .55; Layout.preferredWidth: 28 }
                Rectangle {
                    Layout.fillWidth: true
                    implicitHeight: 28
                    color: Qt.alpha(picker.theme.foreground, .04)
                    border.width: 1
                    border.color: Qt.alpha(picker.theme.foreground, .4)
                    TextInput {
                        id: lonField
                        anchors.fill: parent
                        anchors.leftMargin: 8
                        anchors.rightMargin: 8
                        color: picker.theme.foreground
                        font.family: picker.theme.font
                        font.pixelSize: 12
                        verticalAlignment: TextInput.AlignVCenter
                        clip: true
                        Keys.onPressed: event => {
                            if (event.key === Qt.Key_Escape) { picker.close(); event.accepted = true; }
                            else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) { picker.accept(); event.accepted = true; }
                            else if (event.key === Qt.Key_Tab || event.key === Qt.Key_Backtab) {
                                picker.tab(event.key === Qt.Key_Backtab || !!(event.modifiers & Qt.ShiftModifier));
                                event.accepted = true;
                            }
                        }
                    }
                }
            }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 0
                Repeater {
                    model: picker.rows
                    Rectangle {
                        id: row
                        required property var modelData
                        required property int index
                        readonly property bool current: index === picker.selected
                        readonly property color ink: current ? picker.theme.accent : picker.theme.foreground
                        Layout.fillWidth: true
                        implicitHeight: 28
                        color: current ? Qt.alpha(picker.theme.foreground, .08) : "transparent"
                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            spacing: 12
                            Word { text: row.modelData.name; font.bold: true; color: row.ink; Layout.fillWidth: true }
                            Word { text: row.modelData.where; font.pixelSize: 10; color: row.ink; opacity: .6 }
                        }
                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            onPositionChanged: picker.selected = row.index
                            onClicked: { picker.selected = row.index; picker.accept(); }
                        }
                    }
                }
                Word {
                    visible: !!picker.coordError
                    text: picker.coordError.toUpperCase()
                    color: picker.theme.accent
                    Layout.fillWidth: true; Layout.leftMargin: 12; Layout.preferredHeight: 28; font.letterSpacing: 1
                }
                Word {
                    visible: !picker.coordError && picker.coordEntry.lat !== undefined
                    text: picker.coordEntry.lat === undefined ? "" : "↵ GO TO " + picker.coordEntry.lat.toFixed(4) + ", " + picker.coordEntry.lon.toFixed(4)
                    Layout.fillWidth: true; Layout.leftMargin: 12; Layout.preferredHeight: 28; opacity: .55; font.letterSpacing: 1
                }
                Word {
                    visible: !picker.coordError && picker.coordEntry.lat === undefined && !picker.rows.length
                    text: picker.outsideGazetteer ? "SEARCH FINDS NO TOWNS HERE · ENTER LATITUDE AND LONGITUDE"
                        : picker.query.trim() ? "NO PLACE MATCHES · TRY COORDINATES" : "TOWNS OF 5,000+ PEOPLE, OR ENTER LATITUDE AND LONGITUDE"
                    Layout.fillWidth: true; Layout.leftMargin: 12; Layout.preferredHeight: 28; opacity: .55; font.letterSpacing: 1
                }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(picker.theme.foreground, .17) }
            RowLayout {
                Layout.fillWidth: true
                spacing: 14
                Word { text: "tab fields"; font.pixelSize: 10; opacity: .55; visible: !picker.compact }
                Word { text: "↑ ↓ move"; font.pixelSize: 10; opacity: .55 }
                Word { text: "↵ set location"; font.pixelSize: 10; opacity: .55 }
                Word { text: "esc close"; font.pixelSize: 10; opacity: .55; visible: !picker.compact }
                Item { Layout.fillWidth: true }
                Word { text: picker.rows.length ? picker.rows.length + " shown" : ""; font.pixelSize: 10; opacity: .55 }
            }
        }
    }
}
