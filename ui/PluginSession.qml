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
    property bool ipLocationDismissed: true
    property bool locationPending: false
    property string locationError: ""
    property int locateAttempt: 0
    property int activeAttempt: 0
    readonly property bool locating: needsLocation && !ipLocationDismissed && locationPending
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
        cancelIpLocation();
        pendingLocationPicker = true;
        locationPickerRequested();
    }

    function cancelIpLocation() {
        ipLocationDismissed = true;
        locationPending = false;
        locateAttempt += 1;
        if (locator.running) locator.running = false;
    }

    function userNavigated(lat, lon, spanKm) {
        if (!Location.validPair(lat, lon)) return;
        if (needsLocation) {
            centerLat = lat;
            centerLon = lon;
            span = Location.clampSpan(spanKm);
            locationSource = "state";
            hasView = true;
            needsLocation = false;
            persist();
            applyRadar();
        }
        cancelIpLocation();
    }

    // One-shot wttr.in estimate (DESIGN.md). UI curl — not the engine.
    function requestIpLocation() {
        if (!initialized || !ready || hasView || !needsLocation
            || locationPending || !engine.state || engine.state.source !== "live") return;
        locateAttempt += 1;
        activeAttempt = locateAttempt;
        ipLocationDismissed = false;
        locationError = "";
        locationPending = true;
        var url = Quickshell.env("OMASTORM_LOCATION_URL") || "https://wttr.in/?format=j2";
        locator.command = ["curl", "-fsS", "--max-time", "10", "-A",
            "omastorm (https://omastorm.com)", url];
        locator.running = true;
    }

    function acceptIpLocation(place) {
        if (!ready || hasView || ipLocationDismissed || !place
            || !engine.state || engine.state.source !== "live") return;
        // Recheck sources that may have arrived while the lookup was pending.
        resolve();
        if (hasView) return;
        centerLat = place.lat;
        centerLon = place.lon;
        placeName = place.name || "";
        locationSource = "ip";
        span = Location.clampSpan(remembered.span);
        hasView = true;
        needsLocation = false;
        locationPending = false;
        persist();
        viewChanged();
        applyRadar();
    }

    function finishIpLocation(exitCode, raw, attempt) {
        // A cancelled or superseded curl can still report; ignore it.
        if (attempt !== undefined && attempt !== locateAttempt) return;
        if (ipLocationDismissed || hasView || !needsLocation) {
            locationPending = false;
            return;
        }
        if (exitCode !== 0) {
            locationError = "Couldn’t find your location. Try again or choose manually.";
            locationPending = false;
            viewChanged();
            return;
        }
        var place = Location.parseWttrHome(raw);
        if (!place) {
            locationError = "Couldn’t find your location. Try again or choose manually.";
            locationPending = false;
            viewChanged();
            return;
        }
        acceptIpLocation(place);
    }

    property Process locator: Process {
        command: ["true"]
        stdout: StdioCollector { waitForEnd: true }
        onExited: function (exitCode) {
            session.finishIpLocation(exitCode, String(stdout.text || ""), session.activeAttempt);
        }
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

    function persist() {
        if (!hasView) return;
        remembered.snapshot(centerLat, centerLon, span, lockWanted ? lockId : "", placeName);
    }

    function rememberView(lat, lon, spanKm) {
        if (needsLocation) return;
        if (!Location.validPair(lat, lon)) return;
        var next = Location.clampSpan(spanKm);
        // Mercator round-trip after applyView can report 35.39999999999999
        // for a pick of 35.4 (#32). Keep the stored centre; only take span.
        var sameCenter = hasView
            && Math.abs(centerLat - lat) < 1e-6
            && Math.abs(centerLon - lon) < 1e-6;
        if (sameCenter && span === next) return;
        if (!sameCenter) {
            centerLat = lat;
            centerLon = lon;
        }
        span = next;
        hasView = true;
        persistTimer.restart();
    }

    function setPlace(lat, lon, name) {
        if (!Location.validPair(lat, lon)) return;
        cancelIpLocation();
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
        cancelIpLocation();
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
    onLocatingChanged: viewChanged()
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
