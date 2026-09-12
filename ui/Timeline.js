.pragma library
// Breaks in the tick strip (DESIGN.md, time): an interval over thirty
// minutes and over three usual intervals (median of those under thirty).
var MIN_GAP_MS = 30 * 60 * 1000;
var CADENCE_FACTOR = 3;

function scanMs(frame) {
    var t = frame && frame.scanTime ? Date.parse(frame.scanTime) : NaN;
    return isNaN(t) ? null : t;
}

function cadence(frames) {
    var usual = [];
    for (var i = 1; i < frames.length; i++) {
        var a = scanMs(frames[i - 1]), b = scanMs(frames[i]);
        if (a === null || b === null) continue;
        var d = b - a;
        if (d > 0 && d <= MIN_GAP_MS) usual.push(d);
    }
    if (!usual.length) return 0;
    usual.sort((x, y) => x - y);
    return usual[Math.floor(usual.length / 2)];
}

function threshold(frames) {
    return Math.max(MIN_GAP_MS, CADENCE_FACTOR * cadence(frames));
}

function gaps(frames) {
    var out = [], limit = threshold(frames);
    for (var i = 1; i < frames.length; i++) {
        var a = scanMs(frames[i - 1]), b = scanMs(frames[i]);
        if (a === null || b === null || b - a <= limit) continue;
        out.push({ after: i - 1, from: frames[i - 1].scanTime, to: frames[i].scanTime, ms: b - a });
    }
    return out;
}

// Ticks, break markers, and pads up to `capacity` frames. Only ticks carry an id.
function slots(frames, capacity) {
    var out = [], breaks = gaps(frames), g = 0;
    for (var i = 0; i < frames.length; i++) {
        out.push({ id: frames[i].id, partial: frames[i].status === "partial", empty: false, gap: false });
        if (g < breaks.length && breaks[g].after === i) {
            out.push({ id: "", partial: false, empty: false, gap: true, from: breaks[g].from, to: breaks[g].to, ms: breaks[g].ms });
            g++;
        }
    }
    for (var p = frames.length; p < (capacity || 0); p++) out.push({ id: "", partial: false, empty: true, gap: false });
    return out;
}

// Breaks between the frame that was shown and the one that is, or null: a
// frame new to `previous` (the feed moved), a backward move while playing
// (the loop wrapped), or an end-to-end jump of more than one step.
function crossing(frames, fromId, toId, playing, previous) {
    if (!fromId || !toId || fromId === toId) return null;
    var i = frames.findIndex(f => f.id === fromId), j = frames.findIndex(f => f.id === toId);
    if (i < 0 || j < 0) return null;
    if (previous && previous.findIndex(f => f.id === toId) < 0) return null;
    if (playing && j < i) return null;
    var lo = Math.min(i, j), hi = Math.max(i, j);
    if (lo === 0 && hi === frames.length - 1 && hi - lo > 1) return null;
    var total = 0;
    gaps(frames).forEach(g => { if (g.after >= lo && g.after < hi) total += g.ms; });
    return total > 0 ? { ms: total, backward: j < i } : null;
}

function elapsed(ms) {
    var minutes = Math.round(ms / 60000);
    if (minutes < 60) return minutes + " min";
    var h = Math.floor(minutes / 60), m = minutes % 60;
    if (h < 3) return m ? h + "h " + m + "m" : h + "h";
    var hours = Math.round(minutes / 60);
    if (hours < 48) return hours + "h";
    var d = Math.floor(hours / 24);
    return hours % 24 ? d + "d " + (hours % 24) + "h" : d + "d";
}

// "Skipped 26h · no scans available", or shorter forms for narrow rows.
function notice(crossed, form) {
    if (!crossed) return "";
    var jump = (crossed.backward ? "Back " : "Skipped ") + elapsed(crossed.ms);
    return form === "bare" ? jump : form === "short" ? jump + " · no scans" : jump + " · no scans available";
}
