import QtQuick
import Quickshell
import qs.Ui
import qs.Commons
import "Status.js" as Status

BarWidget {
    id: root
    moduleName: "com.omastorm.radar"
    property var session: PluginSession
    property bool opened: false
    property bool popoutSwitchClosing: false
    readonly property var state: session.engine.state
    // The feed condition (Status.js), "" while no engine answers. The mark
    // is full strength only while frames are arriving; each other
    // condition has its own cue, so loading, stale, a silent station, and
    // no engine at all never look alike:
    //   ok / archived   full mark
    //   loading         dim mark, no dot
    //   stale           mark near full, yellow dot
    //   unavailable,
    //   offline         dim mark, solid urgent dot (cached frames stand)
    //   no engine       faint mark, hollow dot
    readonly property string condition: Status.condition(state)
    readonly property bool live: condition === "ok" || condition === "archived"
    readonly property bool down: condition === "offline" || condition === "unavailable"
    readonly property bool stale: condition === "stale"
    readonly property bool noEngine: !state
    readonly property color statusInk: stale ? session.theme.snapshot.yellow : Color.urgent
    Accessible.role: Accessible.Button
    Accessible.name: Status.summary(state, state ? state.frame : null, session.engine.site ? session.engine.site.name : "")
    function open() {
        popoutSwitchClosing = false;
        opened = true;
    }
    function close() { opened = false; }
    function closeForPopoutSwitch() { popoutSwitchClosing = true; close(); }
    function expand() {
        Quickshell.execDetached(["omarchy", "shell", "shell", session.windowOpen ? "summon" : "toggle", "com.omastorm.radar", "{}"]);
        close();
    }
    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight
    BarIconButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        slotSize: 27
        opticalSize: 16
        useActiveColor: false
        active: root.opened
        iconComponent: Component {
            Item {
                RadarMark {
                    anchors.centerIn: parent; ink: button.foreground
                    opacity: root.live ? 1 : root.stale ? .85 : root.noEngine ? .35 : .6
                }
                Rectangle {
                    anchors.right: parent.right; anchors.bottom: parent.bottom
                    width: 5; height: 5
                    visible: root.down || root.stale || root.noEngine
                    color: root.noEngine ? "transparent" : root.statusInk
                    border.width: root.noEngine ? 1 : 0
                    border.color: button.foreground
                    opacity: root.noEngine ? .6 : 1
                }
            }
        }
        onPressed: b => { if (b === Qt.LeftButton) { if (root.opened) root.close(); else root.open(); } }
    }
    KeyboardPanel {
        id: popup
        anchorItem: button
        bar: root.bar
        owner: root
        open: root.opened
        // The reviewed card has 308 px content within 336: the 14 px inset
        // includes the 2 px border (KeyboardPanel adds border to padding).
        padding: 12
        borderSpec: Border.flat(Color.accent, 2)
        contentWidth: 336
        contentHeight: content.item ? content.item.implicitHeight + 28 : 440
        focusTarget: content.item
        Loader {
            id: content
            anchors.fill: parent
            active: root.opened
            sourceComponent: Popover {
                session: root.session
                onCloseRequested: root.close()
                onExpandRequested: root.expand()
            }
        }
    }
}
