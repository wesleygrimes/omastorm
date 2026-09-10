import QtQuick
import QtQuick.Layouts
import "Keys.js" as KeyMap

// The `?` sheet (DESIGN.md, picker and keys; keyboard map as built) after
// the KeyboardHints canvas: a scrim over the window and a card with KEYS,
// where the bindings live, two columns of key caps and what they do from
// the bindings in force, and a footer. `?` or Escape, or a click on the
// scrim, closes it; the window's own shortcuts stand down while it is open.
Item {
    id: sheet
    property var theme
    property var bindings: ({})       // action id -> canonical sequences
    property bool compact: false
    property real cardTop: 20         // the map's top edge; the card sits below it
    property bool open: false
    readonly property var columns: KeyMap.sheet(bindings)
    readonly property string closeKeys: (bindings.close || []).map(KeyMap.pretty).join(" or ")
    component Word: Text {
        color: sheet.theme.foreground
        font.family: sheet.theme.font
        font.pixelSize: 12
        elide: Text.ElideRight
        verticalAlignment: Text.AlignVCenter
    }
    visible: open
    focus: open
    function show() { open = true; forceActiveFocus(); }
    function close() { open = false; }
    Keys.onPressed: event => {
        if (!open) return;
        if (event.key === Qt.Key_Escape || event.text === "?") { close(); event.accepted = true; }
    }
    Rectangle {
        anchors.fill: parent
        color: Qt.alpha(sheet.theme.background, .5)
        MouseArea { anchors.fill: parent; onClicked: sheet.close() }
    }
    Rectangle {
        id: card
        width: Math.min(660, sheet.width - 40)
        x: Math.round((sheet.width - width) / 2)
        y: Math.round(Math.max(20, Math.min(sheet.cardTop + 20, sheet.height - height - 20)))
        height: column.implicitHeight + 36
        color: Qt.alpha(sheet.theme.background, .97)
        border.width: 1
        border.color: sheet.theme.foreground
        MouseArea { anchors.fill: parent } // a click on the card stays on the card
        ColumnLayout {
            id: column
            anchors.fill: parent
            anchors.margins: 18
            spacing: 12
            RowLayout {
                Layout.fillWidth: true
                spacing: 10
                Word { text: "KEYS"; font.bold: true; font.pixelSize: 14; font.letterSpacing: 2 }
                Item { Layout.fillWidth: true }
                Word { text: "rebindable in ~/.config/omastorm/config.toml"; font.pixelSize: 10; opacity: .55 }
            }
            GridLayout {
                Layout.fillWidth: true
                columns: sheet.compact ? 1 : 2
                columnSpacing: 28
                rowSpacing: 0
                Repeater {
                    model: sheet.columns
                    ColumnLayout {
                        id: keyColumn
                        required property var modelData
                        Layout.fillWidth: true
                        Layout.alignment: Qt.AlignTop
                        spacing: 0
                        Repeater {
                            model: keyColumn.modelData
                            RowLayout {
                                id: row
                                required property var modelData
                                Layout.fillWidth: true
                                Layout.preferredHeight: 26
                                spacing: 8
                                Row {
                                    Layout.preferredWidth: 124
                                    Layout.minimumWidth: 124
                                    spacing: 4
                                    Repeater {
                                        model: row.modelData.caps
                                        Rectangle {
                                            required property string modelData
                                            width: Math.max(18, cap.implicitWidth + 10)
                                            height: 18
                                            color: "transparent"
                                            border.width: 1
                                            border.color: Qt.alpha(sheet.theme.foreground, .4)
                                            Word { id: cap; anchors.centerIn: parent; text: parent.modelData; font.pixelSize: 11 }
                                        }
                                    }
                                }
                                Word { text: row.modelData.label; opacity: .85; Layout.fillWidth: true }
                            }
                        }
                    }
                }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(sheet.theme.foreground, .17) }
            Word {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                elide: Text.ElideNone
                font.pixelSize: 10
                opacity: .55
                lineHeight: 1.4
                text: sheet.closeKeys
                    ? "With nothing else open, " + sheet.closeKeys + " closes the window."
                    : "No key closes the window."
            }
        }
    }
}
