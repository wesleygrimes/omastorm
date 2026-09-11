import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

ColumnLayout {
    id: prompt
    required property var session
    required property var theme
    signal manualChosen()
    spacing: 10
    component Word: Text {
        Layout.fillWidth: true
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        color: prompt.theme.foreground
        font.family: prompt.theme.font
        font.pixelSize: 12
    }
    component Action: Button {
        Layout.alignment: Qt.AlignHCenter
        implicitHeight: 30
        implicitWidth: label.implicitWidth + 20
        contentItem: Text {
            id: label
            text: parent.text
            color: prompt.theme.foreground
            font.family: prompt.theme.font
            font.pixelSize: 12
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
    Word { text: "Choose your location"; font.bold: true; font.pixelSize: 13 }
    Action {
        objectName: "approximateLocation"
        text: prompt.session.locating ? "Finding your location…" : "Use approximate location"
        visible: !!prompt.session.engine.state && prompt.session.engine.state.source === "live"
        enabled: !prompt.session.locationPending
        onClicked: prompt.session.requestIpLocation()
    }
    Word {
        visible: !!prompt.session.engine.state && prompt.session.engine.state.source === "live"
        text: "Uses your public IP via wttr.in."
        font.pixelSize: 11
        opacity: .7
    }
    Word {
        visible: !prompt.session.ipLocationDismissed && !!prompt.session.locationError
        text: prompt.session.locationError
        font.pixelSize: 11
    }
    Action { objectName: "manualLocation"; text: "Choose manually"; onClicked: prompt.manualChosen() }
}
