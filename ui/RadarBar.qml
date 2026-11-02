import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui
import qs.Commons
import qs.Commons as Commons
import "Keys.js" as KeyMap

BarWidget {
    id: root
    moduleName: Runtime.pluginId
    property var session: PluginSession
    property bool opened: false
    property bool popoutSwitchClosing: false
    // Dev-only acknowledgement: the owner waits for the actual loaded widget,
    // rather than treating asynchronous registry discovery as successful QML.
    property var developmentStatus: Runtime.development ? statusFactory.createObject(root) : null
    property Component statusFactory: Component {
        IpcHandler {
            target: "omastorm-dev"
            function status(): string {
                return JSON.stringify({revision: Runtime.instance.revision, pluginId: root.moduleName,
                    runtime: root.session.engine.runtime, connected: !!root.session.engine.state,
                    window: root.session.windowOpen, popover: root.opened});
            }
        }
    }
    readonly property var state: session.engine.state
    readonly property bool live: state && state.mode === "live" && state.connection.status === "ok"
    readonly property bool down: !state || state.connection.status === "offline" || state.connection.status === "unavailable"
    function open() {
        popoutSwitchClosing = false;
        opened = true;
    }
    function close() { opened = false; }
    function closeForPopoutSwitch() { popoutSwitchClosing = true; close(); }
    function expand() {
        Quickshell.execDetached(["omarchy", "shell", "shell", session.windowOpen ? "summon" : "toggle", Runtime.pluginId, "{}"]);
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
                RadarMark { anchors.centerIn: parent; ink: button.foreground; opacity: root.live ? 1 : .6 }
                Rectangle { anchors.right: parent.right; anchors.bottom: parent.bottom; width: 5; height: 5; color: Commons.Color.urgent; visible: root.down }
                // An update waiting on a shell restart; the popover names it.
                Rectangle { anchors.right: parent.right; anchors.bottom: parent.bottom; width: 5; height: 5; color: Commons.Color.accent; visible: !root.down && root.session.updatePending }
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
        // Fixed height: binding to the Loader item jumps from the 440
        // fallback to a settling layout on every open (shrink/grow jitter).
        // Card geometry is stable (308×372 content at popover_scale 1, plus
        // the unscaled 28 inset of padding and border around it).
        padding: 12
        borderSpec: Border.flat(Commons.Color.accent, 2)
        contentWidth: KeyMap.scaled(KeyMap.POPOVER_WIDTH, root.session.popoverScale) + 28
        contentHeight: KeyMap.scaled(KeyMap.POPOVER_HEIGHT, root.session.popoverScale) + 28
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
