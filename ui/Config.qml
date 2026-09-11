import QtQuick
import Quickshell
import Quickshell.Io
import "Toml.js" as Toml

// ~/.config/omastorm/config.toml (docs/protocol.md, configuration):
// deliberate preferences — an explicit map centre, a locked radar, the
// treatment, the weak-return floor, and the `[keys]` table. Watched like
// the theme files, so an edit applies to the running window. OMASTORM_CONFIG
// names another file for checks and captures; a missing file is no config.
//
// Omarchy's own location (DESIGN.md, location), the file its weather panel
// writes, is read beside it when no explicit or remembered centre exists.
// It is this machine's setting, so a check or capture that names its own
// config leaves it alone unless OMASTORM_LOCATION names a location file too.
QtObject {
    id: root
    readonly property string path: Quickshell.env("OMASTORM_CONFIG") || (Quickshell.env("HOME") + "/.config/omastorm/config.toml")
    readonly property string locationPath: Quickshell.env("OMASTORM_LOCATION")
        || (Quickshell.env("OMASTORM_CONFIG") ? "" : Quickshell.env("HOME") + "/.local/state/omarchy/settings/weather.json")
    property var values: ({})
    // False until both files have been read or found missing, so a surface
    // can wait for them before acting on the engine's first state.
    readonly property bool ready: configRead && locationRead
    property bool configRead: false
    property bool locationRead: false
    readonly property var centerLat: typeof values.center_lat === "number" ? values.center_lat : undefined
    readonly property var centerLon: typeof values.center_lon === "number" ? values.center_lon : undefined
    readonly property string lockedRadar: typeof values.locked_radar === "string" ? values.locked_radar.trim().toUpperCase() : ""
    // The raw value; the window judges it against the three treatments.
    readonly property var treatment: values.treatment
    // The raw value; the window judges it: a dBZ number, false, or unset.
    readonly property var weakFloor: values.weak_floor
    // The `[keys]` table as action id -> value, for Keys.resolve.
    readonly property var keys: {
        var table = {};
        for (var key in values) if (key.indexOf("keys.") === 0) table[key.slice(5)] = values[key];
        return table;
    }
    // { name, lat, lon } from weather.json, or null when the file is
    // missing, unreadable, or has no coordinates.
    property var location: null
    property FileView file: FileView {
        path: root.path
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: { root.values = Toml.parse(text()); root.configRead = true; }
        onLoadFailed: { root.values = ({}); root.configRead = true; }
    }
    property FileView locationFile: FileView {
        path: root.locationPath
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: { root.location = root.parseLocation(text()); root.locationRead = true; }
        onLoadFailed: { root.location = null; root.locationRead = true; }
    }
    function parseLocation(raw) {
        try {
            var json = JSON.parse(raw);
            var lat = json.latitude, lon = json.longitude;
            if (typeof lat !== "number" || typeof lon !== "number") return null;
            if (!isFinite(lat) || !isFinite(lon) || Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
            return { name: typeof json.name === "string" ? json.name : "", lat: lat, lon: lon };
        } catch (e) { return null; }
    }
    Component.onCompleted: { if (!locationPath) locationRead = true; }
}
