pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io
import "Sites.js" as Sites
import "Keys.js" as KeyMap

QtObject {
    id: session
    // One non-map connection keeps the bar current, including on multiple
    // outputs. Visible maps have separate sockets for their tile rectangles.
    property Engine engine: Engine {}
    property Config config: Config {}
    property Theme theme: Theme {}
    property bool windowOpen: false
    property bool initialized: false
    property string treatment: Quickshell.env("OMASTORM_STYLE") || "GLYPHS"
    // The weak-return floor in dBZ, or null for every measured return
    // (DESIGN.md, weak-return floor); config.toml's weak_floor and the `w`
    // key change it, OMASTORM_WEAK outranks the file for captures.
    property var weakFloor: KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) !== undefined ? KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) : KeyMap.DEFAULT_FLOOR
    property string startupError: ""
    signal homeRequested()
    // { lat, lon } from config.toml's home_lat/home_lon (docs/protocol.md,
    // configuration), or null. The popover's map centres on it, and it
    // picks the home station when `home_site` is unset. The window reports
    // a value out of range; here a bad pair is simply no point.
    readonly property var homePoint: KeyMap.homePoint(config.homeLat, config.homeLon, [])
    readonly property string homeSite: {
        if (config.homeSite) return config.homeSite;
        var point = homePoint || config.location;
        if (!point) return "";
        var best = "", distance = Infinity;
        for (var site of engine.sites) {
            var km = Sites.distanceKm(point.lat, point.lon, site.lat, site.lon);
            if (km < distance) { best = site.id; distance = km; }
        }
        return best;
    }
    function returnHome() {
        if (!engine.state || !config.ready) return;
        if (homeSite && (engine.state.site.id !== homeSite || engine.state.source !== "live")) {
            engine.send({type: "select_site", id: homeSite});
            homeRequested();
        }
    }
    function initialize() {
        if (initialized || !engine.state || !config.ready) return;
        initialized = true;
        if (!windowOpen || engine.state.source === "archived") returnHome();
        applyFollow();
    }
    function applyFollow() {
        if (engine.state && config.follow !== undefined && config.follow !== engine.state.site.follow)
            engine.send({type: "follow", enabled: config.follow});
    }
    function applyTreatment() {
        var errors = [], wanted = KeyMap.treatment(config.treatment, errors);
        if (!Quickshell.env("OMASTORM_STYLE") && wanted) treatment = wanted;
        if (KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) === undefined) weakFloor = KeyMap.weakFloor(config.weakFloor, errors);
    }
    onHomeSiteChanged: if (initialized) returnHome()
    property Connections engineEvents: Connections {
        target: session.engine
        function onStateChanged() {
            if (!session.engine.state) session.initialized = false;
            else { session.startupError = ""; session.initialize(); }
        }
    }
    property Connections configEvents: Connections {
        target: session.config
        function onReadyChanged() { session.initialize(); }
        function onFollowChanged() { session.applyFollow(); }
        function onTreatmentChanged() { session.applyTreatment(); }
        function onWeakFloorChanged() { session.applyTreatment(); }
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
    Component.onCompleted: { applyTreatment(); bootstrap(); }
}
