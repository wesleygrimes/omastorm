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

// Ground kilometres per Mercator unit at `lat`, same 6371 km sphere as
// RadarMap. A city jump keeps this scale on screen so London is not a
// zoom relative to KFCX.
function kmPerUnit(lat) {
    return 2 * Math.PI * 6371 * Math.cos(lat * Math.PI / 180);
}
function scaleSpan(fromLat, toLat, spanKm) {
    if (!validLat(fromLat) || !validLat(toLat)) return clampSpan(spanKm);
    var from = kmPerUnit(fromLat);
    if (!(from > 0)) return clampSpan(spanKm);
    return clampSpan(spanKm * kmPerUnit(toLat) / from);
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

function containsCoverage(coverage, lat, lon, dishLat, dishLon) {
    if (!coverage || !validPair(lat, lon)) return false;
    if (coverage.kind === "circle") {
        var clat = coverage.lat !== undefined && coverage.lat !== null ? coverage.lat : dishLat;
        var clon = coverage.lon !== undefined && coverage.lon !== null ? coverage.lon : dishLon;
        if (!validPair(clat, clon)) return false;
        return distanceKm(lat, lon, clat, clon) <= (coverage.radiusKm || 0);
    }
    if (coverage.kind === "box") {
        if (lat < coverage.south || lat > coverage.north) return false;
        if (coverage.west <= coverage.east)
            return lon >= coverage.west && lon <= coverage.east;
        return lon >= coverage.west || lon <= coverage.east;
    }
    if (coverage.kind === "polygon" && coverage.vertices && coverage.vertices.length >= 3) {
        var inside = false, verts = coverage.vertices, j = verts.length - 1;
        for (var i = 0; i < verts.length; i++) {
            var a = verts[i], b = verts[j];
            var intersect = ((a.lat > lat) !== (b.lat > lat))
                && (lon < (b.lon - a.lon) * (lat - a.lat) / (b.lat - a.lat) + a.lon);
            if (intersect) inside = !inside;
            j = i;
        }
        return inside;
    }
    return false;
}

function coverageCentroid(coverage) {
    if (!coverage) return null;
    if (coverage.kind === "box")
        return { lat: (coverage.north + coverage.south) / 2, lon: (coverage.west + coverage.east) / 2 };
    if (coverage.kind === "circle" && validPair(coverage.lat, coverage.lon))
        return { lat: coverage.lat, lon: coverage.lon };
    if (coverage.kind === "polygon" && coverage.vertices && coverage.vertices.length) {
        var lat = 0, lon = 0, n = coverage.vertices.length;
        for (var i = 0; i < n; i++) {
            lat += coverage.vertices[i].lat;
            lon += coverage.vertices[i].lon;
        }
        return { lat: lat / n, lon: lon / n };
    }
    return null;
}

function liveMosaics(sources) {
    var out = [];
    for (var s of sources || []) {
        if (s && s.kind === "mosaic" && s.id && s.id !== "fixture-mosaic") out.push(s);
    }
    return out;
}

function hitRange(from, count) {
    var out = [];
    for (var i = 0; i < count; i++) out.push(from + i);
    return out;
}

function matchSource(source, query) {
    var q = String(query || "").trim().toLowerCase().replace(/\s+/g, " ");
    var id = String(source.id || "").toLowerCase();
    var name = String(source.name || "").toLowerCase();
    if (!q) return { idHits: [], nameHits: [] };
    var i;
    if (id.indexOf(q) === 0) return { idHits: hitRange(0, q.length), nameHits: [] };
    if (name.indexOf(q) === 0) return { idHits: [], nameHits: hitRange(0, q.length) };
    if ((i = id.indexOf(q)) >= 0) return { idHits: hitRange(i, q.length), nameHits: [] };
    if ((i = name.indexOf(q)) >= 0) return { idHits: [], nameHits: hitRange(i, q.length) };
    return null;
}

function mosaicWhere(covering, lat, lon, clat, clon, metric) {
    if (covering) return "covers";
    var km = distanceKm(lat, lon, clat, clon);
    var r = Math.PI / 180, dl = (clon - lon) * r;
    var y = Math.sin(dl) * Math.cos(clat * r);
    var x = Math.cos(lat * r) * Math.sin(clat * r) - Math.sin(lat * r) * Math.cos(clat * r) * Math.cos(dl);
    var deg = (Math.atan2(y, x) * 180 / Math.PI + 360) % 360;
    var compass = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][Math.round(deg / 45) % 8];
    if (metric) return km < 1 ? "< 1 km" : Math.round(km) + " km " + compass;
    var mi = km / 1.609344;
    return mi < 1 ? "< 1 mi" : Math.round(mi) + " mi " + compass;
}

// Live GridFamily mosaics for the radar list. Empty browse keeps covering
// (or already selected) mosaics; type to search by id or name. The fixture
// mosaic is not a user-facing source.
function rankMosaics(sources, query, lat, lon, browse, selectedId, limit, metric) {
    var cap = typeof limit === "number" && limit > 0 ? limit : 4;
    var q = String(query || "").trim();
    if (!q && !browse) return [];
    var rows = [];
    for (var s of liveMosaics(sources)) {
        var c = coverageCentroid(s.coverage);
        if (!c) continue;
        var covering = containsCoverage(s.coverage, lat, lon);
        var selected = selectedId && s.id === selectedId;
        if (!q && !covering && !selected) continue;
        var m = matchSource(s, q);
        if (q && !m) continue;
        rows.push({
            kind: "mosaic",
            id: s.id,
            name: s.id,
            place: s.name || s.id,
            label: s.name || s.id,
            where: mosaicWhere(covering, lat, lon, c.lat, c.lon, metric),
            lat: c.lat,
            lon: c.lon,
            covering: covering,
            selected: selected,
            km: distanceKm(lat, lon, c.lat, c.lon),
            idHits: m ? m.idHits : [],
            placeHits: m ? m.nameHits : []
        });
    }
    rows.sort((a, b) => (b.covering - a.covering) || (b.selected - a.selected) || a.km - b.km || (a.id < b.id ? -1 : 1));
    return rows.slice(0, cap);
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

function polarLock(siteId) {
    var id = String(siteId || "").trim().toUpperCase();
    return id ? { sourceId: "nexrad", target: { kind: "site", siteId: id } } : null;
}
function mosaicLock(sourceId) {
    var id = String(sourceId || "").trim();
    return id ? { sourceId: id, target: { kind: "mosaic" } } : null;
}
// Remembered lock: object form, with one-release migration of a NEXRAD site string.
function parseLock(value) {
    if (typeof value === "string") return polarLock(value);
    if (!value || typeof value !== "object") return null;
    if (typeof value.sourceId !== "string" || !value.target || typeof value.target !== "object") return null;
    if (value.target.kind === "site") return polarLock(value.target.siteId);
    if (value.target.kind === "mosaic") return mosaicLock(value.sourceId);
    return null;
}
function lockEquals(a, b) {
    a = parseLock(a); b = parseLock(b);
    if (!a && !b) return true;
    if (!a || !b) return false;
    if (a.sourceId !== b.sourceId || a.target.kind !== b.target.kind) return false;
    if (a.target.kind === "site") return a.target.siteId === b.target.siteId;
    return true;
}
function lockSiteId(lock) {
    var l = parseLock(lock);
    return l && l.target.kind === "site" ? l.target.siteId : "";
}
function lockKey(lock) {
    var l = parseLock(lock);
    if (!l) return "";
    return l.target.kind === "site" ? l.target.siteId : l.sourceId + ":mosaic";
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
        errors.push("home_site is unused; location is a place (center_lat/center_lon or search)");
    if (values.follow !== undefined)
        errors.push("follow is unused; the map follows the nearest radar unless locked");
    if (values["metar.show"] !== undefined && typeof values["metar.show"] !== "boolean")
        errors.push("metar.show must be true or false");
    if (values["metar.pick"] !== undefined) {
        var pick = String(values["metar.pick"]).toLowerCase();
        if (pick !== "nearest" && pick !== "priority")
            errors.push("metar.pick must be nearest or priority");
    }
    if (values["metar.count"] !== undefined) {
        var n = values["metar.count"];
        if (typeof n !== "number" || !isFinite(n) || n !== Math.floor(n) || n < 1 || n > 16)
            errors.push("metar.count must be 1 through 16");
    }
    if (values["metar.mark"] !== undefined) {
        var mark = String(values["metar.mark"]).toLowerCase();
        if (mark !== "chip" && mark !== "ink" && mark !== "pin")
            errors.push("metar.mark must be chip, ink, or pin");
    }
    if (values["metar.always_on_when_in_view"] !== undefined) {
        if (typeof values["metar.always_on_when_in_view"] !== "string")
            errors.push("metar.always_on_when_in_view must be quoted ICAO ids");
        else {
            for (var part of values["metar.always_on_when_in_view"].trim().split(/\s+/)) {
                if (part && !/^[A-Za-z0-9]{3,4}$/.test(part)) {
                    errors.push("metar.always_on_when_in_view must be quoted ICAO ids");
                    break;
                }
            }
        }
    }
    return errors;
}

// Remembered view from state.json. Invalid fields are dropped, not fatal.
function parseState(raw) {
    var empty = { lat: undefined, lon: undefined, span: undefined, lock: null, name: "" };
    if (raw === undefined || raw === null || raw === "") return empty;
    try {
        var json = typeof raw === "string" ? JSON.parse(raw) : raw;
        if (!json || typeof json !== "object") return empty;
        var lat = json.lat, lon = json.lon, span = json.span;
        return {
            lat: validLat(lat) ? lat : undefined,
            lon: validLon(lon) ? lon : undefined,
            span: typeof span === "number" && isFinite(span) && span > 0 ? span : undefined,
            lock: parseLock(json.lock),
            name: typeof json.name === "string" ? json.name : ""
        };
    } catch (e) { return empty; }
}

// The remembered view, plus what is on screen right now (docs/configuration.md,
// remembered state and view export): `site` is the station shown, `scan` the
// frame's RFC 3339 time, `live` whether that frame is the live head. Those
// three are for other programs to read; launch never reads them back.
function stateObject(viewLat, viewLon, span, lock, name, site, scan, live) {
    var o = {};
    if (validPair(viewLat, viewLon)) { o.lat = viewLat; o.lon = viewLon; }
    if (typeof span === "number" && isFinite(span) && span > 0) o.span = span;
    var parsed = parseLock(lock);
    if (parsed) o.lock = parsed;
    if (name) o.name = name;
    if (site) o.site = site;
    if (scan) o.scan = scan;
    if (site) o.live = live === true;
    return o;
}

// Overlay the on-screen view (`site` / `scan` / `live` — see `stateObject`
// above) onto an existing snapshot: only those three fields change, every
// other key is preserved as-is, in the same order as `existing` whenever
// possible. The window owns the camera and the lock on disk; the bar owns
// only the on-screen view. A sweep that arrives after the user pans must
// not write the bar's stale `lat` / `lon` / `span` / `lock` / `name` over
// the window's new view, so sweep writes go through this merge and never
// through a whole-file replace. `existing` may be null on first launch.
function overlay(existing, site, scan, live) {
    var out = {};
    var e = (existing && typeof existing === "object") ? existing : {};
    for (var k in e) {
        if (k === "site" || k === "scan" || k === "live") continue;
        if (e.hasOwnProperty(k)) out[k] = e[k];
    }
    if (site) { out.site = site; out.live = live === true; }
    else { delete out.site; delete out.live; }
    if (scan) out.scan = scan;
    else delete out.scan;
    return out;
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

// wttr.in `?format=j2` nearest_area (DESIGN.md, approximate IP location).
// j2 stays under a small body size; Omarchy's weather `j1` is much larger.
function parseWttrCoord(value) {
    // Number(null) and Number("") are 0 — reject those before coercing.
    if (typeof value === "number") return value;
    if (typeof value === "string" && value.trim()) return Number(value.trim());
    return NaN;
}

function parseWttrHome(raw) {
    try {
        var json = typeof raw === "string" ? JSON.parse(raw) : raw;
        var areas = json && json.nearest_area;
        if (!areas || !areas.length) return null;
        var area = areas[0];
        var lat = parseWttrCoord(area.latitude), lon = parseWttrCoord(area.longitude);
        if (!validPair(lat, lon)) return null;
        var name = "";
        var labels = [].concat(area.areaName || [], area.region || []);
        for (var i = 0; i < labels.length; i++) {
            var value = labels[i] && labels[i].value;
            if (typeof value === "string" && value.trim()) {
                name = value.trim().replace(/[\x00-\x1f\x7f]/g, "").slice(0, 100);
                break;
            }
        }
        return { name: name, lat: lat, lon: lon };
    } catch (e) { return null; }
}

// `/` search (DESIGN.md): three or four letters is a site id or prefix.
function looksLikeSiteId(query) {
    return /^[A-Za-z]{3,4}$/.test(String(query).trim());
}

// Decimal degrees, latitude then longitude, comma or space (Google Maps).
// null means the text is not a coordinate pair; otherwise parseCoordFields.
function parseCoordQuery(text) {
    var t = String(text).trim();
    if (!t) return { empty: true };
    var m = t.match(/^([+-]?\d+(?:\.\d+)?)\s*[, ]\s*([+-]?\d+(?:\.\d+)?)$/);
    if (!m) return null;
    return parseCoordFields(m[1], m[2]);
}

function coordRow(lat, lon) {
    return {
        kind: "place",
        name: lat.toFixed(4) + ", " + lon.toFixed(4),
        where: "coordinates",
        label: lat.toFixed(4) + ", " + lon.toFixed(4),
        lat: lat,
        lon: lon
    };
}

// Mix site rows, mosaic source rows, place rows, and an optional coordinate
// row. Empty query with browseSites lists covering mosaics then the nearest
// dishes; otherwise type to search. Places first unless the query looks like
// a site id or a mosaic source.
function mergeSearch(siteRows, placeRows, coord, query, browseSites, limit, mosaicRows) {
    var cap = typeof limit === "number" && limit > 0 ? limit : 4;
    var sites = siteRows || [], places = placeRows || [], mosaics = mosaicRows || [];
    var q = String(query || "").trim();
    var extra = coord && coord.lat !== undefined ? [coordRow(coord.lat, coord.lon)] : [];
    if (coord && coord.error) return extra.slice(0, cap);
    if (!q) return browseSites ? mosaics.concat(sites).slice(0, cap) : extra.slice(0, cap);
    var sourceFirst = false;
    var needle = q.toLowerCase();
    for (var i = 0; i < mosaics.length; i++) {
        var id = String(mosaics[i].id || "").toLowerCase();
        var name = String(mosaics[i].place || mosaics[i].label || "").toLowerCase();
        if (id.indexOf(needle) === 0 || name.indexOf(needle) === 0) { sourceFirst = true; break; }
    }
    var combined = looksLikeSiteId(q) ? sites.concat(mosaics, extra, places)
        : sourceFirst ? mosaics.concat(extra, places, sites)
        : extra.concat(places, mosaics, sites);
    return combined.slice(0, cap);
}
