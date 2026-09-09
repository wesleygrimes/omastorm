.pragma library
// The site picker's matching and ranking (DESIGN.md, picker as built) over
// hello.sites. Pure functions so a check can drive them without a window.

// Names for the table's postal abbreviations, so a row reads TULSA, OKLAHOMA
// and "oklahoma" finds it. The four overseas sites carry no state.
var STATES = {
    AL: "Alabama", AK: "Alaska", AZ: "Arizona", AR: "Arkansas", CA: "California", CO: "Colorado",
    CT: "Connecticut", DE: "Delaware", DC: "District of Columbia", FL: "Florida", GA: "Georgia",
    HI: "Hawaii", ID: "Idaho", IL: "Illinois", IN: "Indiana", IA: "Iowa", KS: "Kansas",
    KY: "Kentucky", LA: "Louisiana", ME: "Maine", MD: "Maryland", MA: "Massachusetts",
    MI: "Michigan", MN: "Minnesota", MS: "Mississippi", MO: "Missouri", MT: "Montana",
    NE: "Nebraska", NV: "Nevada", NH: "New Hampshire", NJ: "New Jersey", NM: "New Mexico",
    NY: "New York", NC: "North Carolina", ND: "North Dakota", OH: "Ohio", OK: "Oklahoma",
    OR: "Oregon", PA: "Pennsylvania", RI: "Rhode Island", SC: "South Carolina", SD: "South Dakota",
    TN: "Tennessee", TX: "Texas", UT: "Utah", VT: "Vermont", VA: "Virginia", WA: "Washington",
    WV: "West Virginia", WI: "Wisconsin", WY: "Wyoming", PR: "Puerto Rico", GU: "Guam"
};
function stateName(abbr) { return STATES[abbr] || abbr || ""; }
// The row's place column: the table's city, then the state spelled out.
function place(site) { var s = stateName(site.state); return site.name.toUpperCase() + (s ? ", " + s.toUpperCase() : ""); }

function distanceKm(lat1, lon1, lat2, lon2) {
    var r = Math.PI / 180, dp = (lat2 - lat1) * r, dl = (lon2 - lon1) * r;
    var h = Math.sin(dp / 2) ** 2 + Math.cos(lat1 * r) * Math.cos(lat2 * r) * Math.sin(dl / 2) ** 2;
    return 2 * 6371 * Math.asin(Math.sqrt(Math.max(0, Math.min(1, h))));
}
// Initial great-circle bearing from the first point to the second, degrees clockwise from north.
function bearingDeg(lat1, lon1, lat2, lon2) {
    var r = Math.PI / 180, dl = (lon2 - lon1) * r;
    var y = Math.sin(dl) * Math.cos(lat2 * r);
    var x = Math.cos(lat1 * r) * Math.sin(lat2 * r) - Math.sin(lat1 * r) * Math.cos(lat2 * r) * Math.cos(dl);
    return (Math.atan2(y, x) * 180 / Math.PI + 360) % 360;
}
function compass(deg) { return ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][Math.round(deg / 45) % 8]; }
function where(km, deg) { return km < 1 ? "< 1 km" : Math.round(km) + " km " + compass(deg); }

function range(from, count) { var out = []; for (var i = 0; i < count; i++) out.push(from + i); return out; }
// Index where `needle` starts a word of `text` (the start, or after a space or comma), or -1.
function wordStart(text, needle) {
    for (var i = text.indexOf(needle); i >= 0; i = text.indexOf(needle, i + 1))
        if (i === 0 || text[i - 1] === " " || text[i - 1] === ",") return i;
    return -1;
}
// The letters of `needle` in order through `text`, leftmost first; null if any is missing.
function subsequence(text, needle) {
    var hits = [], at = 0;
    for (var ch of needle) {
        if (ch === " ") continue;
        at = text.indexOf(ch, at);
        if (at < 0) return null;
        hits.push(at++);
    }
    return hits;
}

// How `query` matches one station: the tier it lands in and the matched
// letters in the ID and place columns, or null. Tiers, best first:
//   0  the query starts the ID, with or without its leading K/P/T
//   1  the query starts a word of the city
//   2  the query is the state's abbreviation or starts a word of its name
//   3  the query appears anywhere in the ID, city, or state
//   4  the query's letters appear in order across the ID and place
function match(site, query) {
    var q = query.trim().toLowerCase().replace(/\s+/g, " ");
    var id = site.id.toLowerCase(), name = site.name.toLowerCase(), placeText = place(site).toLowerCase();
    var state = stateName(site.state).toLowerCase(), abbr = (site.state || "").toLowerCase();
    var stateAt = name.length + 2, i;
    if (!q) return { tier: 5, idHits: [], placeHits: [] };
    if (id.indexOf(q) === 0) return { tier: 0, idHits: range(0, q.length), placeHits: [] };
    if (id.slice(1).indexOf(q) === 0) return { tier: 0, idHits: range(1, q.length), placeHits: [] };
    if ((i = wordStart(name, q)) >= 0) return { tier: 1, idHits: [], placeHits: range(i, q.length) };
    if (abbr && abbr === q) return { tier: 2, idHits: [], placeHits: range(stateAt, state.length) };
    if (state && (i = wordStart(state, q)) >= 0) return { tier: 2, idHits: [], placeHits: range(stateAt + i, q.length) };
    if ((i = id.indexOf(q)) >= 0) return { tier: 3, idHits: range(i, q.length), placeHits: [] };
    if ((i = placeText.indexOf(q)) >= 0) return { tier: 3, idHits: [], placeHits: range(i, q.length) };
    var hits = subsequence(id + " " + placeText, q);
    if (!hits) return null;
    return { tier: 4, idHits: hits.filter(h => h < id.length), placeHits: hits.filter(h => h > id.length).map(h => h - id.length - 1) };
}

// The stations matching `query`, best tier first and nearer the centre
// first within a tier, cut to `limit` rows: { rows, total }. Each row has
// the station, its distance and bearing from the centre, and the matched
// letter positions for the two columns. `pinned` names the station the
// session has locked, if any: an empty query lists it first, since that
// is the station the picker would otherwise have to be told about.
function rank(sites, query, lat, lon, limit, pinned) {
    var all = [], q = String(query || "").trim();
    for (var site of sites) {
        var m = match(site, query);
        if (!m) continue;
        var km = distanceKm(lat, lon, site.lat, site.lon);
        all.push({ site: site, tier: m.tier, km: km, where: where(km, bearingDeg(lat, lon, site.lat, site.lon)),
                   idHits: m.idHits, placeHits: m.placeHits, place: place(site), pinned: !!pinned && site.id === pinned });
    }
    all.sort((a, b) => (!q && b.pinned - a.pinned) || a.tier - b.tier || a.km - b.km || (a.site.id < b.site.id ? -1 : 1));
    return { rows: all.slice(0, limit), total: all.length };
}

function escape(text) { return String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;"); }
// `text` as styled text with the letters at `hits` in `color`.
function mark(text, hits, color) {
    var out = "", open = false;
    for (var i = 0; i < text.length; i++) {
        var hit = hits.indexOf(i) >= 0;
        if (hit && !open) { out += "<font color=\"" + color + "\">"; open = true; }
        if (!hit && open) { out += "</font>"; open = false; }
        out += escape(text[i]);
    }
    return open ? out + "</font>" : out;
}
