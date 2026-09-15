import QtQuick
import Quickshell
import Quickshell.Io
import "Toml.js" as Toml

QtObject {
    id: root
    property bool registerIpc: true
    readonly property string themePath: Quickshell.env("OMASTORM_THEME_DIR") || (Quickshell.env("HOME") + "/.local/state/omarchy/current/theme")
    readonly property string userPath: Quickshell.env("OMASTORM_USER_SHELL") || (Quickshell.env("HOME") + "/.config/omarchy/shell.toml")
    // `omarchy theme set` replaces the whole theme directory (`rm -rf`, then
    // `mv`) and then rewrites `theme.name` beside it. A watched file's change
    // event can arrive while the old directory is gone and before the new one
    // is in place, and reloading then fails and leaves the watch on a deleted
    // file. So theme events wait for the swap to settle before reloading, and
    // `theme.name`, rewritten in place, marks every switch.
    readonly property string themeNamePath: themePath.replace(/\/+$/, "").replace(/\/[^\/]*$/, "") + "/theme.name"
    property var colors: ({})
    property var shell: ({})
    property var user: ({})

    function parse(raw) { return Toml.parse(raw); }
    function setting(key, fallback) {
        return user[key] !== undefined ? user[key] : shell[key] !== undefined ? shell[key] : fallback;
    }
    function color(value, fallback) {
        var resolved = colors[value] !== undefined ? colors[value] : value;
        return typeof resolved === "string" && /^#[0-9a-fA-F]{6}$/.test(resolved) ? resolved : fallback;
    }
    readonly property var snapshot: ({
        background: color(setting("popups.background", colors.background), "#1a1b26"),
        foreground: color(setting("popups.text", colors.foreground), "#a9b1d6"),
        accent: color(colors.accent, "#7aa2f7"),
        // Condition colours (DESIGN.md): stale is the
        // theme's yellow, unavailable and offline its red.
        yellow: color(colors.yellow, "#e0af68"),
        red: color(colors.red, "#f7768e"),
        // Both Text and Canvas resolve the generic family through Qt/fontconfig.
        font: "monospace",
        baseSize: Number(setting("font.base-size", 12)) > 0 ? Number(setting("font.base-size", 12)) : 12
    })
    function reload() {
        colorsFile.reload();
        shellFile.reload();
        userFile.reload();
    }
    property Timer settle: Timer {
        interval: 150
        onTriggered: root.reload()
    }
    property FileView colorsFile: FileView {
        path: root.themePath + "/colors.toml"
        watchChanges: true
        printErrors: false
        onFileChanged: root.settle.restart()
        onLoaded: root.colors = root.parse(text())
        onLoadFailed: root.colors = ({})
    }
    property FileView shellFile: FileView {
        path: root.themePath + "/shell.toml"
        watchChanges: true
        printErrors: false
        onFileChanged: root.settle.restart()
        onLoaded: root.shell = root.parse(text())
        onLoadFailed: root.shell = ({})
    }
    property FileView userFile: FileView {
        path: root.userPath
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: root.user = root.parse(text())
        onLoadFailed: root.user = ({})
    }
    property FileView themeNameFile: FileView {
        path: root.themeNamePath
        watchChanges: true
        printErrors: false
        onFileChanged: {
            reload();
            root.settle.restart();
        }
    }
    property IpcHandler ipc: IpcHandler {
        target: root.registerIpc ? "theme" : ""
        function reload(): void { root.reload(); }
    }
}
