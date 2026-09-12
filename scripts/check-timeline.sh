#!/usr/bin/env bash
# Break detection and crossing (ui/Timeline.js) on synthetic timelines
# shaped like the catalogs: the KAKQ 26 h hole, ordinary cadence jitter, a
# VCP change, missed volumes, the loop wrapping. Needs no daemon.
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir="$PWD/target/check-timeline"
mkdir -p "$check_dir"
cp ui/Timeline.js "$check_dir/Timeline.js"
cat > "$check_dir/shell.qml" <<'QML'
import QtQuick
import Quickshell
import "Timeline.js" as Strip
ShellRoot {
    function check(ok, message) { if (!ok) throw new Error(message); }
    function eq(actual, expected, message) { check(JSON.stringify(actual) === JSON.stringify(expected), message + ": " + JSON.stringify(actual) + " != " + JSON.stringify(expected)); }
    // Frames from `start` at `minutes` per step; `offsets` (minutes) override the rhythm.
    function series(start, offsets) {
        var t0 = Date.parse(start);
        return offsets.map((m, i) => ({ id: "f" + i, scanTime: new Date(t0 + m * 60000).toISOString(), status: "complete" }));
    }
    function cumulative(intervals) { var out = [0]; intervals.forEach(d => out.push(out[out.length - 1] + d)); return out; }
    Component.onCompleted: {
        var H = 60, D = 24 * H;
        // KAKQ, 2026-09-10/11: 4.5-7.1 minute cadence with a VCP change and a 1549 minute hole.
        var kakq = [4.5, 4.5, 4.5, 5.2, 5.2, 5.2, 5.2, 5.3, 5.4, 5.4, 5.4, 5.4, 5.4, 5.5, 5.5, 5.5, 5.5, 5.5, 5.5, 5.5, 1549.1, 7.0, 7.0, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1, 7.1];
        var frames = series("2026-09-10T18:36:16Z", cumulative(kakq));
        eq(frames.length, 40, "KAKQ frame count");
        var gaps = Strip.gaps(frames);
        eq(gaps.length, 1, "KAKQ has one break");
        eq(gaps[0].after, 20, "KAKQ break follows frame 20");
        eq(Math.round(gaps[0].ms / 60000), 1549, "KAKQ break length");
        eq(Strip.elapsed(gaps[0].ms), "26h", "KAKQ break reads as 26h");
        // Ordinary variation is not a break: jitter, a VCP change, one or two missed volumes.
        eq(Strip.gaps(series("2026-09-11T18:00:00Z", cumulative([3.3, 3.4, 3.6, 4.3, 4.5, 4.7, 5.5, 8.7, 8.8]))).length, 0, "Cadence jitter");
        eq(Strip.gaps(series("2026-09-11T18:00:00Z", cumulative([4.4, 4.4, 4.4, 6.7, 6.8, 7.1, 7.1]))).length, 0, "A VCP change");
        eq(Strip.gaps(series("2026-09-11T18:00:00Z", cumulative([8.7, 8.7, 8.7, 17.5, 8.7, 8.7]))).length, 0, "One missed volume at a slow cadence");
        eq(Strip.gaps(series("2026-09-11T18:00:00Z", cumulative([3.5, 3.5, 17.5, 17.9, 3.5]))).length, 0, "Missed volumes under thirty minutes");
        eq(Strip.gaps(series("2026-09-11T18:00:00Z", cumulative([12, 12, 12, 35, 12, 12]))).length, 0, "Two missed volumes at a 12 minute cadence stay under three usual intervals");
        eq(Strip.gaps(series("2026-09-11T18:00:00Z", cumulative([3.5, 3.5, 35.2, 3.5, 85.1, 3.5]))).length, 2, "KTLX: 35 and 85 minute holes are breaks");
        // Two frames a day apart still show the hole; a placeholder without a time never does.
        eq(Strip.gaps(series("2026-09-10T20:56:00Z", [0, 26 * H])).length, 1, "A two-frame history with a hole");
        var loading = series("2026-09-11T18:00:00Z", cumulative([4.5, 4.5])).concat([{ id: "KAKQ-loading", scanTime: "", status: "partial" }]);
        eq(Strip.gaps(loading).length, 0, "A placeholder has no time and no break");
        eq(Strip.gaps([]).length, 0, "Empty timeline");
        eq(Strip.gaps([frames[0]]).length, 0, "Single frame");
        // The strip: a break marker is a position, not a frame or a seek target; pads fill to the capacity in frames.
        var wide = Strip.slots(frames, 60), narrow = Strip.slots(frames, 0);
        eq(wide.filter(s => s.id).length, 40, "Ticks equal frames");
        eq(wide.filter(s => s.gap).length, 1, "One break marker");
        eq(wide.filter(s => s.empty).length, 20, "Pads fill to sixty frames");
        eq(wide.length, 61, "Sixty positions plus the break");
        eq(wide[21].gap && wide[20].id === "f20" && wide[22].id === "f21", true, "The marker sits between the frames it separates");
        eq(wide.filter(s => s.gap || s.empty).every(s => s.id === ""), true, "Breaks and pads carry no id");
        eq(narrow.length, 41, "Compact: ticks plus the break, no pads");
        // Crossing: one step over the hole either way, a scrub across it, and the moves that are not a crossing.
        var t = frames;
        eq(Strip.crossing(t, "f20", "f21", false, t), { ms: gaps[0].ms, backward: false }, "Step forward across the hole");
        eq(Strip.crossing(t, "f21", "f20", false, t), { ms: gaps[0].ms, backward: true }, "Step back across the hole");
        eq(Strip.crossing(t, "f5", "f30", false, t), { ms: gaps[0].ms, backward: false }, "A scrub across the hole");
        eq(Strip.crossing(t, "f5", "f6", false, t), null, "A step with no hole");
        eq(Strip.crossing(t, "f5", "f5", false, t), null, "No move");
        eq(Strip.crossing(t, "f39", "f0", true, t), null, "The loop wrapping");
        eq(Strip.crossing(t, "f39", "f0", false, t), null, "The oldest key");
        eq(Strip.crossing(t, "f0", "f39", false, t), null, "The newest key");
        eq(Strip.crossing(t, "f20", "f21", true, t), { ms: gaps[0].ms, backward: false }, "Playback crossing forward");
        eq(Strip.crossing(t, "", "f21", false, t), null, "No previous frame");
        eq(Strip.crossing(t, "f20", "elsewhere", false, t), null, "An id outside the timeline");
        eq(Strip.crossing(t, "f20", "f21", false, t.slice(0, 21)), null, "A frame new to the timeline: the feed moved");
        var two = series("2026-09-10T20:56:00Z", [0, 26 * H]);
        eq(Strip.crossing(two, "f0", "f1", false, two), { ms: 26 * H * 60000, backward: false }, "Two frames: the step is the crossing");
        eq(Strip.crossing(two, "f1", "f0", true, two), null, "Two frames: the loop wrapping");
        // Words.
        eq(Strip.elapsed(35.2 * 60000), "35 min", "Minutes");
        eq(Strip.elapsed(85.1 * 60000), "1h 25m", "Hours and minutes under three hours");
        eq(Strip.elapsed(2 * H * 60000), "2h", "Whole hours under three");
        eq(Strip.elapsed(113.2 * 60000), "1h 53m", "KRLX");
        eq(Strip.elapsed(896.5 * 60000), "15h", "KJAX");
        eq(Strip.elapsed(1272.9 * 60000), "21h", "KTLX");
        eq(Strip.elapsed(50 * H * 60000), "2d 2h", "Days and hours");
        eq(Strip.elapsed(3 * D * 60000), "3d", "Whole days");
        eq(Strip.notice({ ms: gaps[0].ms, backward: false }, "full"), "Skipped 26h · no scans available", "Forward notice");
        eq(Strip.notice({ ms: gaps[0].ms, backward: true }, "full"), "Back 26h · no scans available", "Backward notice");
        eq(Strip.notice({ ms: gaps[0].ms, backward: false }, "short"), "Skipped 26h · no scans", "Short notice");
        eq(Strip.notice({ ms: gaps[0].ms, backward: true }, "bare"), "Back 26h", "Bare notice");
        eq(Strip.notice(null, "full"), "", "No crossing, no notice");
        console.log("TIMELINE_PASSED");
        Qt.quit();
    }
    Timer { interval: 5000; running: true; onTriggered: Qt.quit() }
}
QML
QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic quickshell -p "$check_dir/shell.qml" > "$check_dir/result.log" 2>&1 || true
cat "$check_dir/result.log"
rg -q TIMELINE_PASSED "$check_dir/result.log"
