import QtQuick
import Quickshell
import Quickshell.Io
import "Location.js" as Location

// ~/.local/state/omastorm/state.json (DESIGN.md, location and remembered
// state): the last camera and the UI radar lock. Onboarding and navigation
// write this file, never config.toml. OMASTORM_STATE names another file for
// checks and captures; when OMASTORM_CONFIG is set the machine's own state
// is not read unless OMASTORM_STATE names one.
QtObject {
    id: root
    readonly property string path: Quickshell.env("OMASTORM_STATE")
        || (Quickshell.env("OMASTORM_CONFIG") ? "" : (Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state") + "/omastorm/state.json")
    property var parsed: Location.parseState("")
    readonly property bool ready: stateRead
    property bool stateRead: false
    property string error: ""
    readonly property var lat: parsed.lat
    readonly property var lon: parsed.lon
    readonly property var span: parsed.span
    readonly property var lock: parsed.lock
    readonly property string name: parsed.name
    // The trio we last wrote on disk via `overlay`, so a sweep identical to
    // it can be skipped. Reset by every `snapshot` so a fresh view lands on
    // disk at least once per state change, not just on equal re-fires.
    property string lastOverlaySite: ""
    property string lastOverlayScan: ""
    property bool lastOverlayLive: false
    property FileView file: FileView {
        path: root.path
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: { root.parsed = Location.parseState(text()); root.stateRead = true; }
        onLoadFailed: { root.parsed = Location.parseState(""); root.stateRead = true; }
    }
    function snapshot(viewLat, viewLon, span, lock, name, site, scan, live) {
        var text = JSON.stringify(Location.stateObject(viewLat, viewLon, span, lock, name, site, scan, live));
        parsed = Location.parseState(text);
        // A full snapshot rewrites the camera and the on-screen view in one
        // pass. Set the overlay dedup to whatever trio landed on disk so a
        // sweep that matches it does not reissue the same write.
        lastOverlaySite = site || "";
        lastOverlayScan = scan || "";
        lastOverlayLive = !!site && live === true;
        write(text);
    }
    // The window owns the camera and lock on disk; the on-screen view
    // (`site` / `scan` / `live` — see `Location.overlay`) is shared with the
    // bar, so a sweep here overlays those three only and never rewrites the
    // rest. `parsed` is kept fresh by the FileView above, so external writes
    // land here before the next sweep fires. A sweep identical to the one
    // we last wrote is a no-op — both processes (the window and the bar)
    // subscribe to the same broadcast and would otherwise double-write on
    // every tick.
    function overlay(site, scan, live) {
        if (!path) return;
        if (site === lastOverlaySite && scan === lastOverlayScan && live === lastOverlayLive) return;
        var text = JSON.stringify(Location.overlay(parsed, site, scan, live));
        lastOverlaySite = site || "";
        lastOverlayScan = scan || "";
        lastOverlayLive = !!site && live === true;
        write(text);
    }
    function write(text) {
        if (!path) return;
        var slash = path.lastIndexOf("/");
        var dir = slash >= 0 ? path.slice(0, slash) : ".";
        writer.command = ["sh", "-c",
            "mkdir -p -- \"$1\" && printf '%s\\n' \"$3\" > \"$2\" && mv -f -- \"$2\" \"$4\"",
            "omastorm-state", dir, path + ".tmp", text, path];
        writer.running = true;
    }
    property Process writer: Process {
        command: ["true"]
        onExited: function (exitCode) {
            root.error = exitCode === 0 ? "" : "Could not write state.json";
        }
    }
    Component.onCompleted: { if (!path) stateRead = true; }
}
