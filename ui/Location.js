.pragma library
// Map centre, remembered view, and radar lock (DESIGN.md, location and
// remembered state). Pure functions so a check can drive them without a
// window. Config.toml holds deliberate preferences; state.json holds the
// last camera and the UI radar lock.

var DEFAULT_SPAN = 210;
var MIN_SPAN = 25;

function validLat(value) {
    return typeof value === "number" && isFinite(value) && Math.abs(value) <= 90;
}
function validLon(value) {
    return typeof value === "number" && isFinite(value) && Math.abs(value) <= 180;
}
function validPair(lat, lon) { return validLat(lat) && validLon(lon); }

function clampSpan(value) {
    var n = typeof value === "number" && isFinite(value) ? value : DEFAULT_SPAN;
    return Math.max(MIN_SPAN, n);
}

function parseLatitude(text) {
    var t = String(text).trim();
    if (!t) return { empty: true };
    var n = Number(t);
    if (!isFinite(n)) return { error: "latitude is not a number" };
    if (Math.abs(n) > 90) return { error: "latitude must be in [-90, 90]" };
    return { value: n };
}
function parseLongitude(text) {
    var t = String(text).trim();
    if (!t) return { empty: true };
    var n = Number(t);
    if (!isFinite(n)) return { error: "longitude is not a number" };
    if (Math.abs(n) > 180) return { error: "longitude must be in [-180, 180]" };
    return { value: n };
}
function parseCoordFields(latText, lonText) {
    var lat = parseLatitude(latText), lon = parseLongitude(lonText);
    if (lat.empty && lon.empty) return { empty: true };
    if (lat.empty || lon.empty) return { error: "latitude and longitude must both be set" };
    if (lat.error) return { error: lat.error };
    if (lon.error) return { error: lon.error };
    return { lat: lat.value, lon: lon.value };
}

function distanceKm(lat1, lon1, lat2, lon2) {
    var r = Math.PI / 180, dp = (lat2 - lat1) * r, dl = (lon2 - lon1) * r;
    var h = Math.sin(dp / 2) ** 2 + Math.cos(lat1 * r) * Math.cos(lat2 * r) * Math.sin(dl / 2) ** 2;
    return 2 * 6371 * Math.asin(Math.sqrt(Math.max(0, Math.min(1, h))));
}

// Whether a point lies in the envelope the engine's gazetteer covers
// (engine/build.rs `in_envelope`, docs/protocol.md search_places): 5–75° N,
// west of 20° W or east of 120° E, the reach of the station table. Place
// search finds no towns outside it; coordinates still work anywhere.
function inGazetteer(lat, lon) {
    return validPair(lat, lon) && lat >= 5 && lat <= 75 && (lon <= -20 || lon >= 120);
}

function nearestSite(sites, lat, lon) {
    var best = null, bestKm = Infinity;
    if (!validPair(lat, lon) || !sites) return null;
    for (var s of sites) {
        var km = distanceKm(lat, lon, s.lat, s.lon);
        if (km < bestKm) { bestKm = km; best = s; }
    }
    return best;
}

function envView(text) {
    if (!text) return null;
    var parts = String(text).split(",");
    if (parts.length !== 3) return null;
    var lat = Number(parts[0]), lon = Number(parts[1]), span = Number(parts[2]);
    return validPair(lat, lon) && isFinite(span) && span > 0 ? { lat: lat, lon: lon, span: span } : null;
}

// Explicit centre from config.toml: both keys, both in range, or null.
function configCenter(values) {
    if (!values || values.center_lat === undefined && values.center_lon === undefined) return null;
    if (values.center_lat === undefined || values.center_lon === undefined) return null;
    return validPair(values.center_lat, values.center_lon)
        ? { lat: values.center_lat, lon: values.center_lon } : null;
}

function configLock(values) {
    if (!values || typeof values.locked_radar !== "string") return "";
    return values.locked_radar.trim().toUpperCase();
}

function configErrors(values) {
    var errors = [];
    if (!values) return errors;
    var hasLat = values.center_lat !== undefined, hasLon = values.center_lon !== undefined;
    if (hasLat !== hasLon)
        errors.push("center_lat and center_lon must both be set");
    else if (hasLat && !validPair(values.center_lat, values.center_lon))
        errors.push("center_lat/center_lon must be latitudes in [-90, 90] and longitudes in [-180, 180]");
    if (values.locked_radar !== undefined) {
        if (typeof values.locked_radar !== "string")
            errors.push("locked_radar must be a quoted station id");
        else if (!values.locked_radar.trim())
            errors.push("locked_radar must be a quoted station id");
    }
    if (values.home_site !== undefined)
        errors.push("home_site is unused; location is a place (center_lat/center_lon or the location picker)");
    if (values.follow !== undefined)
        errors.push("follow is unused; the map follows the nearest radar unless locked");
    return errors;
}

// Remembered view from state.json. Invalid fields are dropped, not fatal.
function parseState(raw) {
    var empty = { lat: undefined, lon: undefined, span: undefined, lock: "", name: "" };
    if (raw === undefined || raw === null || raw === "") return empty;
    try {
        var json = typeof raw === "string" ? JSON.parse(raw) : raw;
        if (!json || typeof json !== "object") return empty;
        var lat = json.lat, lon = json.lon, span = json.span;
        return {
            lat: validLat(lat) ? lat : undefined,
            lon: validLon(lon) ? lon : undefined,
            span: typeof span === "number" && isFinite(span) && span > 0 ? span : undefined,
            lock: typeof json.lock === "string" ? json.lock.trim().toUpperCase() : "",
            name: typeof json.name === "string" ? json.name : ""
        };
    } catch (e) { return empty; }
}

function stateObject(viewLat, viewLon, span, lock, name) {
    var o = {};
    if (validPair(viewLat, viewLon)) { o.lat = viewLat; o.lon = viewLon; }
    if (typeof span === "number" && isFinite(span) && span > 0) o.span = span;
    if (lock) o.lock = lock;
    if (name) o.name = name;
    return o;
}

// Map centre at launch: explicit config, remembered view, Omarchy weather,
// else nothing (the location picker). Captures may pass env as the first
// argument to outrank the rest for that process.
function resolvePlace(explicit, remembered, weather, env) {
    if (env && validPair(env.lat, env.lon))
        return { lat: env.lat, lon: env.lon, name: "", source: "view", span: env.span };
    if (explicit && validPair(explicit.lat, explicit.lon))
        return { lat: explicit.lat, lon: explicit.lon, name: "", source: "config", span: remembered && remembered.span };
    if (remembered && validPair(remembered.lat, remembered.lon))
        return { lat: remembered.lat, lon: remembered.lon, name: remembered.name || "", source: "state", span: remembered.span };
    if (weather && validPair(weather.lat, weather.lon))
        return { lat: weather.lat, lon: weather.lon, name: weather.name || "", source: "weather", span: remembered && remembered.span };
    return null;
}

// RESET target: configured centre, else weather, else none (keep the camera,
// restore the default span).
function resolveReset(explicit, weather) {
    if (explicit && validPair(explicit.lat, explicit.lon))
        return { lat: explicit.lat, lon: explicit.lon, name: "", source: "config" };
    if (weather && validPair(weather.lat, weather.lon))
        return { lat: weather.lat, lon: weather.lon, name: weather.name || "", source: "weather" };
    return null;
}
