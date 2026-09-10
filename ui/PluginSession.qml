pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io
import "Location.js" as Location
import "Keys.js" as KeyMap

QtObject {
    id: session
    // One non-map connection keeps the bar current, including on multiple
    // outputs. Visible maps have separate sockets for their tile rectangles.
    property Engine engine: Engine {}
    property Config config: Config {}
    property Remembered remembered: Remembered {}
    property Theme theme: Theme {}
    property bool windowOpen: false
    property bool initialized: false
    property string treatment: Quickshell.env("OMASTORM_STYLE") || "GLYPHS"
    // The weak-return floor in dBZ, or null for every measured return
    // (DESIGN.md, weak-return floor); config.toml's weak_floor and the `w`
    // key change it, OMASTORM_WEAK outranks the file for captures.
    property var weakFloor: KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) !== undefined ? KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) : KeyMap.DEFAULT_FLOOR
    property string startupError: ""
    readonly property bool ready: config.ready && remembered.ready
    readonly property string persistError: remembered.error ? remembered.error.toUpperCase() : ""
    property bool hasView: false
    property bool needsLocation: false
    property string placeName: ""
    property string locationSource: ""
    property real centerLat: 0
    property real centerLon: 0
    property real span: Location.DEFAULT_SPAN
    property string lockId: ""
    property bool lockWanted: false
    property string lockSource: ""
    property string lastConfigLock: ""
    property bool pendingLocationPicker: false
    property var appliedExplicit: null
    signal viewChanged()
    signal locationPickerRequested()

    function requestLocationPicker() {
        pendingLocationPicker = true;
        locationPickerRequested();
    }

    function resolve() {
        if (!config.ready || !remembered.ready) return;
        var env = Location.envView(Quickshell.env("OMASTORM_VIEW"));
        var explicit = Location.configCenter(config.values);
        var rememberedView = remembered.parsed;
        var place = Location.resolvePlace(explicit, rememberedView, config.location, env);
        if (!hasView) {
            if (place) {
                needsLocation = false;
                centerLat = place.lat;
                centerLon = place.lon;
                span = Location.clampSpan(place.span);
                hasView = true;
                locationSource = place.source;
                placeName = place.name || "";
            } else {
                needsLocation = true;
                locationSource = "";
                placeName = "";
            }
            applyLaunchLock(rememberedView);
        }
        if (explicit) {
            var same = appliedExplicit && appliedExplicit.lat === explicit.lat && appliedExplicit.lon === explicit.lon;
            appliedExplicit = explicit;
            if (hasView && !same) {
                placeName = "";
                locationSource = "config";
                centerLat = explicit.lat;
                centerLon = explicit.lon;
                needsLocation = false;
            }
        } else {
            appliedExplicit = null;
        }
        applyConfigLockChange();
        viewChanged();
    }

    function applyLaunchLock(rememberedView) {
        var cfg = Location.configLock(config.values);
        lastConfigLock = cfg;
        if (cfg) {
            lockId = cfg;
            lockWanted = true;
            lockSource = "config";
        } else if (rememberedView && rememberedView.lock) {
            lockId = rememberedView.lock;
            lockWanted = true;
            lockSource = "state";
        } else {
            lockId = "";
            lockWanted = false;
            lockSource = "nearest";
        }
    }

    function applyConfigLockChange() {
        var cfg = Location.configLock(config.values);
        if (cfg === lastConfigLock) return;
        lastConfigLock = cfg;
        if (cfg) {
            lockId = cfg;
            lockWanted = true;
            lockSource = "config";
        } else {
            lockId = remembered.lock || "";
            lockWanted = !!lockId;
            lockSource = lockWanted ? "state" : "nearest";
        }
    }

    // What the engine shows now, for the view export (docs/configuration.md):
    // the station, the frame's scan time, and whether that frame is the
    // live head — the newest of a live timeline.
    readonly property string shownSite: engine.state ? engine.state.site.id : ""
    readonly property string shownScan: engine.state && engine.state.frame ? engine.state.frame.scanTime || "" : ""
    readonly property bool shownLive: {
        if (!engine.state || !engine.state.frame || engine.state.source !== "live") return false;
        var t = engine.state.timeline || [];
        return t.length > 0 && t[t.length - 1].id === engine.state.frame.id;
    }
    readonly property string shownKey: shownSite + "|" + shownScan + "|" + shownLive
    onShownKeyChanged: if (initialized && hasView) persistTimer.restart()

    function persist() {
        if (!hasView) return;
        remembered.snapshot(centerLat, centerLon, span, lockWanted ? lockId : "", placeName, shownSite, shownScan, shownLive);
    }

    function rememberView(lat, lon, spanKm) {
        if (needsLocation) return;
        if (!Location.validPair(lat, lon)) return;
        var next = Location.clampSpan(spanKm);
        if (hasView && centerLat === lat && centerLon === lon && span === next) return;
        centerLat = lat;
        centerLon = lon;
        span = next;
        hasView = true;
        persistTimer.restart();
    }

    function setPlace(lat, lon, name) {
        if (!Location.validPair(lat, lon)) return;
        placeName = name || "";
        locationSource = "state";
        needsLocation = false;
        pendingLocationPicker = false;
        centerLat = lat;
        centerLon = lon;
        span = Location.DEFAULT_SPAN;
        hasView = true;
        var cfg = Location.configLock(config.values);
        if (cfg && lockSource === "config") {
            lockId = cfg;
            lockWanted = true;
            lockSource = "config";
        } else {
            lockId = "";
            lockWanted = false;
            lockSource = "nearest";
        }
        persist();
        viewChanged();
        applyRadar();
    }

    function resetView() {
        var target = Location.resolveReset(Location.configCenter(config.values), config.location);
        if (target) {
            centerLat = target.lat;
            centerLon = target.lon;
            locationSource = target.source;
            placeName = target.name || "";
        } else if (!hasView) {
            requestLocationPicker();
            return;
        }
        span = Location.DEFAULT_SPAN;
        hasView = true;
        persist();
        viewChanged();
        applyRadar();
    }

    function chooseRadar(id, lat, lon, name) {
        lat = Number(lat);
        lon = Number(lon);
        if (!id || !Location.validPair(lat, lon)) return;
        placeName = name || id;
        locationSource = "state";
        needsLocation = false;
        pendingLocationPicker = false;
        centerLat = lat;
        centerLon = lon;
        hasView = true;
        lockId = id;
        lockWanted = true;
        lockSource = "state";
        persist();
        viewChanged();
        applyRadar();
    }

    function setLock(id, on) {
        if (on && id) {
            lockId = id;
            lockWanted = true;
            lockSource = "state";
        } else {
            lockId = "";
            lockWanted = false;
            lockSource = "nearest";
        }
        persist();
        applyRadar();
    }

    function followNearest(id) {
        lockId = "";
        lockWanted = false;
        lockSource = "nearest";
        persist();
        if (!engine.state) return;
        if (engine.state.site.locked) engine.send({type: "lock", enabled: false});
        if (!engine.state.site.follow) engine.send({type: "follow", enabled: true});
        if (id && (engine.state.site.id !== id || engine.state.source !== "live"))
            engine.send({type: "select_site", id: id});
    }

    function applyRadar() {
        if (!engine.state || !ready) return;
        if (needsLocation) return;
        if (lockWanted && lockId) {
            if (engine.state.site.id !== lockId || engine.state.source !== "live")
                engine.send({type: "select_site", id: lockId});
            if (!engine.state.site.locked) engine.send({type: "lock", enabled: true});
            if (!engine.state.site.follow) engine.send({type: "follow", enabled: true});
        } else {
            if (engine.state.site.locked) engine.send({type: "lock", enabled: false});
            if (!engine.state.site.follow) engine.send({type: "follow", enabled: true});
            if (hasView) engine.send({type: "view_center", lat: centerLat, lon: centerLon});
        }
    }

    function initialize() {
        if (initialized || !engine.state || !ready) return;
        initialized = true;
        resolve();
        applyRadar();
        persist();
    }

    function applyTreatment() {
        var errors = [], wanted = KeyMap.treatment(config.treatment, errors);
        if (!Quickshell.env("OMASTORM_STYLE") && wanted) treatment = wanted;
        if (KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) === undefined) weakFloor = KeyMap.weakFloor(config.weakFloor, errors);
    }

    property Timer persistTimer: Timer { interval: 400; onTriggered: session.persist() }
    property Connections engineEvents: Connections {
        target: session.engine
        function onStateChanged() {
            if (!session.engine.state) session.initialized = false;
            else { session.startupError = ""; session.initialize(); }
        }
    }
    property Connections configEvents: Connections {
        target: session.config
        function onReadyChanged() { session.resolve(); session.initialize(); }
        function onValuesChanged() { if (session.initialized) { session.resolve(); session.applyRadar(); } }
        function onLocationChanged() { if (!session.hasView) session.resolve(); if (session.initialized) session.applyRadar(); }
        function onTreatmentChanged() { session.applyTreatment(); }
        function onWeakFloorChanged() { session.applyTreatment(); }
    }
    property Connections rememberedEvents: Connections {
        target: session.remembered
        function onReadyChanged() { session.resolve(); session.initialize(); }
    }
    // The engine bootstrap (run.sh --ensure: install the pinned engine if
    // needed, start or replace the daemon) runs detached, so a plugin reload
    // mid-install cannot kill it: `omarchy plugin add` clones many files and
    // the registry reloads the plugin on each one. While the engine stays
    // unreachable it is retried every 20 s, so a killed or failed attempt
    // recovers on its own. argv, never shell text, since checkout paths may
    // contain spaces; bash will not start in Quickshell's cwd
    // (qrc:/qs-blackhole), so env -C moves it home. Its stderr lands in
    // bootstrap.log beside the socket, and the popover shows the last line
    // while there is no engine.
    readonly property string root: Quickshell.env("OMASTORM_ROOT") || Quickshell.env("HOME") + "/.config/omarchy/plugins/com.omastorm.radar"
    readonly property string bootstrapLog: (Quickshell.env("XDG_RUNTIME_DIR") || "/tmp") + "/omastorm/bootstrap.log"
    function bootstrap() {
        Quickshell.execDetached(["env", "-C", Quickshell.env("HOME"), "OMASTORM_BOOTSTRAP_LOG=" + bootstrapLog, "bash", root + "/run.sh", "--ensure"]);
    }
    property Timer bootstrapRetry: Timer { interval: 20000; repeat: true; running: !session.engine.state; onTriggered: session.bootstrap() }
    property FileView bootstrapLogFile: FileView {
        path: session.bootstrapLog
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: { var lines = text().trim().split("\n"); session.startupError = session.engine.state ? "" : lines[lines.length - 1]; }
    }
    Component.onCompleted: { applyTreatment(); resolve(); bootstrap(); }
}
