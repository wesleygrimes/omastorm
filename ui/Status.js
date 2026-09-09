.pragma library
// The feed condition as every surface names it (DESIGN.md: show actual scan
// times, label archived data). The bar mark, the popover, and the window
// share this vocabulary so a stale, silent, or unreachable feed never reads
// LIVE anywhere, and so a cached frame is called cached. The engine judges
// the condition (docs/protocol.md, connection.status); nothing here claims
// more about the world than the engine did. Pure functions so a check can
// drive them without a window.

// The condition of an engine state, or "" while there is no engine:
// archived | ok | stale | unavailable | offline | loading.
function condition(state) {
    if (!state) return "";
    return state.source === "archived" ? "archived" : state.connection.status;
}
// Whether frames on screen are cached rather than arriving: the feed is
// reachable but quiet (stale, unavailable) or the engine could not reach it
// (offline). Loading has nothing cached to speak of.
function notUpdating(c) { return c === "stale" || c === "unavailable" || c === "offline"; }
// The header badge: LIVE only while the feed is ok, the condition's own
// word otherwise, ARCHIVED for an archived scan, nothing with no engine.
function badge(c) {
    switch (c) {
    case "archived": return "ARCHIVED";
    case "ok": return "LIVE";
    case "stale": return "STALE";
    case "loading": return "LOADING";
    case "unavailable": return "UNAVAILABLE";
    case "offline": return "OFFLINE";
    default: return "";
    }
}
// The newest complete frame of a timeline, or null.
function newestComplete(frames) {
    var done = (frames || []).filter(f => f.status === "complete");
    return done.length ? done[done.length - 1] : null;
}
// The age of the frame on screen: the engine gives the newest complete
// frame's age, ticking once a second; an older frame adds the distance
// between the two scan times, so no local clock is consulted. -1 when
// there is nothing to age.
function shownAge(state, scan) {
    if (!state || !scan || !scan.scanTime) return -1;
    var newest = newestComplete(state.timeline);
    if (!newest) return -1;
    return Math.max(0, state.connection.ageSeconds + Math.round((Date.parse(newest.scanTime) - Date.parse(scan.scanTime)) / 1000));
}
function ago(seconds) {
    var m = Math.floor(seconds / 60);
    if (m < 1) return "just now";
    if (m < 60) return m + " min ago";
    var h = Math.floor(m / 60);
    return h < 24 ? h + "h " + (m % 60) + "m ago" : Math.floor(h / 24) + "d " + (h % 24) + "h ago";
}
function lasting(seconds) {
    var m = Math.floor(seconds / 60), h = Math.floor(m / 60);
    return m < 60 ? m + " min" : h < 24 ? h + "h " + (m % 60) + "m" : Math.floor(h / 24) + "d " + (h % 24) + "h";
}
// Local clock reading of a wire time (UTC); `zone` appends the zone's
// abbreviation where the reading stands alone.
function clock(iso, zone) { return iso ? Qt.formatTime(new Date(iso), zone ? "HH:mm t" : "HH:mm") : ""; }

// The popover's one-line headline beside the site: the badge, then what the
// engine knows about the newest sweep. A feed that is not delivering says
// NOT UPDATING and the last sweep's time rather than a word about the world;
// which condition the engine judged is in `detail`.
function headline(state, scan) {
    if (!state) return "OFFLINE";
    var c = condition(state), newest = newestComplete(state.timeline), last = newest ? clock(newest.scanTime) : "";
    switch (c) {
    case "archived": return "ARCHIVED";
    case "loading": return "LOADING";
    case "ok": {
        var age = shownAge(state, scan);
        return age < 0 ? "LIVE" : "LIVE · " + ago(age);
    }
    case "stale": return last ? "STALE · LAST SWEEP " + last : "STALE";
    case "unavailable": return last ? "NOT UPDATING · LAST SWEEP " + last : "UNAVAILABLE · NO DATA";
    case "offline": return last ? "NOT UPDATING · LAST SWEEP " + last : "OFFLINE · NOTHING CACHED";
    default: return c.toUpperCase();
    }
}
// The popover's explanation under the map while the feed is not ok: the
// engine's judgement, that the frames are cached, and the one recovery the
// plugin has (stopping the engine; the bar starts it again). No in-card
// retry exists, so none is offered.
function detail(state) {
    if (!state || state.source === "archived") return "";
    var c = state.connection.status, site = state.site.id || "the station", age = state.connection.ageSeconds;
    var frames = (newestComplete(state.timeline) ? " · cached frames shown · " : " · nothing cached · ") + "restart: omastorm-engine stop";
    switch (c) {
    case "stale": return "No sweep for " + lasting(age) + " · showing the last one";
    case "unavailable": return "Unavailable: nothing new from " + site + " on the feed" + (age > 0 ? " for " + lasting(age) : "") + frames;
    case "offline": return "Offline: the engine could not reach the feed" + frames;
    default: return "";
    }
}
// What the bar mark says to a screen reader: the station and the honest
// condition, with the age while frames are arriving and the last sweep
// otherwise. No engine is named as such, apart from a silent station.
function summary(state, scan, siteName) {
    if (!state) return "Omastorm: radar engine not running";
    var c = condition(state), site = state.site.id ? state.site.id + (siteName ? " " + siteName : "") : "no station";
    var newest = newestComplete(state.timeline), last = newest ? clock(newest.scanTime, true) : "";
    switch (c) {
    case "archived": return "Omastorm: " + site + ", archived scan";
    case "loading": return "Omastorm: " + site + ", loading";
    case "ok": {
        var age = shownAge(state, scan);
        return "Omastorm: " + site + ", live" + (age < 0 ? "" : ", " + ago(age));
    }
    case "stale": return "Omastorm: " + site + ", stale" + (last ? ", last sweep " + last : "");
    case "unavailable": return "Omastorm: " + site + ", unavailable, not updating" + (last ? ", last sweep " + last : "");
    case "offline": return "Omastorm: " + site + ", offline, not updating" + (last ? ", last sweep " + last : "");
    default: return "Omastorm: " + site + ", " + c;
    }
}
// The plugin bootstrap's last stderr line, as the popover should show it.
// A daemon that has not answered its launcher's hello yet surfaces as the
// launcher's socket timeout (an OS WouldBlock or TimedOut), which is a
// daemon still starting, not a failure; the raw line stays in bootstrap.log.
function bootstrapNotice(line) {
    var text = String(line || "").trim();
    if (!text) return "";
    if (/WouldBlock|TimedOut|Resource temporarily unavailable|timed out/i.test(text)) return "Starting radar engine…";
    return text;
}
