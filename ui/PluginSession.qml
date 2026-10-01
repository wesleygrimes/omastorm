pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io
import "Location.js" as Location
import "Keys.js" as KeyMap
import "Metar.js" as Metar

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
    // Session aviation overlay; `[metar] show` seeds it, `a` toggles it,
    // neither writes config.toml.
    property bool metarEnabled: false
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
    property string locateKind: ""
    property int locateAttempt: 0
    property int activeAttempt: 0
    readonly property bool locating: locationPending && (locateKind === "locate" || (needsLocation && !ipLocationDismissed))
    property real centerLat: 0
    property real centerLon: 0
    property real span: Location.DEFAULT_SPAN
    property var lock: null
    property bool lockWanted: false
    property string lockSource: ""
    readonly property string lockId: Location.lockSiteId(lock)
    property string lastConfigLock: ""
    property bool pendingLocationPicker: false
    property var appliedExplicit: null
    // GPS follow (DESIGN.md, gpsd follow as built). followPaused is a user
    // choice — a settled pan pauses follow so the camera stays where the
    // user put it; the crosshair chip on the map resumes it. The flag only
    // skips the centring step inside followFix(); the receiver's fix is
    // kept so a resume re-centres on the latest position.
    property bool followPaused: false
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
        locateKind = "";
        locateAttempt += 1;
        locator.queued = false;
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
    // kind is "onboarding" (first view) or "locate" (jump while a view exists).
    function requestApproximateLocation(kind) {
        kind = kind || "onboarding";
        if (!initialized || !ready || locationPending
            || !engine.state || engine.state.mode !== "live") return;
        if (kind === "onboarding" && (hasView || !needsLocation)) return;
        if (kind === "locate" && (!hasView || needsLocation)) return;
        locateAttempt += 1;
        activeAttempt = locateAttempt;
        locateKind = kind;
        ipLocationDismissed = false;
        locationError = "";
        locationPending = true;
        var url = Quickshell.env("OMASTORM_LOCATION_URL") || "https://wttr.in/?format=j2";
        // -k: wttr.in's Let's Encrypt leaf lapses (expired this morning
        // here); this is an IP city estimate, not a trusted channel.
        locator.command = ["curl", "-fsSk", "--max-time", "10", "-A",
            "omastorm (https://omastorm.com)", url];
        // Bind the attempt to this launch. If a prior curl is still dying after
        // cancel, queue one restart instead of overwriting its exit attribution.
        locator.attempt = locateAttempt;
        if (locator.running) {
            locator.queued = true;
            return;
        }
        locator.queued = false;
        locator.running = true;
    }

    function requestIpLocation() { requestApproximateLocation("onboarding"); }

    function applyLocate(place) {
        if (!ready || !hasView || !place
            || !engine.state || engine.state.mode !== "live") {
            locationPending = false;
            locateKind = "";
            return;
        }
        span = Location.scaleSpan(centerLat, place.lat, span);
        centerLat = place.lat;
        centerLon = place.lon;
        placeName = place.name || "";
        locationSource = "ip";
        hasView = true;
        var cfg = Location.configLock(config.values);
        if (cfg && lockSource === "config") {
            lock = Location.polarLock(cfg);
            lockWanted = true;
            lockSource = "config";
        } else {
            lock = null;
            lockWanted = false;
            lockSource = "nearest";
        }
        locationPending = false;
        locateKind = "";
        persist();
        viewChanged();
        applyRadar();
    }

    function acceptIpLocation(place) {
        if (locateKind === "locate") {
            applyLocate(place);
            return;
        }
        if (!ready || hasView || ipLocationDismissed || !place
            || !engine.state || engine.state.mode !== "live") {
            locationPending = false;
            locateKind = "";
            return;
        }
        // Recheck sources that may have arrived while the lookup was pending.
        resolve();
        if (hasView) {
            locationPending = false;
            locateKind = "";
            return;
        }
        centerLat = place.lat;
        centerLon = place.lon;
        placeName = place.name || "";
        locationSource = "ip";
        span = Location.clampSpan(remembered.span);
        hasView = true;
        needsLocation = false;
        locationPending = false;
        locateKind = "";
        persist();
        viewChanged();
        applyRadar();
    }

    function finishIpLocation(exitCode, raw, attempt) {
        // A cancelled or superseded curl can still report; ignore it.
        if (attempt !== undefined && attempt !== locateAttempt) return;
        var locatingNow = locateKind === "locate";
        if (!locatingNow && (ipLocationDismissed || hasView || !needsLocation)) {
            locationPending = false;
            locateKind = "";
            return;
        }
        if (locatingNow && (!hasView || needsLocation || ipLocationDismissed)) {
            locationPending = false;
            locateKind = "";
            return;
        }
        if (exitCode !== 0) {
            locationError = locatingNow
                ? "Couldn’t find your location."
                : "Couldn’t find your location. Try again or choose manually.";
            locationPending = false;
            locateKind = "";
            viewChanged();
            return;
        }
        var place = Location.parseWttrHome(raw);
        if (!place) {
            locationError = locatingNow
                ? "Couldn’t find your location."
                : "Couldn’t find your location. Try again or choose manually.";
            locationPending = false;
            locateKind = "";
            viewChanged();
            return;
        }
        acceptIpLocation(place);
    }

    property Process locator: Process {
        property int attempt: 0
        property bool queued: false
        command: ["true"]
        stdout: StdioCollector { waitForEnd: true }
        onExited: function (exitCode) {
            // A terminate-then-retry left the old curl running; start the queued
            // launch and ignore this exit's stdout/code.
            if (queued) {
                queued = false;
                if (!session.ipLocationDismissed && session.locationPending
                    && attempt === session.locateAttempt) {
                    running = true;
                    return;
                }
                return;
            }
            session.finishIpLocation(exitCode, String(stdout.text || ""), attempt);
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
            lock = Location.polarLock(cfg);
            lockWanted = true;
            lockSource = "config";
        } else if (rememberedView && rememberedView.lock) {
            lock = Location.parseLock(rememberedView.lock);
            lockWanted = !!lock;
            lockSource = "state";
        } else {
            lock = null;
            lockWanted = false;
            lockSource = "nearest";
        }
    }

    function applyConfigLockChange() {
        var cfg = Location.configLock(config.values);
        if (cfg === lastConfigLock) return;
        lastConfigLock = cfg;
        if (cfg) {
            lock = Location.polarLock(cfg);
            lockWanted = true;
            lockSource = "config";
        } else {
            lock = Location.parseLock(remembered.lock);
            lockWanted = !!lock;
            lockSource = lockWanted ? "state" : "nearest";
        }
    }

    // The window and the bar popover are separate processes on one engine and
    // one state.json. Each keeps its own lock intent and re-sends it when the
    // engine comes back, so a restart used to jump to whichever client pushed
    // last, often the bar's launch-time lock. The file is the shared answer:
    // follow it whenever another client changes it, and read it again before
    // re-sending on reconnect. Config's locked_radar still outranks it.
    function adoptRememberedLock() {
        if (!ready || lockSource === "config") return;
        var next = Location.parseLock(remembered.lock);
        if (Location.lockEquals(next, lockWanted ? lock : null)) return;
        lock = next;
        lockWanted = !!next;
        lockSource = lockWanted ? "state" : "nearest";
    }

    // Same rule as the lock: state.json is the shared camera. A client that
    // still has an older place in memory (a city search, then mise restart)
    // must not push that centre back over the file or the other client.
    function adoptRememberedView() {
        if (!ready || !hasView) return;
        if (Location.configCenter(config.values) || Location.envView(Quickshell.env("OMASTORM_VIEW")))
            return;
        var view = remembered.parsed;
        if (!view || !Location.validPair(view.lat, view.lon)) return;
        var nextSpan = Location.clampSpan(view.span);
        var same = hasView
            && Math.abs(centerLat - view.lat) < 1e-6
            && Math.abs(centerLon - view.lon) < 1e-6
            && span === nextSpan;
        if (same) {
            if (view.name && placeName !== view.name) placeName = view.name;
            return;
        }
        centerLat = view.lat;
        centerLon = view.lon;
        span = nextSpan;
        if (view.name) placeName = view.name;
        hasView = true;
        needsLocation = false;
        if (locationSource !== "config") locationSource = "state";
        viewChanged();
    }

    function persist() {
        if (!hasView) return;
        remembered.snapshot(centerLat, centerLon, span, lockWanted ? lock : null, placeName);
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
        if (config.fix && !followPaused) followPaused = true;
        persistTimer.restart();
    }

    function setPlace(lat, lon, name) {
        if (!Location.validPair(lat, lon)) return;
        cancelIpLocation();
        if (config.fix) followPaused = true;
        placeName = name || "";
        locationSource = "state";
        needsLocation = false;
        pendingLocationPicker = false;
        span = hasView ? Location.scaleSpan(centerLat, lat, span) : Location.DEFAULT_SPAN;
        centerLat = lat;
        centerLon = lon;
        hasView = true;
        var cfg = Location.configLock(config.values);
        if (cfg && lockSource === "config") {
            lock = Location.polarLock(cfg);
            lockWanted = true;
            lockSource = "config";
        } else {
            lock = null;
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
        if (config.fix) followPaused = true;
        span = Location.DEFAULT_SPAN;
        hasView = true;
        persist();
        viewChanged();
        applyRadar();
    }

    function chooseLock(lockObj, lat, lon, name) {
        lat = Number(lat);
        lon = Number(lon);
        if (!lockObj || !Location.validPair(lat, lon)) return;
        cancelIpLocation();
        if (config.fix) followPaused = true;
        placeName = name || Location.lockKey(lockObj);
        locationSource = "state";
        needsLocation = false;
        pendingLocationPicker = false;
        span = hasView ? Location.scaleSpan(centerLat, lat, span) : Location.DEFAULT_SPAN;
        centerLat = lat;
        centerLon = lon;
        hasView = true;
        lock = lockObj;
        lockWanted = true;
        lockSource = "state";
        persist();
        viewChanged();
        applyRadar();
    }

    function chooseRadar(id, lat, lon, name) {
        chooseLock(Location.polarLock(id), lat, lon, name);
    }

    function chooseMosaic(id, lat, lon, name) {
        chooseLock(Location.mosaicLock(id), lat, lon, name);
    }

    function setLock(selection, on) {
        if (on && selection) {
            lock = Location.parseLock(selection) || Location.polarLock(selection);
            lockWanted = !!lock;
            lockSource = "state";
        } else {
            lock = null;
            lockWanted = false;
            lockSource = "nearest";
        }
        persist();
        applyRadar();
    }

    // A GPS fix (DESIGN.md, gpsd follow as built) is a view centre that
    // moves on its own: the map goes to it, and the engine's ordinary
    // hand-off — nearest radar to the centre, with its own hysteresis —
    // picks the station, exactly as a pan would. A lock still holds: the
    // chaser who pinned a radar keeps it while the map follows the car.
    // A parked receiver's jitter is under the 100 m floor; nothing moves
    // for it. Follow starts only once a view exists: the launch one-shot
    // (config centre, remembered state, weather, prompt) owns placement,
    // and `gpsd = true` follows after it, never instead of it.
    property var appliedFix: null
    function followFix(fix) {
        if (!fix) { appliedFix = null; return; }
        appliedFix = fix;
        if (followPaused) return;
        if (!hasView || needsLocation) return;
        if (Location.distanceKm(fix.lat, fix.lon, centerLat, centerLon) < 0.1) return;
        placeName = "GPS";
        locationSource = "gps";
        centerLat = fix.lat;
        centerLon = fix.lon;
        persist();
        viewChanged();
        applyRadar();
    }
    // A user pan pauses follow; the chip resumes it. A resume with the
    // receiver's current fix in hand re-centres on it right away, through
    // the same persist / viewChanged / applyRadar path a pan takes.
    function pauseFollow() { if (!followPaused) followPaused = true; }
    function resumeFollow() { if (followPaused) { followPaused = false; if (config.fix) followFix(config.fix); } }
    function setFollowPaused(paused) { if (paused) pauseFollow(); else resumeFollow(); }
    // The config key flipped. Turning gpsd back on starts following again
    // and clears any pause that carried over; turning it off clears the
    // GPS-only state. The fix itself lives in Config and is cleared there.
    function applyGpsdChange() {
        if (config.gpsd && config.fix && followPaused) followPaused = false;
        if (!config.gpsd) { appliedFix = null; followPaused = false; }
    }

    function nav() {
        return engine.state && engine.state.navigation ? engine.state.navigation : { follow: true, locked: false };
    }
    function currentSiteId() { return engine.selectedSiteId; }
    function currentMode() { return engine.state ? engine.state.mode : ""; }

    function followNearest(id) {
        lock = null;
        lockWanted = false;
        lockSource = "nearest";
        persist();
        if (!engine.state) return;
        if (nav().locked) engine.send({type: "lock", enabled: false});
        if (!nav().follow) engine.send({type: "follow", enabled: true});
        if (hasView) engine.send({type: "view_center", lat: centerLat, lon: centerLon});
        else if (id && (currentSiteId() !== id || currentMode() !== "live"))
            engine.send({type: "select_site", id: id});
    }

    function applyRadar() {
        if (!engine.state || !ready) return;
        if (needsLocation) return;
        var wanted = lockWanted ? Location.parseLock(lock) : null;
        if (wanted) {
            if (wanted.target.kind === "site") {
                if (currentSiteId() !== wanted.target.siteId || currentMode() !== "live")
                    engine.send({type: "select_site", id: wanted.target.siteId});
            } else if (wanted.target.kind === "mosaic") {
                var sel = engine.state.selection;
                if (!sel || sel.sourceId !== wanted.sourceId || sel.target.kind !== "mosaic")
                    engine.send({type: "select_source", id: wanted.sourceId});
            }
            if (!nav().locked) engine.send({type: "lock", enabled: true});
            if (!nav().follow) engine.send({type: "follow", enabled: true});
        } else {
            if (nav().locked) engine.send({type: "lock", enabled: false});
            if (!nav().follow) engine.send({type: "follow", enabled: true});
            if (hasView) engine.send({type: "view_center", lat: centerLat, lon: centerLon});
        }
    }

    function applyMetarConfig() {
        metarEnabled = Metar.enabledFromConfig(config.values);
    }

    function initialize() {
        if (initialized || !engine.state || !ready) return;
        initialized = true;
        resolve();
        adoptRememberedView();
        adoptRememberedLock();
        applyRadar();
        applyMetarConfig();
        persist();
        // A receiver that already has a fix follows once the view above
        // exists; without one (the prompt is up) the fix waits.
        if (config.fix) followFix(config.fix);
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
        function onValuesChanged() { if (session.initialized) { session.resolve(); session.applyRadar(); session.applyMetarConfig(); session.applyGpsdChange(); } }
        function onLocationChanged() { if (!session.hasView) session.resolve(); if (session.initialized) session.applyRadar(); }
        function onFixChanged() { if (session.initialized) session.followFix(session.config.fix); }
        function onGpsdChanged() { if (session.initialized) session.applyGpsdChange(); }
        function onTreatmentChanged() { session.applyTreatment(); }
        function onWeakFloorChanged() { session.applyTreatment(); }
    }
    property Connections rememberedEvents: Connections {
        target: session.remembered
        function onReadyChanged() { session.resolve(); session.initialize(); }
        function onLockChanged() { if (session.initialized) session.adoptRememberedLock(); }
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
    // `omarchy plugin update` fast-forwards the clone and the shell rescans,
    // but the rescan keeps this singleton and its compiled QML, so the bar,
    // the popover, and the window keep running what was loaded, and this
    // bootstrap never re-runs for a new engine pin, until the shell restarts
    // (README, troubleshooting). Watch the manifest: a version other than the
    // one loaded means an update is on disk and waiting.
    property string loadedVersion: ""
    property string installedVersion: ""
    readonly property bool updatePending: !!loadedVersion && !!installedVersion && installedVersion !== loadedVersion
    readonly property string updateNotice: updatePending ? "UPDATED TO " + installedVersion + " · RESTART THE SHELL" : ""
    function restartShell() {
        Quickshell.execDetached(["omarchy", "restart", "shell"]);
    }
    property FileView manifestFile: FileView {
        path: session.root + "/manifest.json"
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: {
            var version = "";
            try { version = String(JSON.parse(text()).version || ""); } catch (e) { return; }
            if (!version) return;
            if (!session.loadedVersion) session.loadedVersion = version;
            session.installedVersion = version;
        }
    }
    property FileView bootstrapLogFile: FileView {
        path: session.bootstrapLog
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: { var lines = text().trim().split("\n"); session.startupError = session.engine.state ? "" : lines[lines.length - 1]; }
    }
    Component.onCompleted: { applyTreatment(); resolve(); bootstrap(); }
}
