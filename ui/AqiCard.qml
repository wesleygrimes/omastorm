import QtQuick
import "Aqi.js" as Aqi

// The air quality reading over the map: the chosen scale's index, the raw
// concentrations, and the source with its observation time. Opens on the
// chip (the point readout) or a dot click (one station's breakdown); a
// click closes it. Colors are the AQI band palette, numbers carry the
// meaning, and the scale is always named, since the scales are not
// comparable (issue #3).
Rectangle {
    id: card
    required property var session
    required property var theme
    // The point reading (the `aqi` reply) behind this card; a station's
    // breakdown (`station`) replaces it when one was picked.
    property var station: session.stationDetail
    property var reading: session.aqi
    property string scale: session.aqiScale
    readonly property var shown: station ? station.computed : reading
    readonly property string title: station ? (station.station.name || "STATION").toUpperCase() : "AIR QUALITY"
    readonly property var index: Aqi.indexFor(shown, scale)
    readonly property color bandColor: Aqi.band(index) || Qt.alpha(theme.foreground, .5)
    readonly property string indexText: index != null ? "AQI " + index + " (" + Aqi.NAMES[scale] + ")"
        : scale === "raw" ? "RAW µg/m³" : "NO " + Aqi.NAMES[scale] + " INDEX"
    readonly property string whenText: {
        var secs = station ? (station.station.observed_at || 0) : (shown ? shown.observed_at : 0);
        if (!secs) return "";
        var minutes = Math.floor(Math.max(0, Date.now() / 1000 - secs) / 60);
        return (minutes < 1 ? "just now" : minutes < 60 ? minutes + " min ago"
            : minutes < 1440 ? Math.floor(minutes / 60) + "h ago"
            : Math.floor(minutes / 1440) + "d ago") + " · " + (shown ? shown.source.toUpperCase() : "");
    }
    // WAQI stations report the China scale; the card says so wherever a
    // station's index is what the number is.
    readonly property string note: station ? "STATION · WAQI (CHINA SCALE)" : ""
    width: 232
    height: column.implicitHeight + 20
    color: Qt.alpha(theme.background, .94)
    border.width: 1
    border.color: Qt.alpha(theme.foreground, .22)
    radius: 3
    MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onClicked: card.closeCard()
    }
    function closeCard() {
        session.stationDetail = null;
        session.aqiCardOpen = false;
    }
    Column {
        id: column
        anchors.fill: parent
        anchors.margins: 10
        spacing: 5
        Text {
            width: parent.width
            text: card.title
            color: card.theme.foreground
            font.family: card.theme.font
            font.pixelSize: 11
            font.bold: true
            elide: Text.ElideRight
        }
        Row {
            spacing: 7
            Rectangle {
                width: 9; height: 9; radius: 4.5
                anchors.verticalCenter: parent.verticalCenter
                color: card.bandColor
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: card.indexText
                color: card.theme.foreground
                font.family: card.theme.font
                font.pixelSize: 14
                font.bold: true
            }
        }
        Text {
            width: parent.width
            text: card.shown ? (Aqi.pollutants(card.shown) || "no pollutant breakdown") : ""
            color: card.theme.foreground
            opacity: .8
            font.family: card.theme.font
            font.pixelSize: 11
            wrapMode: Text.Wrap
        }
        Text {
            width: parent.width
            text: card.note + (card.whenText ? " · observed " + card.whenText : "")
            color: card.theme.foreground
            opacity: .55
            font.family: card.theme.font
            font.pixelSize: 10
            elide: Text.ElideRight
        }
    }
}
