import QtQuick
import QtQuick.Layouts
import "Location.js" as Location
import "Sites.js" as Sites

// `/` search (DESIGN.md): one field for a city, a radar (site or mosaic
// source), or pasted coordinates. Enter on a place centres and unlocks;
// Enter on a site or mosaic locks and centres. The station title opens the
// same card on covering mosaics and the nearest dishes.
Item {
    id: picker
    property var theme
    property var engine
    property var session: null
    property var sites: []
    property var sources: []
    property string selectedSourceId: ""
    property bool onboarding: false
    property bool browseSites: false
    property bool manual: true
    signal manualStarted()
    property real centerLat: 0
    property real centerLon: 0
    property bool compact: false
    property real cardTop: 20
    property bool open: false
    property bool closeOnScrim: true
    property alias query: field.text
    readonly property bool fieldFocused: field.activeFocus
    property int selected: 0
    property var placeResults: []
    property string pendingQuery: ""
    property string settledQuery: ""
    readonly property int limit: 4
    readonly property var coordEntry: Location.parseCoordQuery(settledQuery)
    readonly property string coordError: coordEntry && coordEntry.error ? coordEntry.error : ""
    readonly property bool metric: Qt.locale().measurementSystem === Locale.MetricSystem
    component Word: Text {
        color: picker.theme.foreground
        font.family: picker.theme.font
        font.pixelSize: 12
        elide: Text.ElideRight
        verticalAlignment: Text.AlignVCenter
    }
    signal chosen(var row)
    readonly property var siteRanked: {
        if (!open || !sites || !sites.length) return { rows: [], total: 0 };
        return Sites.rank(sites, settledQuery, centerLat, centerLon, picker.limit, picker.metric);
    }
    readonly property var siteRows: {
        var out = [];
        for (var r of siteRanked.rows) {
            out.push({
                kind: "site", site: r.site, name: r.site.id, where: r.where,
                label: r.site.id, lat: r.site.lat, lon: r.site.lon,
                place: r.place, idHits: r.idHits, placeHits: r.placeHits
            });
        }
        return out;
    }
    readonly property var placeRows: {
        var out = [];
        for (var p of placeResults) {
            var bits = [];
            if (p.region) bits.push(p.region);
            if (p.country && p.country !== "-99") bits.push(p.country);
            var where = bits.join(", ");
            if (!where) {
                var km = Location.validPair(centerLat, centerLon) ? Location.distanceKm(centerLat, centerLon, p.lat, p.lon) : 0;
                where = Location.validPair(centerLat, centerLon) ? Sites.where(km, Sites.bearingDeg(centerLat, centerLon, p.lat, p.lon), picker.metric) : (p.class || "").toUpperCase();
            }
            out.push({
                kind: "place", name: p.name, lat: p.lat, lon: p.lon,
                where: where, label: p.region ? p.name + ", " + p.region : p.name
            });
        }
        return out;
    }
    readonly property var mosaicRows: {
        if (!open) return [];
        return Location.rankMosaics(sources, settledQuery, centerLat, centerLon,
            browseSites, selectedSourceId, picker.limit, picker.metric);
    }
    readonly property var rows: {
        if (!open || (onboarding && !manual)) return [];
        return Location.mergeSearch(siteRows, placeRows, coordEntry, settledQuery, browseSites, picker.limit, mosaicRows);
    }
    visible: open
    function show(text, offerLocation) {
        onboarding = offerLocation === true;
        browseSites = offerLocation === "sites";
        manual = !onboarding;
        if (manual) manualStarted();
        field.text = text || "";
        selected = 0;
        placeResults = [];
        pendingQuery = "";
        open = true;
        Qt.callLater(() => { if (manual) { field.cursorPosition = field.length; field.forceActiveFocus(); } });
        applyQuery();
    }
    function close() {
        settle.stop();
        open = false;
        field.text = "";
        selected = 0;
        placeResults = [];
        pendingQuery = "";
        settledQuery = "";
        field.focus = false;
    }
    function accept() {
        applyQuery();
        if (!open || coordError || !rows.length) return;
        var row = rows[Math.min(selected, rows.length - 1)];
        close();
        chosen(row);
    }
    function move(delta) { selected = Math.max(0, Math.min(rows.length - 1, selected + delta)); }
    function go(lat, lon, name) {
        close();
        chosen({ kind: "place", lat: lat, lon: lon, name: name || "", label: name || "" });
    }
    function applyQuery() {
        settle.stop();
        settledQuery = query;
        if (!open || !engine || (onboarding && !manual)) return;
        var q = settledQuery.trim();
        var coord = Location.parseCoordQuery(q);
        if (!q || (coord && coord.lat !== undefined) || (coord && coord.error)) {
            placeResults = [];
            pendingQuery = "";
            return;
        }
        pendingQuery = q;
        var cmd = {type: "search_places", query: q};
        if (Location.validPair(centerLat, centerLon)) {
            cmd.lat = centerLat;
            cmd.lon = centerLon;
        }
        engine.send(cmd);
    }
    onQueryChanged: {
        selected = 0;
        if (!String(query).trim()) applyQuery();
        else settle.restart();
    }
    Timer {
        id: settle
        interval: 100
        onTriggered: picker.applyQuery()
    }
    Connections {
        target: picker.engine
        function onPlacesReady(message) {
            if (!picker.open || message.query !== picker.pendingQuery) return;
            picker.placeResults = message.results || [];
        }
    }
    Rectangle {
        anchors.fill: parent
        color: Qt.alpha(picker.theme.background, .5)
        MouseArea {
            anchors.fill: parent
            onClicked: if (picker.closeOnScrim) picker.close()
            onWheel: wheel => { wheel.accepted = true }
        }
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
        MouseArea { anchors.fill: parent; onWheel: wheel => { wheel.accepted = true } }
        ColumnLayout {
            id: column
            anchors.fill: parent
            anchors.margins: 14
            spacing: 10
            Loader {
                Layout.fillWidth: true
                active: picker.onboarding && !picker.manual
                visible: active
                sourceComponent: LocationPrompt {
                    session: picker.session
                    theme: picker.theme
                    onManualChosen: {
                        picker.manual = true;
                        picker.manualStarted();
                        field.forceActiveFocus();
                    }
                }
            }
            ColumnLayout {
                Layout.fillWidth: true
                visible: picker.manual
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
                        Word { text: "city · radar · coordinates"; font.pixelSize: 10; opacity: .45; visible: !picker.compact }
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
                            readonly property bool isSite: !!(modelData && modelData.kind === "site")
                            readonly property bool isMosaic: !!(modelData && modelData.kind === "mosaic")
                            readonly property bool isRadar: isSite || isMosaic
                            readonly property color ink: current ? picker.theme.accent : picker.theme.foreground
                            Layout.fillWidth: true
                            implicitHeight: 28
                            color: current ? Qt.alpha(picker.theme.foreground, .08) : "transparent"
                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 12
                                anchors.rightMargin: 12
                                spacing: 12
                                Word {
                                    visible: row.isRadar
                                    text: row.isRadar ? Sites.mark(row.modelData.name, row.modelData.idHits || [], picker.theme.accent) : ""
                                    textFormat: Text.StyledText
                                    font.bold: true
                                    color: row.ink
                                    Layout.preferredWidth: 52
                                }
                                Word {
                                    text: row.isRadar
                                        ? Sites.mark(row.modelData.place || "", row.modelData.placeHits || [], picker.theme.accent)
                                        : (row.modelData.label || row.modelData.name || "")
                                    textFormat: row.isRadar ? Text.StyledText : Text.PlainText
                                    font.bold: !row.isRadar
                                    color: row.ink
                                    opacity: row.isRadar ? .9 : 1
                                    Layout.fillWidth: true
                                }
                                Word {
                                    text: (row.modelData && row.modelData.where) || ""
                                    font.pixelSize: 10
                                    color: row.ink
                                    opacity: .6
                                }
                                Word {
                                    text: row.isSite ? "site" : row.isMosaic ? "source" : "place"
                                    font.pixelSize: 10
                                    color: row.ink
                                    opacity: .45
                                    font.letterSpacing: 1
                                }
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
                        Layout.fillWidth: true
                        Layout.leftMargin: 12
                        Layout.preferredHeight: 28
                        visible: !picker.rows.length
                        text: picker.coordError ? picker.coordError.toUpperCase()
                            : picker.settledQuery.trim() ? "NO MATCHES" : "CITY, RADAR, OR LAT, LON"
                        color: picker.coordError ? picker.theme.accent : picker.theme.foreground
                        opacity: picker.coordError ? 1 : .55
                        font.letterSpacing: 1
                    }
                }
                Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(picker.theme.foreground, .17) }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 14
                    Word { text: "↑ ↓ move"; font.pixelSize: 10; opacity: .55 }
                    Word { text: "↵ go"; font.pixelSize: 10; opacity: .55 }
                    Word { text: "esc close"; font.pixelSize: 10; opacity: .55; visible: !picker.compact }
                    Item { Layout.fillWidth: true }
                    Word { text: picker.rows.length ? picker.rows.length + " shown" : ""; font.pixelSize: 10; opacity: .55 }
                }
            }
        }
    }
}
