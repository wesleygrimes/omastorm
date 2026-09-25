.pragma library
// METAR overlay (DESIGN.md): ICAO chips replace city names when the
// feature is on. FAA flight-category colors are a data palette, not theme
// chrome. There is no decoded English.

var COLORS = {
    vfr: "#00c000",
    mvfr: "#0050e0",
    ifr: "#e00000",
    lifr: "#e000e0"
};

function color(category) {
    return COLORS[String(category || "").toLowerCase()] || "";
}

function label(category) {
    var c = String(category || "").toUpperCase();
    return c === "VFR" || c === "MVFR" || c === "IFR" || c === "LIFR" ? c : "";
}

function enabledFromConfig(values) {
    if (!values || values["metar.show"] === undefined) return false;
    return values["metar.show"] === true;
}

function pickFromConfig(values) {
    if (!values || values["metar.pick"] === undefined) return "nearest";
    var pick = String(values["metar.pick"]).toLowerCase();
    return pick === "priority" || pick === "nearest" ? pick : "";
}

function countFromConfig(values) {
    if (!values || values["metar.count"] === undefined) return 16;
    var n = values["metar.count"];
    if (typeof n !== "number" || !isFinite(n) || n !== Math.floor(n) || n < 1 || n > 16)
        return 0;
    return n;
}

function markFromConfig(values) {
    if (!values || values["metar.mark"] === undefined) return "pin";
    var mark = String(values["metar.mark"]).toLowerCase();
    return mark === "chip" || mark === "ink" || mark === "pin" ? mark : "";
}

function alwaysOnFromConfig(values) {
    if (!values || values["metar.always_on_when_in_view"] === undefined) return [];
    var raw = values["metar.always_on_when_in_view"];
    if (typeof raw !== "string") return null;
    var ids = [], seen = {};
    for (var part of raw.trim().split(/\s+/)) {
        if (!part) continue;
        var id = part.toUpperCase();
        if (!/^[A-Z0-9]{3,4}$/.test(id)) return null;
        if (seen[id]) continue;
        seen[id] = true;
        ids.push(id);
        if (ids.length >= 16) break;
    }
    return ids;
}

function configError(values) {
    if (!values) return "";
    if (values["metar.show"] !== undefined && typeof values["metar.show"] !== "boolean")
        return "metar.show must be true or false";
    if (values["metar.pick"] !== undefined && !pickFromConfig(values))
        return "metar.pick must be nearest or priority";
    if (values["metar.count"] !== undefined && !countFromConfig(values))
        return "metar.count must be 1 through 16";
    if (values["metar.always_on_when_in_view"] !== undefined && alwaysOnFromConfig(values) === null)
        return "metar.always_on_when_in_view must be quoted ICAO ids";
    if (values["metar.mark"] !== undefined && !markFromConfig(values))
        return "metar.mark must be chip, ink, or pin";
    return "";
}

function available(state, site, source) {
    if (!state || state.mode !== "live") return false;
    if (!site || site.lat === undefined || site.lon === undefined) return false;
    if (source && source.id && source.id !== "nexrad") return false;
    var sel = state.selection;
    if (sel && sel.sourceId && sel.sourceId !== "nexrad") return false;
    if (sel && sel.target && sel.target.kind && sel.target.kind !== "site") return false;
    return true;
}

function shouldQuery(state, enabled, site, source) {
    return !!(enabled && available(state, site, source));
}

function command(site, bbox, values) {
    var cmd = { type: "metar_query", lat: site.lat, lon: site.lon };
    var pick = pickFromConfig(values) || "nearest";
    var count = countFromConfig(values) || 16;
    var always = alwaysOnFromConfig(values) || [];
    if (count !== 16) cmd.limit = count;
    if (always.length) cmd.always_on = always;
    if (bbox && (pick === "priority" || always.length)) {
        cmd.south = bbox.south;
        cmd.west = bbox.west;
        cmd.north = bbox.north;
        cmd.east = bbox.east;
        if (pick === "priority") cmd.pick = "priority";
    }
    return cmd;
}
