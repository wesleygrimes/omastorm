import QtQuick
import Quickshell
import Quickshell.Io

// No qs.* imports or desktop shell: mounts the production card and panel.
ShellRoot {
    id: harness
    property var session: PluginSession
    property bool expanded: false
    FloatingWindow {
        id: preview
        visible: true
        implicitWidth: 380
        implicitHeight: 500
        color: "#181414"
        Item {
            id: picture
            anchors.fill: parent
            Rectangle {
                x: 0; y: 0; width: parent.width; height: 32; color: theme.snapshot.background
                RadarMark { x: 294; y: 8; ink: theme.snapshot.foreground }
            }
            Rectangle {
                x: 22; y: 42; width: 336; height: 400
                color: theme.snapshot.background
                border.width: 2; border.color: theme.snapshot.accent
                visible: !harness.expanded
                Popover {
                    id: popover
                    x: 14; y: 14; width: 308
                    session: harness.session
                    onExpandRequested: { harness.expanded = true; panel.open("{}"); }
                    onCloseRequested: harness.expanded = true
                }
            }
        }
    }
    Theme { id: theme; registerIpc: false }
    Panel { id: panel }
    IpcHandler {
        target: "popover"
        function expand(): void { popover.expandRequested(); }
        function reopen(): void { harness.expanded = false; }
        function closeWindow(): void { panel.dismiss(); }
        function treatment(value: string): void { panel.treatment = value; }
        function step(delta: int): void { popover.step(delta); }
        function play(): void { popover.play(); }
        function status(): string {
            return JSON.stringify({site: popover.state ? popover.state.site.id : "", frame: popover.scan ? popover.scan.id : "",
                playing: popover.state ? popover.state.playing : false, treatment: session.treatment,
                connected: popover.engine.socket.connected, retry: popover.engine.reconnect.running, transport: popover.engine.error, condition: popover.condition, text: popover.statusText, expanded: harness.expanded,
                window: panel.opened, windowSite: panel.siteId, windowFrame: panel.scan ? panel.scan.id : "",
                windowPlaying: panel.playing, windowTreatment: panel.treatment, error: session.startupError,
                notice: popover.gapNotice, windowNotice: panel.gapNotice, gaps: popover.slots.filter(s => s.gap).length,
                needsLocation: session.needsLocation, lat: session.centerLat, lon: session.centerLon});
        }
        function capture(path: string): void { picture.grabToImage(result => result.saveToFile(path)); }
        function quit(): void { Qt.quit(); }
    }
}
