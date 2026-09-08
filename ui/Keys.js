.pragma library
// The keyboard map (DESIGN.md, picker and keyboard; keyboard map as built):
// the actions, their default bindings, the `[keys]` table from config.toml
// laid over them, and the rows of the `?` sheet. Pure functions so a check
// can drive them; the window supplies `canon`, which asks Qt whether a
// sequence parses (an empty string means it does not).

// Bindings are Qt key sequences separated by spaces: "h Left" binds both.
// Shift+L is the lock because lowercase l pans; the digit keys pick a
// treatment; `w` toggles the weak-return floor; `?` opens the sheet; Escape with nothing open closes the window.
var ACTIONS = [
    { id: "search", keys: "/ s" },
    { id: "nearest", keys: "n" },
    { id: "lock", keys: "Shift+L" },
    { id: "home", keys: "Shift+H" },
    { id: "pan_left", keys: "h Left" },
    { id: "pan_down", keys: "j Down" },
    { id: "pan_up", keys: "k Up" },
    { id: "pan_right", keys: "l Right" },
    { id: "zoom_in", keys: "+ =" },
    { id: "zoom_out", keys: "-" },
    { id: "reset", keys: "0" },
    { id: "previous_frame", keys: "[" },
    { id: "next_frame", keys: "]" },
    { id: "play", keys: "Space" },
    { id: "oldest", keys: "Home" },
    { id: "newest", keys: "End" },
    { id: "pixels", keys: "1" },
    { id: "glyphs", keys: "2" },
    { id: "stipple", keys: "3" },
    { id: "weak", keys: "w" },
    { id: "help", keys: "?" },
    { id: "close", keys: "Escape" }
];
// The sheet's two columns (DESIGN.md, phase 4 UX pass). A row of
// several actions shows each one's first key and names the alternates.
var ROWS = [
    [{ label: "search sites", actions: ["search"] },
     { label: "nearest site", actions: ["nearest"] },
     { label: "lock / release site", actions: ["lock"] },
     { label: "save site as home", actions: ["home"] },
     { label: "pan", actions: ["pan_left", "pan_down", "pan_up", "pan_right"] },
     { label: "zoom", actions: ["zoom_in", "zoom_out"] },
     { label: "reset to home view", actions: ["reset"] }],
    [{ label: "previous frame", actions: ["previous_frame"] },
     { label: "next frame", actions: ["next_frame"] },
     { label: "play / pause", actions: ["play"] },
     { label: "oldest / newest frame", actions: ["oldest", "newest"] },
     { label: "Pixels, Glyphs, Stipple", actions: ["pixels", "glyphs", "stipple"] },
     { label: "weak returns: hide / show", actions: ["weak"] },
     { label: "this sheet · esc closes", actions: ["help"] }]
];
var TREATMENTS = ["PIXELS", "GLYPHS", "STIPPLE"];

function split(value) { return String(value).trim().split(/\s+/).filter(s => s !== ""); }

// The defaults with the `[keys]` table (action id -> string) laid over them.
// A value that is not a quoted string, a sequence Qt cannot parse, an
// unknown action, or a key another action already has leaves that action
// on its default and is reported; an empty string unbinds. Returns
// { bindings, errors }.
function resolve(table, canon) {
    var bindings = {}, defaults = {}, errors = [], configured = [];
    for (var action of ACTIONS) bindings[action.id] = defaults[action.id] = split(action.keys).map(canon);
    for (var key in table) {
        var value = table[key];
        if (!ACTIONS.some(a => a.id === key)) { errors.push("[keys] " + key + " is not an action"); continue; }
        if (typeof value !== "string") { errors.push("[keys] " + key + " must be a quoted string"); continue; }
        var sequences = split(value), bad = sequences.find(s => canon(s) === "");
        if (bad !== undefined) { errors.push("[keys] " + key + " = \"" + value + "\": " + bad + " is not a key"); continue; }
        bindings[key] = sequences.map(canon);
        configured.push(key);
    }
    var holder = function(sequence, except) { return ACTIONS.map(a => a.id).find(id => id !== except && bindings[id].indexOf(sequence) >= 0); };
    for (var id of configured) {
        for (var sequence of bindings[id]) {
            var other = holder(sequence, id);
            if (!other) continue;
            errors.push("[keys] " + id + " = \"" + table[id] + "\": " + sequence + " is " + other + "'s key");
            bindings[id] = defaults[id];
            break;
        }
    }
    // Two defaults never share a key, and a reverted action holds only
    // defaults, so any key still bound twice goes to the first action.
    var owner = {};
    for (var each of ACTIONS) bindings[each.id] = bindings[each.id].filter(s => owner[s] ? false : (owner[s] = each.id));
    return { bindings: bindings, errors: errors };
}

// The treatment setting, or "" when unset; a value outside the three is reported.
function treatment(value, errors) {
    if (value === undefined || value === "") return "";
    var upper = String(value).toUpperCase();
    if (TREATMENTS.indexOf(upper) >= 0) return upper;
    errors.push("treatment = \"" + value + "\": not PIXELS, GLYPHS, or STIPPLE");
    return "";
}

// The weak-return floor (DESIGN.md, weak-return floor): measured returns
// below this many dBZ draw nothing and the legend says so. `weak_floor` in
// config.toml is a number, or false to draw every measured return; unset
// is the default floor. Returns the floor, null for off; a value that is
// neither is reported and leaves the default.
var DEFAULT_FLOOR = 5;
function weakFloor(value, errors) {
    if (value === undefined) return DEFAULT_FLOOR;
    if (value === false) return null;
    if (typeof value === "number" && isFinite(value) && value >= -32 && value <= 95) return value;
    errors.push("weak_floor = " + JSON.stringify(value) + ": a dBZ number or false");
    return DEFAULT_FLOOR;
}
// The home view's centre (docs/protocol.md, configuration): `home_lat` and
// `home_lon` in config.toml put the camera on a point of the user's
// choosing instead of the home station's own offset. Both are needed and
// each must be in range; returns { lat, lon }, or null when the file names
// neither. A mistake is reported and leaves the station's own home view.
function homePoint(lat, lon, errors) {
    if (lat === undefined && lon === undefined) return null;
    if (lat === undefined || lon === undefined) {
        errors.push("home_lat and home_lon: the home view needs both");
        return null;
    }
    if (typeof lat !== "number" || !isFinite(lat) || lat < -90 || lat > 90) {
        errors.push("home_lat = " + JSON.stringify(lat) + ": a latitude between -90 and 90");
        return null;
    }
    if (typeof lon !== "number" || !isFinite(lon) || lon < -180 || lon > 180) {
        errors.push("home_lon = " + JSON.stringify(lon) + ": a longitude between -180 and 180");
        return null;
    }
    return { lat: lat, lon: lon };
}

// OMASTORM_WEAK, set by the capture scripts, outranks the file: "off" or a
// dBZ number. Empty or unparsable is no override (undefined).
function envFloor(text) {
    if (!text) return undefined;
    if (String(text).toLowerCase() === "off") return null;
    var n = Number(text);
    return isFinite(n) ? n : undefined;
}

// A sequence as the sheet's key cap shows it: letters in their case,
// arrows and the named keys as words or symbols, modifiers spelled out.
var NAMES = { Space: "space", Esc: "esc", Escape: "esc", Left: "←", Right: "→", Up: "↑", Down: "↓", Return: "↵", Enter: "↵", Backspace: "⌫", Tab: "tab", PgUp: "pgup", PgDown: "pgdn", Del: "del", Ins: "ins", "-": "−" };
function pretty(sequence) {
    var shiftLetter = sequence.match(/^Shift\+([A-Z])$/);
    if (shiftLetter) return shiftLetter[1];
    if (/^[A-Z]$/.test(sequence)) return sequence.toLowerCase();
    return NAMES[sequence] !== undefined ? NAMES[sequence] : sequence;
}
function isArrows(sequences) {
    var arrows = ["Left", "Down", "Up", "Right"];
    return sequences.length === 4 && arrows.every(a => sequences.indexOf(a) >= 0);
}
// The sheet's rows for a set of bindings: [{ caps: [...], label }] per column.
function sheet(bindings) {
    return ROWS.map(column => column.map(row => {
        var lists = row.actions.map(id => bindings[id] || []);
        if (lists.length === 1) return { caps: lists[0].map(pretty), label: row.label };
        var alternates = lists.map(list => list.slice(1)).reduce((all, list) => all.concat(list), []);
        var note = !alternates.length ? "" : isArrows(alternates) ? " (arrows too)" : " (" + alternates.map(pretty).join(" ") + " too)";
        return { caps: lists.filter(list => list.length).map(list => pretty(list[0])), label: row.label + note };
    }));
}
