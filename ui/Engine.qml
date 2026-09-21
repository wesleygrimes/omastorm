import QtQuick
import Quickshell
import Quickshell.Io

QtObject {
    id: engine
    property var state: null
    property var sites: []
    /// Transport and parsing trouble: disconnected, unreadable message,
    /// unknown protocol version. Cleared by the next valid state.
    property string error: ""
    /// The engine's answer to this client's last command when it could not be
    /// carried out. Only this client hears it, and a later state does not
    /// clear it; the next command from here does, except a tile request,
    /// which is the map's housekeeping rather than something the user did.
    property string rejection: ""
    property bool incompatible: false
    /// One tile answering this client's `tiles_needed`; a reply, not state.
    signal tileReady(var tile)
    /// Places answering this client's `search_places`; a reply, not state.
    signal placesReady(var message)
    readonly property string runtime: Quickshell.env("XDG_RUNTIME_DIR") + "/omastorm/"
    property var sources: []
    readonly property string texture: state && state.frame && state.frame.texture ? "file://" + runtime + state.frame.texture : ""
    readonly property string azimuthLut: state && state.frame && state.frame.azimuthLut ? "file://" + runtime + state.frame.azimuthLut : ""
    readonly property string selectedSiteId: {
        if (!state || !state.selection || !state.selection.target) return "";
        return state.selection.target.kind === "site" ? (state.selection.target.siteId || "") : "";
    }
    /// The selected station's row from `hello`, or null when the selection is not a polar site.
    readonly property var site: {
        var id = selectedSiteId;
        return id ? (sites.find(s => s.id === id) || null) : null;
    }
    readonly property var source: {
        if (!state || !state.selection) return null;
        return sources.find(s => s.id === state.selection.sourceId) || null;
    }
    /// The protocol's one rule for texture paths (`docs/protocol.md`): the
    /// literal `tex/` prefix and exactly one further segment that is not empty,
    /// `.`, or `..` and holds no `/`, backslash, or NUL. The engine applies the
    /// same rule before publishing.
    function validTexturePath(path) {
        if (typeof path !== "string" || path.indexOf("tex/") !== 0) return false;
        var name = path.slice(4);
        return name !== "" && name !== "." && name !== ".." && !/[\/\\\0]/.test(name);
    }
    /// The tile path rule (`docs/protocol.md`): the literal `tiles/` prefix,
    /// a set (`ne` or `osm`), a zoom and a column as decimal integers, and
    /// one further segment under the texture rule. The engine applies the
    /// same rule before publishing.
    function validTilePath(path) {
        if (typeof path !== "string") return false;
        var parts = path.split("/");
        if (parts.length !== 5 || parts[0] !== "tiles" || (parts[1] !== "ne" && parts[1] !== "osm")) return false;
        if (!/^[0-9]+$/.test(parts[2]) || !/^[0-9]+$/.test(parts[3])) return false;
        var name = parts[4];
        return name !== "" && name !== "." && name !== ".." && !/[\\\0]/.test(name);
    }
    function receive(data) {
        if (incompatible) return;
        try {
            var message = JSON.parse(data);
            if (message.v !== 2) {
                incompatible = true;
                state = null;
                error = "Unsupported engine protocol version: " + message.v;
                socket.connected = false;
                return;
            }
            if (message.type === "hello") {
                sites = message.sites;
                sources = message.sources || [];
            }
            else if (message.type === "state") {
                if (message.frame === null || message.frame === undefined) {
                    state = message;
                    error = "";
                } else {
                    if (!validTexturePath(message.frame.texture))
                        throw new Error("Invalid texture path: " + JSON.stringify(message.frame.texture));
                    if (message.frame.kind === "mosaic") {
                        // Mosaic frames have texture only; no azimuth lookup.
                    } else if (!validTexturePath(message.frame.azimuthLut))
                        throw new Error("Invalid azimuth lookup path: " + JSON.stringify(message.frame.azimuthLut));
                    state = message;
                    error = "";
                }
            } else if (message.type === "error") rejection = message.message;
            else if (message.type === "tile_ready") {
                if (!validTilePath(message.path))
                    throw new Error("Invalid tile path: " + JSON.stringify(message.path));
                tileReady(message);
            } else if (message.type === "places") {
                placesReady(message);
            }
        } catch (e) { state = null; error = "Invalid engine message: " + e; }
    }
    function send(command) {
        if (command.type !== "tiles_needed" && command.type !== "search_places") rejection = "";
        socket.write(JSON.stringify(command) + "\n");
    }
    property var socket: socketFactory.createObject(engine)
    property Component socketFactory: Component {
        Socket {
            path: engine.runtime + "engine.sock"
            connected: true
            parser: SplitParser { onRead: data => engine.receive(data) }
            onConnectedChanged: {
                if (!connected && !engine.incompatible) {
                    engine.state = null;
                    engine.error = "Radar engine disconnected. Reconnecting…";
                }
            }
            onError: {
                if (!engine.incompatible) engine.error = "Radar engine unavailable. Reconnecting…";
            }
        }
    }
    property Timer reconnect: Timer {
        interval: 1000
        repeat: true
        running: engine.socket && !engine.socket.connected && !engine.incompatible
        // A failed initial connect leaves Quickshell's underlying socket
        // allocated; toggling connected cannot retry it. Replace the object.
        onTriggered: {
            var previous = engine.socket;
            engine.socket = engine.socketFactory.createObject(engine);
            previous.destroy();
        }
    }
}
