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
    readonly property string texture: state && state.frame ? "file://" + runtime + state.frame.texture : ""
    readonly property string azimuthLut: state && state.frame ? "file://" + runtime + state.frame.azimuthLut : ""
    /// The selected station's row from `hello`, or null before it arrives.
    readonly property var site: state ? (sites.find(s => s.id === state.site.id) || null) : null
    /// The protocol's one rule for texture paths (`docs/protocol.md`): the
    /// literal `tex/` prefix and exactly one further segment that is not empty,
    /// `.`, or `..` and holds no `/`, backslash, or NUL. The engine applies the
    /// same rule before publishing.
    function validTexturePath(path) {
        if (typeof path !== "string" || path.indexOf("tex/") !== 0) return false;
        var name = path.slice(4);
        return name !== "" && name !== "." && name !== ".." && !/[\/\\\0]/.test(name);
    }
    /// A rendered product's frame (`docs/protocol.md`, frames): a lat/lon
    /// ground box, the map area within the texture, and one more band edge
    /// than the palette has colours. The engine applies the same rule before
    /// publishing.
    function validOverlay(frame) {
        var box = frame.overlay;
        if (!box || typeof box !== "object") return false;
        var numbers = [box.north, box.south, box.east, box.west];
        if (!numbers.every(function (v) { return typeof v === "number" && isFinite(v); })) return false;
        if (!(box.north > box.south && box.east > box.west)) return false;
        var crop = box.crop;
        if (!crop || !(crop.width > 0 && crop.height > 0)) return false;
        if (!(crop.x >= 0 && crop.y >= 0)) return false;
        if (!Array.isArray(box.levels) || box.levels.length !== (frame.palette || []).length + 1) return false;
        return box.levels.every(function (v) { return typeof v === "number" && isFinite(v); });
    }
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
            if (message.v !== 1) {
                incompatible = true;
                state = null;
                error = "Unsupported engine protocol version: " + message.v;
                socket.connected = false;
                return;
            }
            if (message.type === "hello") sites = message.sites;
            else if (message.type === "state") {
                if (!message.frame || !validTexturePath(message.frame.texture))
                    throw new Error("Invalid texture path: " + JSON.stringify(message.frame.texture));
                // A rendered product is a picture over a ground box: it has no
                // azimuth lookup, so an empty one is expected there rather
                // than a fault.
                if (message.frame.kind === "overlay") {
                    if (!validOverlay(message.frame))
                        throw new Error("Invalid overlay box: " + JSON.stringify(message.frame.overlay));
                } else if (!validTexturePath(message.frame.azimuthLut))
                    throw new Error("Invalid azimuth lookup path: " + JSON.stringify(message.frame.azimuthLut));
                state = message;
                error = "";
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
