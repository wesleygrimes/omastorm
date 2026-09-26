import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import "Sites.js" as Sites
import "Keys.js" as KeyMap
import "Location.js" as Location
import "Metar.js" as Metar

Item {
    id: app
    // A standalone launcher owns its process; a plugin never does.
    property var session: null
    property var shell: null
    property var manifest: null
    readonly property var store: PluginSession
    property bool opened: session === null
    function open(payload) {
        opened = true;
        if (session) session.windowOpen = true;
        applyView();
        if (store.pendingLocationPicker) Qt.callLater(() => locationPicker.show(""));
        else maybeOfferLocation();
    }
    function close() {
        if (!opened) return;
        opened = false;
        store.persist();
        if (session) session.windowOpen = false;
    }
    function dismiss() {
        if (!session) Qt.quit();
        else if (shell) shell.hide("com.omastorm.radar");
        else close();
    }
    readonly property var state: engine.state
    readonly property var scan: state ? state.frame : null
    // Every station, product, and source string on screen comes from the engine.
    readonly property string siteId: engine.selectedSiteId
    readonly property string siteName: engine.site ? engine.site.name.toUpperCase() : (engine.source && !engine.site ? engine.source.name.toUpperCase() : "")
    readonly property string sourceBadge: state ? state.mode.toUpperCase() : ""
    // The timeline (DESIGN.md): the station's frames oldest
    // first with the sweep in progress last; the engine owns the position.
    readonly property var frames: state ? state.timeline : []
    readonly property int frameIndex: scan ? frames.findIndex(f => f.id === scan.id) : -1
    readonly property bool playing: state ? state.playing : false
    readonly property var newestComplete: { var done = frames.filter(f => f.status === "complete"); return done.length ? done[done.length - 1] : null; }
    // The connection condition while live (DESIGN.md):
    // LIVE / ARCHIVED is the badge; a light beside it carries health.
    // Age under the product line is how stale the frame on screen is.
    // Prose is reserved for rejections, config mistakes, and notices.
    readonly property string condition: state && state.mode === "live" ? state.connection.status : ""
    readonly property bool alert: condition !== "" && condition !== "ok" && condition !== "idle"
    readonly property bool scanning: !!scan && scan.status === "partial" && !!scan.scanTime
    readonly property color conditionColor: condition === "stale" ? theme.yellow
        : condition === "loading" ? theme.accent
        : condition === "unavailable" || condition === "offline" ? theme.red : theme.foreground
    // Light beside LIVE: accent when healthy (or archived), yellow stale,
    // red when the feed is down; pulses while loading or a sweep is painting.
    readonly property color statusLightColor: {
        if (!state) return theme.foreground;
        if (state.mode === "archived") return theme.accent;
        if (condition === "stale") return theme.yellow;
        if (condition === "unavailable" || condition === "offline") return theme.red;
        return theme.accent;
    }
    readonly property bool statusLightPulse: condition === "loading" || (condition === "ok" && scanning)
    // Age of the frame on screen: newest complete age, plus how far the
    // playhead sits behind that sweep. No local clock.
    readonly property int shownAge: !state || !scan || !scan.scanTime || !newestComplete ? -1
        : Math.max(0, state.connection.ageSeconds + Math.round((Date.parse(newestComplete.scanTime) - Date.parse(scan.scanTime)) / 1000))
    readonly property string ageText: condition && shownAge >= 0 ? ago(shownAge) : ""
    // First sweep or COMP is still in flight: the map has tiles and no radar.
    readonly property bool awaitingRadar: !!state && !map.radarReady
        && (condition === "loading" || !!scan && !!scan.scanTime)
    property bool loadingNotice: false
    onAwaitingRadarChanged: if (!awaitingRadar) loadingNotice = false
    Timer {
        interval: 180
        running: app.awaitingRadar
        onTriggered: app.loadingNotice = true
    }
    function ago(seconds) {
        var m = Math.floor(seconds / 60);
        if (m < 1) return "Now";
        if (m < 60) return m + " min ago";
        var h = Math.floor(m / 60);
        return h < 24 ? h + "h " + (m % 60) + "m ago" : Math.floor(h / 24) + "d " + (h % 24) + "h ago";
    }
    // Stamp above the tick strip: locale picks date order and 12/24h only.
    readonly property bool stamp12h: {
        var fmt = Qt.locale().timeFormat(Locale.ShortFormat);
        return fmt.indexOf("A") >= 0 || fmt.indexOf("a") >= 0;
    }
    readonly property string stampDateOrder: {
        var fmt = Qt.locale().dateFormat(Locale.ShortFormat);
        var y = fmt.indexOf("y"), m = fmt.indexOf("M"), d = fmt.indexOf("d");
        if (y >= 0 && (m < 0 || y < m) && (d < 0 || y < d)) return "ymd";
        if (d >= 0 && m >= 0 && d < m) return "dmy";
        return "mdy";
    }
    function pad2(n) { return (n < 10 ? "0" : "") + n; }
    function stamp(iso) {
        if (!iso) return "";
        var d = new Date(iso), y = d.getFullYear(), mo = d.getMonth() + 1, day = d.getDate();
        var dateStr = stampDateOrder === "ymd" ? y + "-" + pad2(mo) + "-" + pad2(day)
            : stampDateOrder === "dmy" ? pad2(day) + "/" + pad2(mo) + "/" + String(y).slice(2)
            : pad2(mo) + "/" + pad2(day) + "/" + String(y).slice(2);
        var timeStr = stamp12h ? Qt.formatTime(d, "h:mm AP") : Qt.formatTime(d, "HH:mm");
        return dateStr + " · " + timeStr + " " + Qt.formatTime(d, "t");
    }
    // One tick per timeline entry, spread across the strip, at every width:
    // the loop is two hours of scans (DESIGN.md), so a fixed 60 positions
    // would leave most of the strip as pads.
    readonly property var slots: {
        var result = [];
        for (var j = 0; j < frames.length; j++)
            result.push({id: frames[j].id, partial: frames[j].status === "partial"});
        return result;
    }
    readonly property int currentSlot: scan ? slots.findIndex(s => s.id === scan.id) : -1
    function togglePlay() { if (frames.length > 1) engine.send({type: playing ? "pause" : "play"}); }
    function step(delta) { if (frames.length > 1) engine.send({type: "step", delta: delta}); }
    function jump(toNewest) { if (frames.length > 1) engine.send({type: "seek", id: frames[toNewest ? frames.length - 1 : 0].id}); }
    readonly property int bands: scan ? scan.palette.length : 0
    function legendLabel(index) {
        var bounds = scan.bounds;
        return index === 0 ? "<" + bounds[1] : index === bands - 1 ? bounds[index] + "+" : String(bounds[index]);
    }
    // Where the weak-return floor cuts the legend strip, as a fraction of its
    // width: the bands are equal columns, so the floor interpolates inside
    // the band it falls in. 0 when the floor is off, under the scale, or not
    // in force because the frame carries no scale (an older engine).
    readonly property bool floorActive: !!scan && weakFloor !== null && scan.scale > 0
    readonly property real floorFraction: {
        if (!floorActive || weakFloor <= scan.bounds[0]) return 0;
        for (var i = 0; i < bands; i++)
            if (weakFloor < scan.bounds[i + 1]) return (i + (weakFloor - scan.bounds[i]) / (scan.bounds[i + 1] - scan.bounds[i])) / bands;
        return 1;
    }
    // Which legend numbers fit: the last always shows; each earlier one shows
    // only if it clears the previous shown number and the last one. This keeps
    // spacing even at any width and band count instead of hiding by parity.
    TextMetrics { id: legendMetrics; font.family: app.theme.font; font.pixelSize: 10; text: "0" }
    readonly property var legendShown: {
        var n = bands, shown = [];
        if (!n) return shown;
        var column = legendRow.width / n, gap = 8, last = (n - 1) * column, cursor = 0;
        for (var i = 0; i < n - 1; i++) {
            var x = i * column, right = x + legendLabel(i).length * legendMetrics.advanceWidth + gap;
            shown[i] = x >= cursor && right <= last;
            if (shown[i]) cursor = right;
        }
        shown[n - 1] = true;
        return shown;
    }
    Engine { id: engine }
    // Deliberate preferences and remembered view (DESIGN.md, location).
    // PluginSession owns config.toml, state.json, and the camera; this
    // window applies the view to its map and sends map-local tile requests.
    readonly property var config: store.config
    property bool applyingView: false
    Timer { id: applyingViewClear; interval: 250; onTriggered: app.applyingView = false }
    function applyView() {
        if (!store.hasView) return;
        applyingView = true;
        applyingViewClear.restart();
        map.holdSpan = true;
        map.lookAt(store.centerLat, store.centerLon);
        map.span = store.span;
        if (engine.state)
            engine.send({type: "view_center", lat: store.centerLat, lon: store.centerLon});
        Qt.callLater(() => { map.holdSpan = false; });
    }
    property bool viewApplied: false
    onStateChanged: {
        if (!state) viewApplied = false;
        else {
            store.initialize();
            if (!viewApplied) { applyView(); viewApplied = true; }
            maybeOfferLocation();
        }
    }
    function maybeOfferLocation() {
        if (!opened || !store.initialized || !store.needsLocation || store.locating || locationPicker.open) return;
        Qt.callLater(() => {
            if (app.opened && app.store.initialized && app.store.needsLocation && !app.store.locating && !locationPicker.open) locationPicker.show("", true);
        });
    }
    Connections {
        target: store
        function onViewChanged() {
            if (locationPicker.open && locationPicker.onboarding && !app.store.needsLocation) locationPicker.close();
            if (app.opened) app.applyView();
            app.maybeOfferLocation();
            if (app.store.hasView && app.store.locationError && !app.store.needsLocation) {
                app.flashMap(app.store.locationError);
                app.store.locationError = "";
            }
        }
        function onLocationPickerRequested() { if (app.opened) locationPicker.show(""); }
    }
    Connections {
        target: config
        function onReadyChanged() { app.applyView(); }
        function onKeysChanged() { app.applySettings(); }
        function onTreatmentChanged() { app.applySettings(); }
        function onWeakFloorChanged() { app.applySettings(); }
        function onValuesChanged() { app.applySettings(); if (app.store.metarEnabled) app.requestMetars(); }
    }
    // The keyboard map (DESIGN.md, keyboard map as built): Keys.js lays the
    // `[keys]` table over the defaults, asking Qt whether each sequence
    // parses; a bad value or a key bound twice keeps the default and is
    // named in the status slot, like a rejection, until the file is fixed.
    // The treatment and weak_floor settings are judged the same way.
    // OMASTORM_STYLE and OMASTORM_WEAK, set by the capture scripts, outrank
    // the file.
    property var bindings: ({})
    property var configErrors: []
    readonly property string configError: !configErrors.length ? "" : configErrors[0].toUpperCase() + (configErrors.length > 1 ? " (+" + (configErrors.length - 1) + " MORE)" : "")
    Shortcut { id: probe; enabled: false }
    function canon(sequence) { probe.sequence = sequence; return probe.portableText; }
    function applySettings() {
        var errors = Location.configErrors(config.values);
        var wanted = KeyMap.treatment(config.treatment, errors), floor = KeyMap.weakFloor(config.weakFloor, errors);
        var resolved = KeyMap.resolve(config.keys, canon);
        bindings = resolved.bindings;
        configErrors = errors.concat(resolved.errors);
        if (!session && !Quickshell.env("OMASTORM_STYLE") && wanted) treatment = wanted;
        if (!session && KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) === undefined) weakFloor = floor;
    }
    Component.onCompleted: applySettings()
    readonly property bool overlayOpen: locationPicker.open || sheet.open
    property var metars: []
    property var selectedMetar: null
    function toggleMetar() {
        if (!Metar.available(state, engine.site, engine.source)) return;
        store.metarEnabled = !store.metarEnabled;
    }
    function requestMetars() {
        if (!Metar.shouldQuery(state, store.metarEnabled, engine.site, engine.source)) {
            if (!store.metarEnabled) selectedMetar = null;
            else { metars = []; selectedMetar = null; }
            return;
        }
        engine.send(Metar.command(engine.site, map.viewBbox(), config.values));
    }
    onSiteIdChanged: { metars = []; selectedMetar = null; requestMetars(); }
    Connections {
        target: store
        function onMetarEnabledChanged() {
            if (!app.store.metarEnabled) { app.selectedMetar = null; return; }
            if (app.metars.length) return;
            app.requestMetars();
        }
    }
    Connections {
        target: engine
        function onMetarsReady(message) { app.metars = message.results || []; }
        function onStateChanged() {
            if (!Metar.available(app.state, engine.site, engine.source)) {
                app.metars = [];
                app.selectedMetar = null;
            }
        }
    }
    Timer {
        interval: 600000
        repeat: true
        running: store.metarEnabled && Metar.available(state, engine.site, engine.source)
        onTriggered: app.requestMetars()
    }
    Timer {
        interval: 60000
        repeat: true
        running: store.metarEnabled && Metar.available(state, engine.site, engine.source)
        onTriggered: { if (new Date().getUTCMinutes() <= 1) app.requestMetars(); }
    }
    function run(action) {
        switch (action) {
        case "search": treatmentMenu.close(); locationPicker.show(""); break;
        case "nearest": nearest(); break;
        case "lock": toggleLock(); break;
        case "follow": toggleFollow(); break;
        case "locate": locateMe(); break;
        case "pan_left": map.pan(-1, 0); break;
        case "pan_right": map.pan(1, 0); break;
        case "pan_up": map.pan(0, -1); break;
        case "pan_down": map.pan(0, 1); break;
        case "zoom_in": map.zoom(Math.min(map.span, map.maxSpan) / 1.25); break;
        case "zoom_out": map.zoom(Math.min(map.span, map.maxSpan) * 1.25); break;
        case "reset": resetView(); break;
        case "previous_frame": step(-1); break;
        case "next_frame": step(1); break;
        case "play": togglePlay(); break;
        case "oldest": jump(false); break;
        case "newest": jump(true); break;
        case "pixels": case "glyphs": case "stipple": treatment = action.toUpperCase(); treatmentMenu.close(); break;
        case "weak": weakFloor = weakFloor === null ? configuredFloor : null; break;
        case "aviation": toggleMetar(); break;
        case "help": treatmentMenu.close(); if (sheet.open) sheet.close(); else sheet.show(); break;
        case "close": if (app.selectedMetar) app.selectedMetar = null; else dismiss(); break;
        }
    }
    // Drives the keyboard map from outside for checks and captures:
    // quickshell ipc --pid <pid> call keys run pan_left
    IpcHandler {
        target: "keys"
        function run(action: string): void { app.run(action); }
        function bindings(): string { return JSON.stringify(app.bindings); }
        function errors(): string { return JSON.stringify(app.configErrors); }
        function menu(open: bool): void { if (open) treatmentMenu.show(); else treatmentMenu.close(); }
        function field(name: string): string { var value = JSON.parse(status())[name]; return value === undefined ? "" : String(value); }
        function status(): string {
            return JSON.stringify({sheet: sheet.open, menu: treatmentMenu.opened, treatment: app.treatment, weakFloor: app.weakFloor === null ? "off" : app.weakFloor, error: app.configError,
                                   span: Math.round(map.span * 10) / 10, lat: Math.round(map.centerLat * 1000) / 1000, lon: Math.round(map.centerLon * 1000) / 1000,
                                   locationSource: app.store.locationSource, needsLocation: app.store.needsLocation, locating: app.store.locating,
                                   site: app.siteId, source: engine.source ? engine.source.id : "", locked: app.locked, lockSource: app.store.lockSource, outsideCoverage: app.outsideCoverage,
                                   gpsEnabled: app.gpsEnabled, gpsFollowing: app.gpsFollowing, gpsPaused: app.gpsPaused, gpsNoFix: app.gpsNoFix});
        }
    }
    // Site navigation (DESIGN.md, location): the lock pins the radar against
    // hand-offs; `n` releases it and selects the nearest radar without moving
    // the camera. The site picker locks and centres on that station.
    readonly property bool locked: state && state.navigation ? state.navigation.locked : false
    readonly property bool following: state && state.navigation ? state.navigation.follow && !state.navigation.locked : false
    // The follow chip on the map (DESIGN.md, gpsd follow as built) is the
    // crosshair on the map's top-left, hidden when gpsd is off. Its three
    // shapes read the camera-control state of GPS follow: filled accent
    // while a fix is steering the camera, outlined accent after a pan has
    // paused it, outlined dimmed with `NO FIX` while the receiver has
    // nothing to report. Click to pause and resume; the padlock on the
    // site row is a separate verb that only pins the radar.
    readonly property bool gpsEnabled: config.gpsd
    readonly property bool gpsHasFix: !!config.fix
    readonly property bool gpsPaused: store.followPaused
    readonly property bool gpsFollowing: gpsEnabled && gpsHasFix && !gpsPaused
    readonly property bool gpsNoFix: gpsEnabled && !gpsHasFix
    readonly property var resetTarget: Location.resolveReset(Location.configCenter(config.values), config.location)
    readonly property bool outsideCoverage: {
        if (!locked) return false;
        if (engine.site && engine.site.coverage)
            return !Location.containsCoverage(engine.site.coverage, map.centerLat, map.centerLon, engine.site.lat, engine.site.lon);
        if (engine.source && engine.source.coverage)
            return !Location.containsCoverage(engine.source.coverage, map.centerLat, map.centerLon);
        return false;
    }
    function toggleLock() {
        if (!state || !state.selection) return;
        store.setLock(locked ? null : state.selection, !locked);
    }
    // The follow chip's single verb: pause ↔ resume. While the receiver is
    // silent (NO FIX) a click has nothing to hold or steer with yet.
    function toggleFollow() {
        if (!gpsEnabled || gpsNoFix) return;
        store.setFollowPaused(!gpsPaused);
    }
    readonly property string placeLabel: {
        var t = app.resetTarget;
        if (t && Location.distanceKm(map.centerLat, map.centerLon, t.lat, t.lon) < 2) return (t.name || "OMARCHY'S LOCATION").toUpperCase();
        if (app.store.placeName && Location.distanceKm(map.centerLat, map.centerLon, app.store.centerLat, app.store.centerLon) < 2) {
            var name = app.store.placeName.toUpperCase();
            return app.store.locationSource === "ip" ? "IP NEAR " + name : name;
        }
        if (app.store.locationSource === "ip" && Location.distanceKm(map.centerLat, map.centerLon, app.store.centerLat, app.store.centerLon) < 2)
            return "IP NEAR YOU";
        var lat = map.centerLat, lon = map.centerLon;
        return Math.abs(lat).toFixed(2) + "° " + (lat < 0 ? "S" : "N") + "  " + Math.abs(lon).toFixed(2) + "° " + (lon < 0 ? "W" : "E");
    }
    property string mapNotice: ""
    Timer { id: mapNoticeTimer; interval: Quickshell.env("OMASTORM_CAPTURE") ? 20000 : 3000; onTriggered: app.mapNotice = "" }
    function flashMap(text) {
        mapNotice = text || "";
        if (mapNotice) mapNoticeTimer.restart();
        else mapNoticeTimer.stop();
    }
    function resetView() {
        store.resetView();
    }
    function nearest() {
        var s = map.nearest();
        if (!state || !s) return;
        store.followNearest(s.id);
    }
    function locateMe() {
        if (!store.hasView || store.needsLocation) return;
        if (!state || state.mode !== "live") {
            flashMap("Archived views never locate");
            return;
        }
        store.requestApproximateLocation("locate");
    }
    function choose(s) {
        if (!state || !s) return;
        store.chooseRadar(s.id, Number(s.lat), Number(s.lon), s.name || s.id);
    }
    function acceptSearch(row) {
        if (!row) return;
        if (row.kind === "site") {
            app.choose(row.site);
            return;
        }
        if (row.kind === "mosaic") {
            app.store.chooseMosaic(row.id, Number(row.lat), Number(row.lon), row.label || row.place || row.id);
            return;
        }
        app.store.setPlace(row.lat, row.lon, row.name || row.label || "");
    }
    // Drives the picker from outside for checks and captures:
    // quickshell ipc --pid <pid> call picker open tul
    IpcHandler {
        target: "picker"
        function open(query: string): void { locationPicker.show(query, query ? undefined : "sites"); }
        function accept(): void { locationPicker.accept(); }
        function close(): void { locationPicker.close(); }
        function move(delta: int): void { locationPicker.move(delta); }
        function matches(): string {
            return JSON.stringify(locationPicker.rows.map(r => r.kind === "site" ? r.site.id : (r.kind === "mosaic" ? r.id : (r.label || r.name))));
        }
        function status(): string {
            return JSON.stringify({open: locationPicker.open, query: locationPicker.query, selected: locationPicker.selected, total: locationPicker.siteRanked.total, focused: locationPicker.fieldFocused});
        }
    }
    IpcHandler {
        target: "location"
        function open(query: string): void { locationPicker.show(query); }
        function prompt(): void { locationPicker.show("", true); }
        function accept(): void { locationPicker.accept(); }
        function close(): void { locationPicker.close(); }
        function move(delta: int): void { locationPicker.move(delta); }
        function go(lat: string, lon: string, name: string): void { locationPicker.go(Number(lat), Number(lon), name); }
        function matches(): string {
            return JSON.stringify(locationPicker.rows.map(r => r.kind === "site" ? r.site.id : (r.kind === "mosaic" ? r.id : (r.where ? (r.name + ", " + r.where) : (r.label || r.name)))));
        }
        function status(): string {
            return JSON.stringify({open: locationPicker.open, query: locationPicker.query, selected: locationPicker.selected, focused: locationPicker.fieldFocused, count: locationPicker.rows.length, error: locationPicker.coordError});
        }
    }
    readonly property var theme: session ? session.theme.snapshot : themeInputs.snapshot
    Theme { id: themeInputs; registerIpc: !app.session }
    // Glyphs is the default; config.toml's `treatment` and the keys change it.
    property string treatment: session ? session.treatment : Quickshell.env("OMASTORM_STYLE") || "GLYPHS"
    onTreatmentChanged: if (session) session.treatment = treatment
    // The weak-return floor (DESIGN.md, weak-return floor): measured returns
    // under this many dBZ draw nothing, null draws them all. `w` toggles
    // between off and the configured floor (the default when config.toml
    // has none or says false); the popover inherits it through the session.
    property var weakFloor: session ? session.weakFloor : KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) !== undefined ? KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) : KeyMap.DEFAULT_FLOOR
    onWeakFloorChanged: if (session) session.weakFloor = weakFloor
    readonly property real configuredFloor: { var floor = KeyMap.weakFloor(config.weakFloor, []); return floor === null ? KeyMap.DEFAULT_FLOOR : floor; }
    Connections {
        target: app.session
        function onTreatmentChanged() { app.treatment = app.session.treatment; }
        function onWeakFloorChanged() { app.weakFloor = app.session.weakFloor; }
        function onLocationPickerRequested() { if (app.opened) locationPicker.show(""); }
    }
    // Quickshell keeps the process alive after its last window closes, which
    // left 400-600 MB orphans behind every close. Quit with the window; the
    // engine daemon is separate and stays up.
    Connections { target: Quickshell; function onLastWindowClosed() { if (!app.session) Qt.quit(); } }
    FloatingWindow {
        id: win
        title: "Omastorm"
        visible: app.opened
        onVisibleChanged: if (!visible && app.opened) app.dismiss()
        implicitWidth: Number(Quickshell.env("OMASTORM_WIDTH")) || 960
        implicitHeight: Number(Quickshell.env("OMASTORM_HEIGHT")) || 680
        minimumSize: Qt.size(360 * Math.max(1, app.theme.baseSize/12), 360 * Math.max(1, app.theme.baseSize/12))
        color: app.theme.background
        property bool compact: width < 560

        component LabelText: Text {
            color: app.theme.foreground
            font.family: app.theme.font
            font.pixelSize: app.theme.baseSize
            elide: Text.ElideRight
        }
        component Control: Button {
            id: button
            property bool selected: false
            implicitHeight: 30
            implicitWidth: Math.max(30, contentItem.implicitWidth + 18)
            padding: 6
            contentItem: LabelText {
                text: button.text
                color: button.selected ? app.theme.background : app.theme.foreground
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }
            background: Rectangle {
                color: button.selected ? app.theme.accent : button.hovered || button.activeFocus ? Qt.alpha(app.theme.accent, .18) : "transparent"
                border.width: 1
                border.color: button.selected || button.activeFocus ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
            }
        }
        // Chrome icons as Nerd Font glyphs (same Material Design Icons set
        // Omarchy's shell uses for media / panels). The theme's monospace
        // alias resolves to JetBrainsMono Nerd Font on Omarchy.
        component Glyph: Item {
            id: glyphRoot
            property string glyph: "play"
            property color ink: app.theme.foreground
            property real fade: 1
            implicitWidth: 16
            implicitHeight: 16
            // Codepoints match Omarchy media (play/pause/prev/next) and common
            // MDI lock / keyboard / crosshair / search / chevron glyphs.
            readonly property var icons: ({
                "play": "󰐊", "pause": "󰏤", "back": "󰒮", "fwd": "󰒭",
                "first": "󰒫", "last": "󰒬", "lock": "󰌾", "unlock": "󰌿",
                "keys": "󰌌", "locate": "󰍎", "search": "󰍉", "chevron": "󰅀",
                "radar": "󰐷", "follow": "󰍏"
            })
            Text {
                anchors.centerIn: parent
                text: glyphRoot.icons[glyphRoot.glyph] || ""
                color: glyphRoot.ink
                opacity: glyphRoot.fade
                font.family: app.theme.font
                font.pixelSize: 14
                renderType: Text.NativeRendering
            }
        }
        // A 30 px control showing one glyph. Never takes keyboard focus: the
        // keys stay global.
        component GlyphButton: Button {
            id: transport
            property string glyph: "play"
            property bool selected: false
            implicitHeight: 30
            implicitWidth: 30
            padding: 0
            focusPolicy: Qt.NoFocus
            contentItem: Glyph {
                glyph: transport.glyph
                ink: transport.selected ? app.theme.background : app.theme.foreground
                fade: transport.enabled ? 1 : .35
            }
            background: Rectangle {
                color: transport.selected ? app.theme.accent : transport.hovered ? Qt.alpha(app.theme.accent, .18) : "transparent"
                border.width: 1
                border.color: transport.selected ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
            }
        }
        // MOCK: one chip per idea, in two parts. The name opens the picker;
        // the glyph to its right is the toggle (crosshair = follow place,
        // padlock = pin radar), filled with the accent while on.
        component Chip: RowLayout {
            id: chip
            property string glyph: "locate"
            property string label: ""
            property string tag: ""
            property bool on: false
            property bool tagAccent: false
            property bool enabled: true
            signal toggled()
            signal opened()
            spacing: 0
            Button {
                id: name
                implicitHeight: 30
                implicitWidth: contentItem.implicitWidth + (win.compact ? 14 : 22)
                padding: 0
                focusPolicy: Qt.NoFocus
                enabled: chip.enabled
                visible: !win.compact
                onClicked: chip.opened()
                contentItem: RowLayout {
                    spacing: 7
                    Item { Layout.fillWidth: true }
                    LabelText { text: chip.label; opacity: chip.enabled ? 1 : .35 }
                    LabelText {
                        text: chip.tag; visible: chip.tag !== ""
                        color: chip.tagAccent ? app.theme.accent : app.theme.foreground
                        opacity: chip.tagAccent ? .9 : .55
                        font.pixelSize: 10; font.letterSpacing: 1
                    }
                    Glyph { glyph: "chevron"; implicitWidth: 12; fade: .6 }
                    Item { Layout.fillWidth: true }
                }
                background: Rectangle {
                    color: name.hovered ? Qt.alpha(app.theme.accent, .18) : "transparent"
                    border.width: 1
                    border.color: Qt.alpha(app.theme.foreground, .22)
                }
            }
            GlyphButton {
                glyph: chip.glyph; selected: chip.on; enabled: chip.enabled
                Layout.leftMargin: -1
                onClicked: chip.toggled()
            }
        }
        Rectangle {
          id: surface
          anchors.fill: parent
          color: app.theme.background
          // One Shortcut per action with the bindings in force (Keys.js has
          // the defaults). While the picker or the sheet is open its card
          // has the keyboard and every window shortcut stands down; while
          // the treatment menu is open the treatment keys still choose and
          // the menu itself takes Escape.
          Instantiator {
            model: KeyMap.ACTIONS.map(a => a.id)
            delegate: Shortcut {
                sequences: app.bindings[modelData] || []
                enabled: app.opened && !app.overlayOpen && (!treatmentMenu.opened || modelData === "pixels" || modelData === "glyphs" || modelData === "stipple")
                onActivated: app.run(modelData)
            }
          }
          ColumnLayout {
            id: layout
            anchors.fill: parent
            anchors.margins: win.compact ? 12 : 20
            spacing: 10
            // Chrome names (use these when tweaking):
            //   brand row     — mark, OMASTORM, status light, LIVE/ARCHIVED
            //   site row      — station title, radar lock (yellow when outside coverage)
            //   product stack — product line + meta line (right of site row)
            //   product line  — REFLECTIVITY / tilt + NOAA NEXRAD
            //   meta line     — age, right-aligned under the product line
            //   map stage     — radar map frame
            //   locate chip   — map marker, top-left of the map
            //   help chip     — ? keys on the map
            //   scale bar     — ground distance, bottom-left of the map
            //   legend        — dBZ scale under the map
            //   transport     — playback buttons
            //   tick strip    — frame ticks
            //   strip stamp   — date/time/zone above the tick strip
            //   frame index   — N / available frames above the strip
            RowLayout {
                id: brandRow
                Layout.fillWidth: true
                RadarMark { ink: app.theme.accent; size: 20; Layout.rightMargin: 8 }
                LabelText { text: "OMASTORM"; font.bold: true; font.letterSpacing: 2.5; font.pixelSize: app.theme.baseSize + 5 }
                Item { Layout.fillWidth: true }
                // LIVE / ARCHIVED as text; the light carries feed health.
                RowLayout {
                    spacing: 8
                    Rectangle {
                        id: statusLight
                        width: 8; height: 8; radius: 4
                        Layout.alignment: Qt.AlignVCenter
                        visible: !!app.state
                        color: app.statusLightColor
                        SequentialAnimation on opacity {
                            running: app.statusLightPulse
                            loops: Animation.Infinite
                            NumberAnimation { from: 1; to: .25; duration: 700; easing.type: Easing.InOutSine }
                            NumberAnimation { from: .25; to: 1; duration: 700; easing.type: Easing.InOutSine }
                            onRunningChanged: if (!running) statusLight.opacity = 1
                        }
                    }
                    LabelText { text: app.sourceBadge; color: app.theme.accent; font.letterSpacing: 1.5 }
                }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(app.theme.foreground, .25) }
            RowLayout {
                id: siteRow
                Layout.fillWidth: true
                // Fixed height: polar↔mosaic product/age changes must not
                // resize the map (a source hand-off is not a layout event).
                Layout.preferredHeight: 30
                Layout.minimumHeight: 30
                Layout.maximumHeight: 30
                // MOCK: the station title is the radar control. Click it to
                // pick a station; the padlock beside it pins that radar (not
                // the map — the crosshair on the map is place-follow).
                // No border or hover fill on the title — it reads as text.
                Button {
                    id: siteTitle
                    implicitHeight: 30
                    padding: 0
                    focusPolicy: Qt.NoFocus
                    enabled: !!app.state
                    onClicked: locationPicker.show("", "sites")
                    Layout.alignment: Qt.AlignTop
                    contentItem: RowLayout {
                        spacing: 8
                        LabelText { text: app.siteId || (engine.source ? engine.source.id : "—"); font.pixelSize: app.theme.baseSize + 7; font.bold: true }
                        LabelText { text: app.siteName; visible: !win.compact; opacity: .65 }
                        Glyph { glyph: "chevron"; implicitWidth: 12; fade: .5 }
                    }
                    background: Item {}
                }
                // Pins the radar on screen; chip outline so it reads as a toggle.
                // Yellow (same stale cue as the status light) when the camera
                // sits outside that radar's rings — no banner.
                Rectangle {
                    id: lockButton
                    implicitWidth: 30; implicitHeight: 30
                    radius: 2
                    Layout.alignment: Qt.AlignTop
                    opacity: !!app.state ? 1 : .35
                    readonly property color lockColor: !app.locked ? app.theme.foreground
                        : app.outsideCoverage ? app.theme.yellow : app.theme.accent
                    color: lockArea.containsMouse && !!app.state ? Qt.alpha(lockColor, .18) : "transparent"
                    border.width: 1
                    border.color: app.locked ? lockColor : Qt.alpha(app.theme.foreground, .22)
                    Glyph {
                        anchors.centerIn: parent
                        glyph: app.locked ? "lock" : "unlock"
                        ink: app.locked ? lockButton.lockColor : app.theme.foreground
                        fade: app.locked ? 1 : .45
                    }
                    MouseArea {
                        id: lockArea
                        anchors.fill: parent
                        hoverEnabled: true
                        enabled: !!app.state
                        onClicked: app.toggleLock()
                    }
                }
                Item { Layout.fillWidth: true }
                // product stack: compact product line; age right-aligned under it.
                ColumnLayout {
                    id: productStack
                    spacing: 2
                    Layout.fillWidth: true
                    Layout.minimumWidth: 0
                    Layout.maximumWidth: implicitWidth
                    Layout.alignment: Qt.AlignTop | Qt.AlignRight
                    RowLayout {
                        id: productLine
                        spacing: 8
                        Layout.fillWidth: true
                        LabelText {
                            text: !app.scan ? "" : app.scan.productName.toUpperCase()
                        }
                        LabelText {
                            id: tiltLabel
                            // Reserve the tilt while a polar frame is loading.
                            // Metadata appearing must not push the product name.
                            visible: !!app.scan && app.scan.kind !== "mosaic"
                            text: app.scan && app.scan.scanTime && app.scan.kind !== "mosaic"
                                ? "/ " + app.scan.elevationDeg.toFixed(1) + "°" : "/ —°"
                            opacity: app.scan && app.scan.scanTime ? 1 : 0
                            Layout.minimumWidth: tiltMetrics.width
                            Layout.preferredWidth: tiltMetrics.width
                            Layout.maximumWidth: tiltMetrics.width
                            TextMetrics { id: tiltMetrics; font: tiltLabel.font; text: "/ 00.0°" }
                        }
                        LabelText {
                            text: !app.scan ? "" : (engine.source ? engine.source.attribution : (app.scan.kind === "mosaic" ? "" : "NOAA NEXRAD"))
                            font.letterSpacing: 1; opacity: .55
                            Layout.fillWidth: true
                            Layout.minimumWidth: 0
                        }
                    }
                    LabelText {
                        id: metaLine
                        Layout.alignment: Qt.AlignRight
                        Layout.minimumHeight: 12
                        text: app.ageText
                        color: app.alert && app.condition !== "loading" ? app.conditionColor : app.theme.foreground
                        opacity: app.ageText === "" ? 0 : (app.alert && app.condition !== "loading" ? 1 : .75)
                    }
                }
            }
            Rectangle {
                id: mapFrame
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.minimumHeight: 100
                Layout.topMargin: -4
                color: app.theme.background
                border.color: Qt.alpha(app.theme.foreground, .17)
                clip: true
                RadarMap {
                    id: map
                    anchors.fill: parent
                    scan: app.scan
                    texture: engine.texture
                    azimuthLut: engine.azimuthLut
                    siteId: app.siteId
                    sourceId: engine.source ? engine.source.id : ""
                    sites: engine.sites
                    coverage: engine.site && engine.site.coverage ? engine.site.coverage
                        : (engine.source && engine.source.coverage ? engine.source.coverage : null)
                    tileRoot: "file://" + engine.runtime
                    theme: app.theme
                    treatment: app.treatment
                    weakFloor: app.weakFloor
                    radarOpacity: app.condition === "unavailable" ? .6 : 1
                    labelSize: win.compact ? 10 : 12
                    locked: app.locked
                    metarMode: app.store.metarEnabled && Metar.available(app.state, engine.site, engine.source) && app.metars.length > 0
                    metars: app.metars
                    metarMark: Metar.markFromConfig(app.config.values) || "chip"
                    interactive: !app.store.needsLocation && !locationPicker.open
                    onNavigated: (lat, lon, spanKm) => app.store.userNavigated(lat, lon, spanKm)
                    // A settled pan hands the centre to the engine, which switches
                    // station while following and unlocked; the camera stays.
                    onViewSettled: (lat, lon) => {
                        if (!app.opened || app.store.needsLocation) return;
                        // A pick or restore already set the store; a settle
                        // still queued from the previous camera must not
                        // write that centre back (radar jumps, map stays).
                        if (app.applyingView
                            && (Math.abs(lat - app.store.centerLat) > 0.05
                                || Math.abs(lon - app.store.centerLon) > 0.05))
                            return;
                        engine.send({type: "view_center", lat: lat, lon: lon});
                        app.store.rememberView(lat, lon, map.span);
                        app.requestMetars();
                    }
                    onResetRequested: app.resetView()
                    Component.onCompleted: app.applyView()
                    // The map asks for tiles when its camera settles and the
                    // engine answers this window alone, tile by tile.
                    onTilesNeeded: (z, x0, y0, x1, y1) => engine.send({type: "tiles_needed", z: z, x0: x0, y0: y0, x1: x1, y1: y1})
                    onMetarPicked: report => app.selectedMetar = report
                }
                Connections { target: engine; function onTileReady(tile) { map.tileReady(tile); } }
                Rectangle {
                    id: metarCard
                    visible: !!app.selectedMetar
                    // Bottom-right, 80% of the map, sitting above the OSM
                    // credit so the scale bar and attribution stay clear.
                    anchors.right: parent.right
                    anchors.bottom: parent.bottom
                    anchors.rightMargin: 12
                    anchors.bottomMargin: 36
                    width: Math.round(parent.width * 0.8)
                    height: metarColumn.implicitHeight + 16
                    color: Qt.alpha(app.theme.background, .95)
                    border.width: 1
                    border.color: Qt.alpha(app.theme.foreground, .22)
                    MouseArea { anchors.fill: parent }
                    Column {
                        id: metarColumn
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.top: parent.top
                        anchors.margins: 8
                        spacing: 4
                        Row {
                            spacing: 8
                            Rectangle {
                                width: 10; height: 10; radius: 5
                                anchors.verticalCenter: parent.verticalCenter
                                color: Metar.color(app.selectedMetar ? app.selectedMetar.category : "") || app.theme.foreground
                            }
                            LabelText {
                                text: (app.selectedMetar ? app.selectedMetar.name || app.selectedMetar.id : "")
                                    + (Metar.label(app.selectedMetar ? app.selectedMetar.category : "") ? "  " + Metar.label(app.selectedMetar.category) : "")
                                font.bold: true
                            }
                            Item { width: 8; height: 1 }
                            LabelText {
                                text: "NOAA/NWS AWC"
                                opacity: .55
                                font.pixelSize: 10
                            }
                        }
                        LabelText {
                            width: metarCard.width - 16
                            wrapMode: Text.Wrap
                            font.family: "monospace"
                            text: app.selectedMetar ? app.selectedMetar.raw : ""
                        }
                    }
                }
                // Locate (DESIGN.md): map marker, top-left; north sits beside it.
                // The follow chip takes the corner ahead of it while gpsd is on
                // (DESIGN.md, gpsd follow as built).
                Row {
                    id: mapTopLeft
                    anchors.top: parent.top; anchors.left: parent.left; anchors.margins: 10
                    spacing: 8
                    visible: !!app.state
                    Rectangle {
                        id: followChip
                        width: 22; height: 22
                        readonly property color ink: app.gpsFollowing ? app.theme.background
                            : app.gpsNoFix ? Qt.alpha(app.theme.foreground, .55)
                            : app.theme.accent
                        readonly property color borderInk: app.gpsFollowing || app.gpsPaused ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
                        color: app.gpsFollowing ? app.theme.accent : followArea.containsMouse ? Qt.alpha(app.theme.accent, .18) : Qt.alpha(app.theme.background, .9)
                        border.width: 1; border.color: borderInk
                        visible: app.gpsEnabled
                        opacity: app.gpsFollowing || app.gpsPaused || followArea.containsMouse ? 1 : .7
                        Glyph { anchors.centerIn: parent; glyph: "follow"; ink: followChip.ink }
                        MouseArea { id: followArea; anchors.fill: parent; hoverEnabled: true; onClicked: app.toggleFollow() }
                    }
                    // NO FIX stands beside the chip while the receiver is silent,
                    // so a silent receiver reads as exactly that and not as a
                    // paused chip or an outage.
                    LabelText {
                        text: "NO FIX"
                        color: Qt.alpha(app.theme.foreground, .7); font.pixelSize: 10; font.letterSpacing: 1
                        anchors.verticalCenter: parent.verticalCenter
                        visible: app.gpsNoFix
                    }
                    Rectangle {
                        id: locateChip
                        width: 26; height: 22
                        readonly property bool pending: app.store.locating && app.store.locateKind === "locate"
                        readonly property bool canLocate: !!app.state && app.store.hasView && app.state.mode === "live"
                        color: pending ? app.theme.accent : locateArea.containsMouse && canLocate ? Qt.alpha(app.theme.accent, .18) : Qt.alpha(app.theme.background, .9)
                        border.width: 1; border.color: pending ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
                        visible: app.store.hasView
                        opacity: canLocate ? (locateArea.containsMouse || pending ? 1 : .7) : .35
                        Glyph { anchors.centerIn: parent; glyph: "locate"; ink: locateChip.pending ? app.theme.background : app.theme.foreground }
                        MouseArea {
                            id: locateArea
                            anchors.fill: parent
                            hoverEnabled: true
                            enabled: locateChip.canLocate
                            onClicked: app.run("locate")
                        }
                    }
                    LabelText {
                        text: "N ↑"; opacity: .75
                        anchors.verticalCenter: parent.verticalCenter
                    }
                }
                // The `?` chip in the map's top-right corner (DESIGN.md, window
                // chrome) opens the keys sheet, as does the key itself.
                Rectangle {
                    id: helpChip
                    anchors.top: parent.top; anchors.right: parent.right; anchors.margins: 10
                    width: helpRow.implicitWidth + 12; height: 22
                    color: Qt.alpha(app.theme.background, .9)
                    opacity: helpArea.containsMouse ? 1 : .7
                    visible: !!app.state
                    RowLayout {
                        id: helpRow
                        anchors.centerIn: parent
                        spacing: 5
                        Glyph { glyph: "keys" }
                        LabelText { text: "?"; font.pixelSize: 10 }
                    }
                    MouseArea { id: helpArea; anchors.fill: parent; hoverEnabled: true; onClicked: app.run("help") }
                }
                // Scale bar (DESIGN.md): fixed-length tick; the label is the
                // round distance that length currently spans. Locale picks
                // kilometres or miles (same measurementSystem as the OS).
                Rectangle {
                    id: scaleBar
                    anchors.bottom: parent.bottom; anchors.left: parent.left; anchors.margins: 10
                    visible: !!app.scan && map.pixelsPerKm > 0
                    readonly property bool metric: Qt.locale().measurementSystem === Locale.MetricSystem
                    readonly property real kmPerMile: 1.609344
                    readonly property var steps: metric
                        ? [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000]
                        : [0.5, 1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000]
                    readonly property real barPx: 72
                    property real nice: metric ? 25 : 10
                    property bool wasMetric: metric
                    width: barPx + 16
                    height: 28
                    color: Qt.alpha(app.theme.background, .9)
                    function nearest(raw) {
                        var best = steps[0], err = Math.abs(steps[0] - raw);
                        for (var i = 1; i < steps.length; i++) {
                            var e = Math.abs(steps[i] - raw);
                            if (e < err) { err = e; best = steps[i]; }
                        }
                        return best;
                    }
                    function stabilize() {
                        if (map.pixelsPerKm <= 0) return;
                        if (wasMetric !== metric) {
                            wasMetric = metric;
                            nice = metric ? 25 : 10;
                        }
                        var exactKm = barPx / map.pixelsPerKm;
                        var exact = metric ? exactKm : exactKm / kmPerMile;
                        // Stay on the current step while exact is closer to it
                        // than to its neighbours (wide band around each step).
                        if (Math.abs(exact - nice) <= nice * 0.35) return;
                        nice = nearest(exact);
                    }
                    Connections {
                        target: map
                        function onPixelsPerKmChanged() { scaleBar.stabilize() }
                        function onSpanChanged() { scaleBar.stabilize() }
                    }
                    onMetricChanged: stabilize()
                    Component.onCompleted: stabilize()
                    onVisibleChanged: if (visible) stabilize()
                    Item {
                        anchors.horizontalCenter: parent.horizontalCenter
                        anchors.verticalCenter: parent.verticalCenter
                        width: scaleBar.barPx
                        height: 16
                        LabelText {
                            anchors.horizontalCenter: parent.horizontalCenter
                            anchors.top: parent.top
                            text: {
                                var u = scaleBar.metric ? "km" : "mi";
                                var n = scaleBar.nice;
                                if (scaleBar.metric && n >= 1000) return (n / 1000) + "k " + u;
                                if (!scaleBar.metric && n < 1) return n + " " + u;
                                return n + " " + u;
                            }
                            font.pixelSize: 10
                            opacity: .75
                        }
                        Rectangle {
                            anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom
                            height: 1
                            color: Qt.alpha(app.theme.foreground, .65)
                        }
                        Rectangle {
                            anchors.left: parent.left; anchors.bottom: parent.bottom
                            width: 1; height: 5
                            color: Qt.alpha(app.theme.foreground, .65)
                        }
                        Rectangle {
                            anchors.right: parent.right; anchors.bottom: parent.bottom
                            width: 1; height: 5
                            color: Qt.alpha(app.theme.foreground, .65)
                        }
                    }
                }
                // OSM ODbL safe harbour: short credit in a map corner. Full
                // catalogue (NOAA, Natural Earth, GeoNames, …) stays in README.
                LabelText {
                    anchors.bottom: parent.bottom; anchors.right: parent.right; anchors.margins: 12
                    text: "© OpenStreetMap"
                    visible: !!app.scan
                    font.pixelSize: 10; opacity: .55
                }
                Rectangle {
                    anchors.centerIn: parent
                    visible: app.loadingNotice && app.awaitingRadar && !(map.error || engine.error)
                    width: loadText.implicitWidth + 20
                    height: 28
                    color: Qt.alpha(app.theme.background, .88)
                    border.width: 1
                    border.color: Qt.alpha(app.theme.foreground, .22)
                    z: 2
                    LabelText {
                        id: loadText
                        anchors.centerIn: parent
                        text: "Loading..."
                        color: app.theme.accent
                    }
                }
                LabelText { anchors.centerIn: parent; width: parent.width-24; wrapMode: Text.Wrap; horizontalAlignment: Text.AlignHCenter; text: map.error || engine.error; visible: text.length > 0 }
                // Rejections and update copy sit on the map. A layout row
                // here used to grow the chrome and shift the stage whenever
                // OPERA loaded, a place was accepted, or the feed rejected.
                LabelText {
                    anchors.top: parent.top
                    anchors.horizontalCenter: parent.horizontalCenter
                    anchors.topMargin: 10
                    width: Math.min(implicitWidth, parent.width - 80)
                    wrapMode: Text.Wrap
                    horizontalAlignment: Text.AlignHCenter
                    text: engine.rejection || app.configError || store.persistError || store.updateNotice
                    color: app.theme.accent
                    visible: text !== ""
                    z: 2
                }
                Rectangle {
                    id: mapToast
                    visible: app.mapNotice !== ""
                    anchors.horizontalCenter: parent.horizontalCenter
                    anchors.bottom: parent.bottom
                    anchors.bottomMargin: 44
                    width: toastText.implicitWidth + 20
                    height: 26
                    color: Qt.alpha(app.theme.background, .92)
                    border.width: 1
                    border.color: Qt.alpha(app.theme.foreground, .22)
                    LabelText {
                        id: toastText
                        anchors.centerIn: parent
                        text: app.mapNotice
                        color: app.theme.accent
                    }
                }
            }
            // legend — colors for the map above. Height is pinned so a
            // polar↔OPERA scan replace (Repeater rebuild, or no frame over
            // the ocean) cannot grow or shrink the map stage.
            ColumnLayout {
                id: legend
                Layout.fillWidth: true
                Layout.preferredHeight: 22
                Layout.minimumHeight: 22
                Layout.maximumHeight: 22
                spacing: 4
                opacity: app.state && app.bands > 0 ? 1 : 0
                Item {
                    Layout.fillWidth: true
                    implicitHeight: 22
                    height: 22
                    RowLayout {
                        id: legendRow
                        anchors.fill: parent; spacing: 0
                        Repeater {
                            model: app.scan ? app.scan.palette : []
                            ColumnLayout {
                                required property string modelData
                                required property int index
                                Layout.fillWidth: true; Layout.preferredWidth: 1; spacing: 4
                                Rectangle { Layout.fillWidth: true; height: 6; color: modelData }
                                // Number at the swatch's left edge; the unit sits at the far
                                // right of the last column so the strip spans the full width.
                                RowLayout {
                                    id: labelRow
                                    Layout.fillWidth: true; spacing: 0
                                    LabelText {
                                        id: number
                                        text: app.legendLabel(index)
                                        opacity: app.legendShown[index] ? 1 : 0
                                        font.pixelSize: 10
                                    }
                                    Item { Layout.fillWidth: true }
                                    LabelText {
                                        text: app.scan ? app.scan.units : ""
                                        font.pixelSize: 10
                                        visible: index===app.bands-1 && labelRow.width >= number.implicitWidth + implicitWidth + 8
                                    }
                                }
                            }
                        }
                    }
                    // The hidden part of the scale (DESIGN.md, weak-return floor):
                    // the swatches under the floor sink into the background with a
                    // tick at the floor, so the legend shows what the map leaves out.
                    Rectangle {
                        visible: app.floorFraction > 0
                        height: 6
                        width: Math.round(legendRow.width * app.floorFraction)
                        color: Qt.alpha(app.theme.background, .8)
                        Rectangle { anchors.right: parent.right; width: 1; height: parent.height; color: app.theme.foreground; opacity: .7 }
                    }
                }
            }
            // Timestamp and frame index above the ticks; transport alongside.
            RowLayout {
                id: playbackRow
                Layout.fillWidth: true
                // Empty timestamps and the first tick reserve the same space.
                Layout.minimumHeight: 32
                Layout.preferredHeight: 32
                Layout.maximumHeight: 32
                spacing: 12
                opacity: app.state ? 1 : 0
                enabled: !!app.state
                RowLayout {
                    id: transport
                    Layout.alignment: Qt.AlignBottom
                    spacing: 5
                    GlyphButton { glyph: "first"; visible: !win.compact; enabled: app.frames.length > 1; onClicked: app.jump(false) }
                    GlyphButton { glyph: "back"; enabled: app.frames.length > 1; onClicked: app.step(-1) }
                    GlyphButton { glyph: app.playing ? "pause" : "play"; selected: app.playing; enabled: app.frames.length > 1; onClicked: app.togglePlay() }
                    GlyphButton { glyph: "fwd"; enabled: app.frames.length > 1; onClicked: app.step(1) }
                    GlyphButton { glyph: "last"; visible: !win.compact; enabled: app.frames.length > 1; onClicked: app.jump(true) }
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 4
                    RowLayout {
                        Layout.fillWidth: true
                        Layout.preferredHeight: 14
                        Layout.minimumHeight: 14
                        Layout.maximumHeight: 14
                        spacing: 8
                        LabelText {
                            id: stripStamp
                            Layout.fillWidth: true
                            text: app.scan && app.scan.scanTime ? app.stamp(app.scan.scanTime) : ""
                            font.pixelSize: 10
                            opacity: text !== "" ? .65 : 0
                            horizontalAlignment: Text.AlignLeft
                            elide: Text.ElideRight
                        }
                        LabelText {
                            visible: !win.compact
                            horizontalAlignment: Text.AlignRight
                            text: app.frameIndex >= 0 ? ((app.frameIndex + 1) + " / " + app.frames.length) : ""
                            font.pixelSize: 10
                            opacity: text !== "" ? .65 : 0
                        }
                    }
                    Item {
                        id: strip
                        Layout.fillWidth: true
                        implicitHeight: 14
                        Repeater {
                            model: app.slots
                            Rectangle {
                                required property var modelData
                                required property int index
                                readonly property bool current: index === app.currentSlot
                                readonly property bool tall: current || modelData.partial
                                x: app.slots.length > 1 ? Math.round(index * (strip.width - width) / (app.slots.length - 1)) : Math.round((strip.width - width) / 2)
                                y: Math.round((strip.height - height) / 2)
                                width: tall ? 3 : 2
                                height: tall ? 14 : 8
                                // Even weight either side of the playhead, as the
                                // popover draws it; there are no pads to cliff into.
                                color: current ? app.theme.accent : modelData.partial ? "transparent"
                                    : Qt.alpha(app.theme.foreground, .40)
                                border.width: modelData.partial && !current ? 1 : 0
                                border.color: app.theme.accent
                            }
                        }
                        // Dragging scrubs: the nearest frame under the pointer is sought
                        // once per frame change; the engine pauses on a seek.
                        MouseArea {
                            anchors.fill: parent
                            anchors.topMargin: -6
                            anchors.bottomMargin: -18
                            enabled: app.frames.length > 1
                            property string target: ""
                            function scrub(mx) {
                                var n = app.slots.length;
                                if (n < 2) return;
                                var i = Math.round(Math.max(0, Math.min(1, mx / strip.width)) * (n - 1));
                                var id = app.slots[i].id;
                                if (id && id !== target) { target = id; engine.send({type: "seek", id: id}); }
                            }
                            onPressed: mouse => { target = ""; scrub(mouse.x); }
                            onPositionChanged: mouse => { if (pressed) scrub(mouse.x); }
                        }
                    }
                }
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: 5
                // MOCK: the bar is hidden. Radar moved to the header, the
                // place to the map, treatment and zoom to the keys and wheel.
                visible: false
                Chip {
                    glyph: "locate"
                    label: app.placeLabel
                    on: false
                    onOpened: locationPicker.show("")
                    onToggled: app.run("locate")
                }
                Chip {
                    glyph: "lock"
                    label: app.siteId || "—"
                    tag: app.locked ? "LOCKED" : "FOLLOWING"
                    on: app.locked
                    tagAccent: app.locked
                    enabled: !!app.state
                    onToggled: app.toggleLock()
                    onOpened: locationPicker.show("", "sites")
                }
                Item { width: 6 }
                Item { Layout.fillWidth: true }
                // The treatment chip (DESIGN.md, treatment control): one
                // low-emphasis control naming the treatment; click opens the
                // three in the picker's row style above it, 1 2 3 choose.
                Button {
                    id: treatmentChip
                    implicitHeight: 30
                    implicitWidth: contentItem.implicitWidth + 18
                    padding: 0
                    opacity: treatmentMenu.opened || hovered || activeFocus ? 1 : .7
                    onClicked: treatmentMenu.opened ? treatmentMenu.close() : treatmentMenu.show()
                    contentItem: RowLayout {
                        spacing: 6
                        Item { Layout.fillWidth: true }
                        LabelText { text: app.treatment }
                        Glyph { glyph: "chevron"; implicitWidth: 12 }
                        Item { Layout.fillWidth: true }
                    }
                    background: Rectangle {
                        color: treatmentMenu.opened || treatmentChip.hovered || treatmentChip.activeFocus ? Qt.alpha(app.theme.accent, .18) : "transparent"
                        border.width: 1
                        border.color: treatmentMenu.opened || treatmentChip.activeFocus ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
                    }
                }
                Rectangle { width: 1; height: 18; color: Qt.alpha(app.theme.foreground, .22); Layout.leftMargin: 4; Layout.rightMargin: 4; visible: !win.compact }
                Control { text: "−"; visible: !win.compact; onClicked: map.zoom(Math.min(map.span,map.maxSpan)*1.25) }
                Control { text: "+"; visible: !win.compact; onClicked: map.zoom(Math.min(map.span,map.maxSpan)/1.25) }
            }
          }
          LocationPicker {
            id: locationPicker
            session: app.store
            onManualStarted: app.store.cancelIpLocation()
            anchors.fill: parent
            theme: app.theme
            engine: engine
            sites: engine.sites
            sources: engine.sources
            selectedSourceId: engine.source ? engine.source.id : ""
            closeOnScrim: !app.store.needsLocation
            centerLat: map.centerLat
            centerLon: map.centerLon
            compact: win.compact
            cardTop: layout.anchors.margins + mapFrame.y
            onChosen: row => app.acceptSearch(row)
          }
          // The treatment menu over the surface (not a Popup, which the
          // window overlay would draw outside the captured surface): a card
          // above the chip's right edge in the picker's row style, the
          // current and the hovered row in accent, each row's key at the
          // right. A click outside, Escape, a treatment key, or a choice
          // closes it; Up, Down, and Return choose from the keyboard.
          Item {
            id: treatmentMenu
            anchors.fill: parent
            property bool opened: false
            property int cursor: -1
            visible: opened
            focus: opened
            function show() { cursor = -1; opened = true; forceActiveFocus(); }
            function close() { opened = false; }
            Keys.onPressed: event => {
                if (!opened) return;
                event.accepted = true;
                if (event.key === Qt.Key_Escape) close();
                else if (event.key === Qt.Key_Up) cursor = Math.max(0, (cursor < 0 ? KeyMap.TREATMENTS.indexOf(app.treatment) : cursor) - 1);
                else if (event.key === Qt.Key_Down) cursor = Math.min(KeyMap.TREATMENTS.length - 1, (cursor < 0 ? KeyMap.TREATMENTS.indexOf(app.treatment) : cursor) + 1);
                else if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && cursor >= 0) app.run(KeyMap.TREATMENTS[cursor].toLowerCase());
                else event.accepted = false;
            }
            MouseArea { anchors.fill: parent; onClicked: treatmentMenu.close() }
            Rectangle {
                id: treatmentCard
                readonly property point anchor: treatmentMenu.opened ? treatmentChip.mapToItem(treatmentMenu, treatmentChip.width, 0) : Qt.point(0, 0)
                x: Math.round(anchor.x - width)
                y: Math.round(anchor.y - height - 6)
                width: 168
                height: treatmentRows.implicitHeight + 12
                color: Qt.alpha(app.theme.background, .95)
                border.width: 1
                border.color: app.theme.foreground
                MouseArea { anchors.fill: parent } // a click on the card stays on the card
                ColumnLayout {
                    id: treatmentRows
                    anchors.fill: parent
                    anchors.margins: 6
                    spacing: 0
                    Repeater {
                        model: KeyMap.TREATMENTS
                        Rectangle {
                            id: treatmentRow
                            required property string modelData
                            required property int index
                            readonly property bool current: app.treatment === modelData
                            readonly property bool hot: rowArea.containsMouse || treatmentMenu.cursor === index
                            readonly property color ink: current || hot ? app.theme.accent : app.theme.foreground
                            Layout.fillWidth: true
                            implicitHeight: 28
                            color: hot ? Qt.alpha(app.theme.foreground, .08) : "transparent"
                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 10
                                anchors.rightMargin: 10
                                LabelText { text: treatmentRow.modelData; color: treatmentRow.ink; Layout.fillWidth: true }
                                LabelText {
                                    text: (app.bindings[treatmentRow.modelData.toLowerCase()] || []).map(KeyMap.pretty).join(" ")
                                    color: treatmentRow.ink; font.pixelSize: 10; opacity: .6
                                }
                            }
                            MouseArea { id: rowArea; anchors.fill: parent; hoverEnabled: true; onClicked: app.run(treatmentRow.modelData.toLowerCase()) }
                        }
                    }
                }
            }
          }
          // The `?` sheet over everything, below the map's top edge.
          KeysSheet {
            id: sheet
            anchors.fill: parent
            theme: app.theme
            bindings: app.bindings
            compact: win.compact
            cardTop: layout.anchors.margins + mapFrame.y
          }
        }
        // Opt-in capture uses the actual QML scene at a fixed size, without a compositor.
        Timer {
            interval: Number(Quickshell.env("OMASTORM_CAPTURE_DELAY")) || 2500; running: !app.session && !!Quickshell.env("OMASTORM_CAPTURE"); repeat: false
            onTriggered: surface.grabToImage(result => { result.saveToFile(Quickshell.env("OMASTORM_CAPTURE")); Qt.quit(); })
        }
    }
}
