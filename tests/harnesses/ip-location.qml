import QtQuick
import Quickshell
import Quickshell.Io
import "Location.js" as Location
ShellRoot {
    id: test
    // Quickshell 0.3.2 destroys root children before singletons, so hand the
    // session back its own engine before quitting rather than the fake.
    property var ownEngine: null
    Component.onCompleted: ownEngine = PluginSession.engine
    function finish() { PluginSession.engine = ownEngine; Qt.quit(); }
    Engine {
        id: fake
        socketFactory: Component { MockSocket {} }
        property var sent: []
        function send(command) { sent = sent.concat([command]); }
    }
    FloatingWindow {
        visible: true
        implicitWidth: 308
        implicitHeight: 260
        LocationPrompt {
            id: consent
            anchors.fill: parent
            session: PluginSession
            theme: PluginSession.theme.snapshot
            onManualChosen: PluginSession.requestLocationPicker()
        }
    }
    property int step: 0
    function action(name) {
        for (var child of consent.children)
            if (child.objectName === name) return child;
        throw new Error("Missing consent action: " + name);
    }
    function assertThat(ok, why) { if (!ok) throw new Error(why); }
    function fresh(values, saved, weather, source) {
        var s = PluginSession;
        if (s.locator.running) s.locator.running = false;
        fake.state = null;
        s.initialized = false;
        s.hasView = false;
        s.needsLocation = false;
        s.ipLocationDismissed = true;
        s.locationPending = false;
        s.locateKind = "";
        s.locationError = "";
        s.appliedExplicit = null;
        s.config.values = values;
        s.remembered.parsed = Location.parseState(saved);
        s.config.location = weather;
        fake.sent = [];
        fake.state = {mode: source || "live", navigation: {follow: true, locked: false}, selection: null, connection: {status: "idle", ageSeconds: 0}};
        s.initialize();
    }
    function waitSettled(next) {
        waiter.next = next;
        waiter.start();
    }
    Timer {
        id: waiter
        property var next
        interval: 20; repeat: true
        onTriggered: {
            var s = PluginSession;
            if (s.locationPending || s.locator.running || s.locator.queued) return;
            stop();
            try { next(); } catch (e) { console.error(e); finish(); }
        }
    }
    Timer {
        interval: 10; running: true; repeat: true
        onTriggered: {
            var s = PluginSession;
            if (!s.ready) return;
            stop();
            try {
                s.engine = fake;
                var good = Location.parseWttrHome('{"nearest_area":[{"areaName":[{"value":"Stamford"}],"latitude":"41.05","longitude":"-73.54"}]}');
                assertThat(good && good.lat === 41.05 && good.name === "Stamford", "parse wttr home");
                assertThat(Location.parseWttrHome("{}") === null, "reject empty wttr");
                assertThat(Location.parseWttrHome('{"nearest_area":[{"latitude":"91","longitude":"0"}]}') === null, "reject bad wttr coords");
                assertThat(Location.parseWttrHome('{"nearest_area":[{"latitude":null,"longitude":null}]}') === null, "reject null wttr coords");
                assertThat(Location.parseWttrHome('{"nearest_area":[{"latitude":"","longitude":""}]}') === null, "reject blank wttr coords");
                var coords = Location.parseCoordQuery("36.23708, -79.97948");
                assertThat(coords && coords.lat === 36.23708 && coords.lon === -79.97948, "comma pair");
                assertThat(Location.parseCoordQuery("36.23708 -79.97948").lat === 36.23708, "space pair");
                assertThat(Location.parseCoordQuery("95, -97.5").error, "lat out of range");
                assertThat(Location.parseCoordQuery("oklahoma") === null, "city is not coords");
                assertThat(Location.looksLikeSiteId("kfcx") && Location.looksLikeSiteId("tlx") && !Location.looksLikeSiteId("tulsa"), "site id heuristic");
                var mixed = Location.mergeSearch(
                    [{kind: "site", name: "KINX"}], [{kind: "place", name: "Tulsa"}], null, "tulsa", false, 4);
                assertThat(mixed[0].name === "Tulsa", "places first for a city");
                var sitesFirst = Location.mergeSearch(
                    [{kind: "site", name: "KFCX"}], [{kind: "place", name: "X"}], null, "kfcx", false, 4);
                assertThat(sitesFirst[0].name === "KFCX", "sites first for a site id");
                var opera = {id:"opera", kind:"mosaic", name:"EUMETNET OPERA",
                    coverage:{kind:"box", north:70, south:32, west:-30, east:50}};
                var covering = Location.rankMosaics([opera, {id:"fixture-mosaic", kind:"mosaic", name:"Fixture",
                    coverage:{kind:"box", north:1, south:0, west:0, east:1}}], "", 48.8, 2.3, true, "opera", 4, true);
                assertThat(covering.length === 1 && covering[0].id === "opera" && covering[0].covering, "covering mosaic in browse");
                var oklahoma = Location.rankMosaics([opera], "", 35, -97, true, "", 4, false);
                assertThat(oklahoma.length === 0, "far mosaic stays out of empty browse");
                var typed = Location.rankMosaics([opera], "eumetnet", 35, -97, false, "", 4, false);
                assertThat(typed.length === 1 && typed[0].id === "opera", "name search finds opera");
                var withOpera = Location.mergeSearch(
                    [{kind:"site", name:"KTLX"}], [], null, "opera", false, 4, typed);
                assertThat(withOpera[0].id === "opera", "mosaic first when the query is its id");

                fresh({}, "", null);
                assertThat(!s.locating && s.needsLocation && !s.locationPending, "startup requires consent");
                s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"X"}],"latitude":"1","longitude":"2"}]}');
                assertThat(!s.hasView, "unsolicited finish ignored while dismissed");

                action("approximateLocation").clicked();
                assertThat(s.locating && s.needsLocation && s.locationPending, "fresh lookup");
                s.requestIpLocation();
                assertThat(s.locationPending, "duplicate lookup ignored while pending");
                waitSettled(afterFirstLookup);
            } catch (e) { console.error(e); finish(); }
        }
    }
    function afterFirstLookup() {
        var s = PluginSession;
        assertThat(s.hasView && !s.needsLocation && s.locationSource === "ip", "IP view");
        assertThat(s.centerLat === 41.05 && s.centerLon === -73.54, "IP coordinates");
        assertThat(s.remembered.lat === 41.05 && s.remembered.lon === -73.54, "remember IP view");
        assertThat(fake.sent.some(c => c.type === "view_center" && c.lat === 41.05), "nearest radar follows IP center");

        fresh({}, JSON.stringify(s.remembered.parsed), null);
        assertThat(s.locationSource === "state" && !s.locationPending, "reopen remembered IP without lookup");
        fresh({center_lat:30, center_lon:-81}, '{"lat":35,"lon":-97}', {lat:36,lon:-79});
        assertThat(s.locationSource === "config" && s.centerLat === 30, "explicit precedence");
        fresh({}, '{"lat":35,"lon":-97}', {lat:36,lon:-79});
        assertThat(s.locationSource === "state" && s.centerLat === 35, "state precedence");
        fresh({}, "", {lat:36,lon:-79});
        assertThat(s.locationSource === "weather" && s.centerLat === 36, "weather precedence");

        fresh({locked_radar:"KTLX"}, "", null);
        s.requestIpLocation();
        waitSettled(afterLock);
    }
    function afterLock() {
        var s = PluginSession;
        assertThat(s.lockId === "KTLX" && s.lockWanted && s.centerLat === 41.05, "configured lock independent of IP center");

        fresh({}, "", null);
        s.requestIpLocation();
        s.setPlace(30,-81,"Picked");
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}');
        assertThat(s.centerLat === 30 && s.placeName === "Picked", "late reply after picker choice");

        fresh({}, "", null);
        s.requestIpLocation();
        s.chooseRadar("KTLX",35,-97,"Radar");
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}');
        assertThat(s.centerLat === 35 && s.lockId === "KTLX", "late reply after radar choice");

        fresh({}, '{"lat":35,"lon":-97,"span":210}', null);
        var kept = Location.scaleSpan(35, 51, 210);
        s.chooseRadar("KTLX", 51, 10, "Hop");
        assertThat(Math.abs(s.span - kept) < 1e-9, "radar pick keeps mercator scale");
        fake.sent = [];
        s.chooseMosaic("opera", 51, 10, "EUMETNET OPERA");
        assertThat(s.lock && s.lock.target.kind === "mosaic" && s.lock.sourceId === "opera", "mosaic lock");
        assertThat(fake.sent.some(c => c.type === "select_source" && c.id === "opera"), "mosaic select_source");

        fresh({}, "", null);
        s.requestIpLocation();
        s.userNavigated(36,-98,170);
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}');
        assertThat(s.centerLat === 36 && s.span === 170 && !s.needsLocation, "late reply after navigation");

        fresh({}, "", null);
        s.requestIpLocation();
        action("manualLocation").clicked();
        assertThat(s.needsLocation && !s.hasView && !s.locating, "manual picker interrupts lookup");

        fresh({}, "", null);
        s.locateAttempt += 1;
        s.activeAttempt = s.locateAttempt;
        s.ipLocationDismissed = false;
        s.locationPending = true;
        s.finishIpLocation(22, "", s.locateAttempt);
        assertThat(s.needsLocation && !s.locating && !!s.locationError, "failure leaves onboarding available");
        s.locateAttempt += 1;
        s.activeAttempt = s.locateAttempt;
        s.ipLocationDismissed = false;
        s.locationPending = true;
        s.locationError = "";
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Stamford"}],"latitude":"41.05","longitude":"-73.54"}]}', s.locateAttempt);
        assertThat(s.hasView && s.locationSource === "ip", "retry accepts successful reply");

        fresh({}, "", null);
        s.locateAttempt += 1;
        s.activeAttempt = s.locateAttempt;
        s.ipLocationDismissed = false;
        s.locationPending = true;
        s.config.location = {lat: 36, lon: -79, name: "Stokesdale"};
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}', s.locateAttempt);
        assertThat(s.locationSource === "weather" && s.centerLat === 36 && !s.locationPending, "resolve mid-lookup clears pending");

        fresh({}, "", null);
        s.locateAttempt += 1;
        s.activeAttempt = s.locateAttempt;
        s.ipLocationDismissed = false;
        s.locationPending = true;
        fake.state = {mode: "archived", navigation: {follow: true, locked: false}, selection: null, connection: {status: "ok", ageSeconds: 0}};
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}', s.locateAttempt);
        assertThat(s.needsLocation && !s.hasView && !s.locationPending, "archive mid-lookup clears pending");

        fresh({}, "", null);
        assertThat(s.needsLocation && !s.locationPending, "no automatic lookup");
        fresh({}, "", null, "archived");
        s.requestIpLocation();
        assertThat(s.needsLocation && !s.locationPending, "archive never locates");
        assertThat(s.config.parseLocation('{"latitude":null,"longitude":null}') === null, "null weather is not zero");

        fresh({}, '{"lat":36.23708,"lon":-79.97948,"span":210,"name":"Stokesdale"}', {lat:36.23708, lon:-79.97948, name:"Stokesdale"});
        assertThat(s.hasView && s.centerLat === 36.23708, "locate starts from a view");
        s.requestApproximateLocation("locate");
        assertThat(s.locateKind === "locate" && s.locationPending, "locate fetch");
        waitSettled(afterLocate);
    }
    function afterLocate() {
        var s = PluginSession;
        assertThat(s.centerLat === 41.05 && s.centerLon === -73.54, "locate recenters");
        assertThat(s.locationSource === "ip" && !s.lockWanted, "locate unlocks nearest");
        var kept = 210 * Math.cos(41.05 * Math.PI / 180) / Math.cos(36.23708 * Math.PI / 180);
        assertThat(Math.abs(s.span - kept) < 0.05, "locate keeps visual zoom");
        s.requestApproximateLocation("locate");
        s.setPlace(30, -81, "Picked");
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}', s.locateAttempt);
        assertThat(s.centerLat === 30 && s.placeName === "Picked", "late locate after picker");

        fresh({}, '{"lat":51.5,"lon":-0.12,"span":210,"name":"London"}', {lat:36,lon:-79});
        assertThat(s.centerLat === 51.5 && s.hasView, "stale session at London");
        s.remembered.parsed = Location.parseState('{"lat":45.455833,"lon":-98.413333,"span":26.5,"name":"ABERDEEN"}');
        s.initialized = false;
        fake.state = null;
        fake.state = {mode: "live", navigation: {follow: true, locked: false}, selection: null, connection: {status: "ok", ageSeconds: 0}};
        s.initialize();
        assertThat(Math.abs(s.centerLat - 45.455833) < 1e-6 && Math.abs(s.centerLon + 98.413333) < 1e-6, "reconnect adopts state.json");
        assertThat(s.placeName === "ABERDEEN", "reconnect adopts remembered name");

        console.log("IP_LOCATION_PASSED");
        finish();
    }
}
