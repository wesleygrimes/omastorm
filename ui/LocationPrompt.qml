import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

ColumnLayout {
    id: prompt
    required property var session
    required property var theme
    // The popover's popover_scale; the window leaves it at 1.
    property real uiScale: 1
    function px(length) { return Math.round(length * uiScale); }
    signal manualChosen()
    spacing: px(10)
    component Word: Text {
        Layout.fillWidth: true
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        color: prompt.theme.foreground
        font.family: prompt.theme.font
        font.pixelSize: prompt.px(12)
    }
    component Action: Button {
        Layout.alignment: Qt.AlignHCenter
        implicitHeight: prompt.px(30)
        implicitWidth: label.implicitWidth + prompt.px(20)
        contentItem: Text {
            textFormat: Text.PlainText
            id: label
            text: parent.text
            color: prompt.theme.foreground
            font.family: prompt.theme.font
            font.pixelSize: prompt.px(12)
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            opacity: parent.enabled ? 1 : .5
        }
        background: Rectangle {
            color: parent.hovered ? Qt.alpha(prompt.theme.accent, .16) : "transparent"
            border.width: 1
            border.color: parent.activeFocus ? prompt.theme.accent : Qt.alpha(prompt.theme.foreground, .4)
        }
    }
    Word { text: "Choose your location"; font.bold: true; font.pixelSize: prompt.px(13) }
    Action {
        objectName: "approximateLocation"
        text: prompt.session.locating ? "Finding your location…" : "Use approximate location"
        visible: !!prompt.session.engine.state && prompt.session.engine.state.mode === "live"
        enabled: !prompt.session.locationPending
        onClicked: prompt.session.requestIpLocation()
    }
    Word {
        visible: !prompt.session.ipLocationDismissed && !!prompt.session.locationError
        text: prompt.session.locationError
        font.pixelSize: prompt.px(11)
    }
    Action { objectName: "manualLocation"; text: "Choose manually"; onClicked: prompt.manualChosen() }
}
