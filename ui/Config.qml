import QtQuick
import Quickshell
import Quickshell.Io
import "Toml.js" as Toml

// ~/.config/omastorm/config.toml (docs/protocol.md, configuration): the
// home station, the home view's own centre, whether the map centre picks
// the station, the treatment, the weak-return floor, and the `[keys]`
// table. Watched like the theme files, so an edit applies to the running
// window. OMASTORM_CONFIG names another file for checks and captures; a
// missing file is no config.
//
// Omarchy's own location (DESIGN.md, site model), the file its weather
// panel writes, is read beside it for the current-location home. It is
// this machine's setting, so a check or capture that names its own config
// leaves it alone unless OMASTORM_LOCATION names a location file too.
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
    readonly property string homeSite: typeof values.home_site === "string" ? values.home_site.trim().toUpperCase() : ""
    // undefined while unset: the engine's shared flag stands.
    readonly property var follow: typeof values.follow === "boolean" ? values.follow : undefined
    // The raw value; the window judges it against the three treatments.
    readonly property var treatment: values.treatment
    // The raw value; the window judges it: a dBZ number, false, or unset.
    readonly property var weakFloor: values.weak_floor
    // The raw values; the window judges them as a latitude/longitude pair.
    // Together they place the home view on a point of the user's choosing
    // instead of the home station's own offset, and pick the home station
    // when `home_site` is unset.
    readonly property var homeLat: values.home_lat
    readonly property var homeLon: values.home_lon
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
        onLoaded: { root.raw = text(); root.values = Toml.parse(root.raw); root.configRead = true; }
        onLoadFailed: { root.raw = ""; root.values = ({}); root.configRead = true; }
    }
    // The file as last read, so a save keeps every other line.
    property string raw: ""
    property string pendingText: ""
    // Save `id` as home_site: the top-level line is replaced in place, or
    // added above the first table; nothing else in the file changes. The
    // directory may not exist yet, so the write follows its creation, and
    // the watch above reloads the result like any other edit.
    function setHome(id) {
        var lines = raw.length ? raw.replace(/\n$/, "").split("\n") : [];
        var line = 'home_site = "' + id + '"', out = [], done = false, inTable = false;
        for (var l of lines) {
            if (/^\s*\[/.test(l)) inTable = true;
            if (!done && !inTable && /^\s*home_site\s*=/.test(l)) { out.push(line); done = true; }
            else out.push(l);
        }
        if (!done) {
            var at = out.findIndex(l => /^\s*\[/.test(l));
            if (at < 0) out.push(line); else out.splice(at, 0, line);
        }
        pendingText = out.join("\n") + "\n";
        mkdir.running = true;
    }
    property Process mkdir: Process {
        command: ["mkdir", "-p", root.path.substring(0, root.path.lastIndexOf("/"))]
        // The view does not report its own write, so the values follow at once.
        onExited: { file.setText(root.pendingText); root.raw = root.pendingText; root.values = Toml.parse(root.raw); }
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
            var lat = Number(json.latitude), lon = Number(json.longitude);
            if (!isFinite(lat) || !isFinite(lon) || Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
            return { name: typeof json.name === "string" ? json.name : "", lat: lat, lon: lon };
        } catch (e) { return null; }
    }
    Component.onCompleted: { if (!locationPath) locationRead = true; }
}
