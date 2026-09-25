import QtQuick
import QtQuick.Shapes
import Quickshell
import "Metar.js" as Metar

// The radar map: camera, GPU radar shader, basemap tile layer, camera-translated
// overlay, and pointer handling. Surfaces place it, feed it state,
// and drive the camera through center, span,
// reset(), zoom(), and maxSpan. It draws no chrome and holds no station or
// product strings; every input arrives through its properties. Tiles come and
// go through tilesNeeded and tileReady(); the surface connects them to the
// engine so the map knows nothing of sockets.
Item {
    id: map
    clip: true
    // Hidden sweep Image keeps the source pixmap size (OPERA is 3800×4400).
    // That must not become this item's implicit size or ColumnLayout
    // treats the map's preferred height as thousands of pixels and the
    // tick strip is laid out below the window.
    implicitWidth: 0
    implicitHeight: 0

    // Inputs from the surface.
    property var scan: null          // socket state frame, or null
    property string texture: ""      // engine-written sweep texture (gates × rays)
    property string azimuthLut: ""   // engine-written azimuth lookup (3600 × 1)
    property string siteId: ""
    property string sourceId: ""
    property var sites: []           // hello.sites; locations, not live availability
    property var coverage: null      // hello site or source coverage; never a hardcoded radius
    readonly property bool mosaic: !!(scan && scan.kind === "mosaic")
    // Circle radius comes only from adapter-declared coverage; mosaics use
    // box/polygon containment and leave this unused.
    readonly property real coverageKm: coverage && coverage.kind === "circle" ? coverage.radiusKm : 0
    property string tileRoot: ""     // file URL of the runtime directory, for tile paths
    property var theme
    property string treatment: "GLYPHS"
    // The weak-return floor in dBZ, or null to draw every measured return
    // (DESIGN.md, weak-return floor). The shader compares raw moment codes,
    // so the frame's scale and offset place the floor in code units; a frame
    // without them (the loading placeholder) has no floor.
    property var weakFloor: null
    readonly property int weakBelow: scan && weakFloor !== null && scan.scale > 0 ? Math.max(0, Math.min(256, Math.ceil(weakFloor * scan.scale + scan.offset))) : 0
    property int labelSize: 12
    property real radarOpacity: 1    // the radar layer alone; the basemap keeps its strength
    property bool locked: false      // the accent frame on the active marker and tag (DESIGN.md, markers)
    property bool interactive: true  // false while location prompt/picker owns the surface
    // A frame with a scan time is radar to draw. The loading placeholder
    // (docs/protocol.md, frame.status: no scan time, one blank row) draws no
    // radar; tiles, labels, markers, and coverage still show, so a station
    // waiting for its first sweep is the map without radar (DESIGN.md, lean
    // startup as built). With no station selected yet the rings, crosshair,
    // and tag stay away too.
    readonly property bool drawable: !!scan && scan.scanTime !== ""
    readonly property int bands: scan ? scan.palette.length : 0
    property string error: ""
    signal tilesNeeded(int z, int x0, int y0, int x1, int y1)
    /// ICAO chips replace city names when METAR is on. Click is a pick, not a pan.
    signal metarPicked(var report)
    property var metars: []
    property bool metarMode: false
    // pin: bigger FCC location marker; ICAO stays theme chrome. Default
    // when mark is omitted or there is no config file. chip: filled FCC
    // block. ink: ICAO in FCC, no fill.
    property string metarMark: "pin"
    // The view centre once a pan or zoom settles, when it moved since the last
    // report; the surface sends it as `view_center` and the engine decides the
    // hand-off. The camera is never moved from here in answer.
    signal viewSettled(real lat, real lon)
    function viewBbox() {
        if (width < 1 || height < 1 || unitsPerPixel <= 0) return null;
        var west = longitude(viewCenterX - width / 2 * unitsPerPixel);
        var east = longitude(viewCenterX + width / 2 * unitsPerPixel);
        var north = latitude(viewCenterY - height / 2 * unitsPerPixel);
        var south = latitude(viewCenterY + height / 2 * unitsPerPixel);
        if (!(south < north) || !(west < east)) return null;
        return { south: south, west: west, north: north, east: east };
    }

    // The frame is Web Mercator: the unit square is the world, x east, y
    // south. Defined once here in double and handed to the shaders as the
    // view centre's offset from the site plus the scale.
    readonly property real maxLatitude: 85.05112878
    function mercatorX(lon) { return (lon + 180) / 360; }
    function mercatorY(lat) {
        var phi = Math.max(-maxLatitude, Math.min(maxLatitude, lat)) * Math.PI / 180;
        return (1 - Math.asinh(Math.tan(phi)) / Math.PI) / 2;
    }
    function longitude(mx) { return mx * 360 - 180; }
    function latitude(my) { return Math.atan(Math.sinh(Math.PI * (1 - 2 * my))) * 180 / Math.PI; }
    readonly property var site: scan ? scan.site : null
    // Hello row for the selected station, available as soon as siteId is set
    // and before the first texture lands.
    readonly property var scaleStation: {
        if (!siteId || !sites || !sites.length) return null;
        for (var i = 0; i < sites.length; i++)
            if (sites[i].id === siteId) return sites[i];
        return null;
    }
    // Overlay origin: polar dish, mosaic coverage centroid, or the camera
    // centre. Never Null Island for a selected mosaic.
    readonly property var overlayAnchor: {
        if (site) return { lat: site.lat, lon: site.lon };
        if (coverage) {
            if (coverage.kind === "box")
                return { lat: (coverage.north + coverage.south) / 2,
                         lon: (coverage.west + coverage.east) / 2 };
            if (coverage.kind === "circle" && coverage.lat !== undefined && coverage.lon !== undefined)
                return { lat: coverage.lat, lon: coverage.lon };
            if (coverage.kind === "polygon" && coverage.vertices && coverage.vertices.length) {
                var lat = 0, lon = 0, n = coverage.vertices.length;
                for (var i = 0; i < n; i++) {
                    lat += coverage.vertices[i].lat;
                    lon += coverage.vertices[i].lon;
                }
                return { lat: lat / n, lon: lon / n };
            }
        }
        if (center) return { lat: center.y, lon: center.x };
        return null;
    }
    // Ground-scale latitude is pinned to the camera on lookAt / pan-settle,
    // not to the dish. A radar hand-off or the first sweep must not change
    // kmPerUnit (that used to rescale span and look like a zoom). Until the
    // camera pins one, fall back to hello station / frame / centre.
    property real pinnedScaleLat: 0
    property bool hasPinnedScale: false
    function pinScaleLat(lat) {
        if (!isFinite(lat)) return;
        pinnedScaleLat = Number(lat);
        hasPinnedScale = true;
    }
    readonly property real scaleLat: {
        if (hasPinnedScale) return pinnedScaleLat;
        // Camera before dish/frame. Mosaic selection clears siteId; falling
        // through from the old dish to centre (or the equator) used to change
        // kmPerUnit and look like a zoom jump — the same class of bug as PR #99.
        if (center) return center.y;
        if (scaleStation) return scaleStation.lat;
        if (site) return site.lat;
        return 0;
    }
    readonly property real siteLat: overlayAnchor ? overlayAnchor.lat : 0
    readonly property real siteMx: overlayAnchor ? mercatorX(overlayAnchor.lon) : 0.5
    readonly property real siteMy: overlayAnchor ? mercatorY(overlayAnchor.lat) : 0.5
    // Ground kilometres per Mercator unit at scaleLat, on the shader's
    // 6371 km sphere; span and the range rings are measured there.
    readonly property real kmPerUnit: 2 * Math.PI * 6371 * Math.cos(scaleLat * Math.PI / 180)

    // Camera. `center` is a longitude/latitude point, or null for the home
    // view: the site offset by `home` kilometres east and north. `span` is
    // the ground distance across the shorter viewport side at the scan site,
    // never under 25 km. The longer side shows at most one world, and the view
    // stays inside the tile pyramid on both axes; the whole network fits one
    // Mercator world, so nothing wraps at the date line.
    property var center: null
    readonly property point home: Qt.point(-5, 15)
    property real span: 210
    readonly property real maxSpan: kmPerUnit * Math.min(width, height) / Math.max(1, width, height)
    readonly property real pixelsPerKm: Math.max(Math.min(width, height) / Math.max(25, span), Math.max(width, height) / kmPerUnit)
    readonly property real worldPixels: pixelsPerKm * kmPerUnit
    readonly property real unitsPerPixel: 1 / worldPixels
    readonly property real wantedX: center ? mercatorX(center.x) : siteMx + home.x / kmPerUnit
    readonly property real wantedY: center ? mercatorY(center.y) : siteMy - home.y / kmPerUnit
    readonly property real viewCenterX: Math.max(width/2*unitsPerPixel, Math.min(1-width/2*unitsPerPixel, wantedX))
    readonly property real viewCenterY: Math.max(height/2*unitsPerPixel, Math.min(1-height/2*unitsPerPixel, wantedY))
    function reset() {
        center = null;
        hasPinnedScale = false;
        span = Math.min(210, maxSpan);
    }
    signal navigated(real lat, real lon, real spanKm)
    function zoom(value, notify) {
        if (!interactive) return;
        span = Math.max(25, Math.min(maxSpan, value));
        if (notify !== false) navigated(centerLat, centerLon, span);
    }
    function look(mx, my) {
        // Freeze scale on the first pan so an unpinned camera does not
        // live-zoom while centre tracks the pointer (mosaic has no dish).
        if (!hasPinnedScale) pinScaleLat(scaleLat);
        center = Qt.point(longitude(mx), latitude(my));
    }
    // Centre exactly on a place. Loading frames and radar hand-offs must
    // not call this; the camera is the user's (DESIGN.md, location).
    function lookAt(lat, lon) {
        // A fresh object: assigning Qt.point onto `var` can no-op when Qt
        // treats the previous point as equal, so the camera never moves.
        center = { x: Number(lon), y: Number(lat) };
        pinScaleLat(lat);
    }
    signal resetRequested()
    // The keyboard pan (DESIGN.md, keyboard map): one step is an eighth of
    // the viewport's shorter side, in the given screen direction.
    function pan(dx, dy) {
        if (!interactive) return;
        var stepPixels = Math.max(1, Math.round(Math.min(width, height) / 8)) * unitsPerPixel;
        look(viewCenterX + dx * stepPixels, viewCenterY + dy * stepPixels);
        navigated(centerLat, centerLon, span);
    }
    readonly property real centerLat: latitude(viewCenterY)
    readonly property real centerLon: longitude(viewCenterX)
    // Centre the home view on a station before its frame arrives, so the
    // camera lands once where the state's site will put it.
    function jumpTo(lat, lon) {
        var k = 2 * Math.PI * 6371 * Math.cos(lat * Math.PI / 180);
        center = Qt.point(longitude(mercatorX(lon) + home.x / k), latitude(mercatorY(lat) - home.y / k));
        pinScaleLat(lat);
    }
    // Span is measured at the pinned camera latitude. When that pin moves
    // (lookAt / pan settle), rescale so Mercator scale on screen stays put.
    // Dish hand-offs and the first sweep do not change the pin, so they no
    // longer nudge the zoom.
    property real heldKmPerUnit: 0
    // Restoring a remembered view sets span itself; a pin change under that
    // restore must not rescale it.
    property bool holdSpan: false
    onKmPerUnitChanged: {
        if (hasPinnedScale && center && heldKmPerUnit > 0 && !holdSpan)
            span *= kmPerUnit / heldKmPerUnit;
        heldKmPerUnit = kmPerUnit;
    }
    function distanceKm(lat1, lon1, lat2, lon2) {
        var r = Math.PI / 180, dp = (lat2 - lat1) * r, dl = (lon2 - lon1) * r;
        var h = Math.sin(dp / 2) ** 2 + Math.cos(lat1 * r) * Math.cos(lat2 * r) * Math.sin(dl / 2) ** 2;
        return 2 * 6371 * Math.asin(Math.sqrt(Math.max(0, Math.min(1, h))));
    }
    // The table station nearest the view centre, for `n`; null before hello.
    function nearest() {
        var best = null, bestKm = Infinity;
        for (var s of sites) {
            var km = distanceKm(centerLat, centerLon, s.lat, s.lon);
            if (km < bestKm) { bestKm = km; best = s; }
        }
        return best;
    }
    // Mercator units to viewport pixels.
    function sx(mx) { return width / 2 + (mx - viewCenterX) * worldPixels; }
    function sy(my) { return height / 2 + (my - viewCenterY) * worldPixels; }

    // Tile layer (DESIGN.md, basemap tiles). The zoom whose 512 px tiles land
    // nearest 1:1 on screen is requested for the visible rectangle when the
    // camera settles; every announced tile inside the last request is drawn,
    // placed by the camera so a pan needs no new tiles until it settles.
    readonly property int tileZoom: Math.max(0, Math.min(22, Math.round(Math.log2(worldPixels / 512))))
    property var tiles: ({})         // "z/x/y" -> {set, path, labels}
    property int displayedLevel: -1
    property var request: null       // the last tiles_needed rectangle
    // Whether any drawn tile came from the osm set; the surface shows the
    // engine's attribution while it is true (docs/protocol.md, basemap).
    property bool osmOnScreen: false
    onViewCenterXChanged: { settle.restart(); Qt.callLater(refreshOverlay); }
    onViewCenterYChanged: { settle.restart(); Qt.callLater(refreshOverlay); }
    onUnitsPerPixelChanged: settle.restart()
    // A state change re-asks even for an unchanged rectangle: a restarted
    // engine publishes under a new generation.
    // Re-ask tiles when a frame appears after none (engine reconnect).
    // Live sweeps and mosaic backfill replace `scan` every publish; those
    // must not re-report the camera or the map zooms on every COMP.
    property bool hadScan: false
    onScanChanged: {
        Qt.callLater(stageSweep);
        if (scan && !hadScan) { settle.reask = true; settle.restart(); }
        hadScan = !!scan;
    }
    Timer { id: settle; interval: 120; property bool reask: true; onTriggered: { map.requestTiles(); map.reportCenter(); } }
    // Forgotten when the engine goes away, so a reconnect reports the centre
    // the camera is at rather than the one the old daemon knew.
    property real reportedLat: NaN
    property real reportedLon: NaN
    property real reportedSpan: NaN
    function reportCenter() {
        if (centerLat === reportedLat && centerLon === reportedLon && span === reportedSpan) return;
        reportedLat = centerLat; reportedLon = centerLon; reportedSpan = span;
        // Pin scale at the settled camera so a long pan does not live-zoom,
        // and a later dish hand-off still leaves kmPerUnit alone.
        if (center) pinScaleLat(center.y);
        viewSettled(centerLat, centerLon);
    }
    function tileRect(z) {
        var n = Math.pow(2, z);
        var tile = function(units) { return Math.max(0, Math.min(n - 1, Math.floor(units * n))); };
        return { z: z,
                 x0: tile(viewCenterX - width / 2 * unitsPerPixel), y0: tile(viewCenterY - height / 2 * unitsPerPixel),
                 x1: tile(viewCenterX + width / 2 * unitsPerPixel), y1: tile(viewCenterY + height / 2 * unitsPerPixel) };
    }
    function requestTiles() {
        if (width <= 0 || height <= 0) return;
        // At most 64 tiles per request (docs/protocol.md): a huge viewport
        // steps out a level rather than asking for a rectangle it cannot have.
        var rect = tileRect(tileZoom);
        while (rect.z > 0 && (rect.x1 - rect.x0 + 1) * (rect.y1 - rect.y0 + 1) > 64) rect = tileRect(rect.z - 1);
        var unchanged = request && ["z", "x0", "y0", "x1", "y1"].every(k => request[k] === rect[k]);
        request = rect;
        if (!unchanged || settle.reask) tilesNeeded(rect.z, rect.x0, rect.y0, rect.x1, rect.y1);
        settle.reask = false;
        rebuildTileModel();
    }
    function tileReady(tile) {
        // Late replies from superseded requests must not grow a hidden cache.
        if (!inside(tile.z, tile.x, tile.y, request)) return;
        var key = tile.z + "/" + tile.x + "/" + tile.y;
        var old = tiles[key];
        tiles[key] = { set: tile.set, path: tile.path, labels: tile.labels,
                       ready: !!old && old.path === tile.path && old.ready };
        rebuildTileModel();
    }
    // Keep delegates for tiles that stay, so a pan or a re-announcement never
    // recreates an Image that is already on screen.
    ListModel { id: tileModel }
    function inside(z, x, y, rect) {
        return rect && z === rect.z && x >= rect.x0 && x <= rect.x1 && y >= rect.y0 && y <= rect.y1;
    }
    function imageReady(key, path, ready) {
        if (!tiles[key] || tiles[key].path !== path) return;
        tiles[key].ready = ready;
        Qt.callLater(rebuildTileModel);
    }
    function rebuildTileModel() {
        if (!request) return;
        if (displayedLevel < 0) displayedLevel = request.z;
        var complete = true;
        for (var y = request.y0; y <= request.y1; y++)
            for (var x = request.x0; x <= request.x1; x++) {
                var tile = tiles[request.z + "/" + x + "/" + y];
                if (!tile || !tile.ready) complete = false;
            }
        // Switch atomically: overlapping translucent masks would double the
        // line weight if both levels painted at once. Images preload hidden.
        if (complete) displayedLevel = request.z;
        var heldRect = tileRect(displayedLevel), wanted = {}, retained = {};
        var osm = false, names = {}, candidates = [];
        for (var key in tiles) {
            var parts = key.split("/"), z = Number(parts[0]), col = Number(parts[1]), row = Number(parts[2]);
            var shown = inside(z, col, row, heldRect);
            if (!shown && !inside(z, col, row, request)) continue;
            var t = tiles[key];
            retained[key] = t;
            wanted[key] = { key: key, level: z, column: col, row: row, path: t.path, set: t.set };
            if (!shown || !t.ready) continue;
            if (t.set === "osm") osm = true;
            for (var place of t.labels) candidates.push(place);
        }
        tiles = retained;
        osmOnScreen = osm;
        // Rank is meaningful within a source; class breaks equal ranks, then
        // name and position make order independent of tile arrival order.
        var classes = {capital: 0, city: 1, town: 2, village: 3};
        candidates.sort((a, b) => a.rank - b.rank || classes[a.class] - classes[b.class]
                        || a.name.localeCompare(b.name) || a.lat - b.lat || a.lon - b.lon);
        var nextPlaces = [];
        for (var p of candidates) {
            var id = p.name + "/" + p.lat.toFixed(5) + "/" + p.lon.toFixed(5);
            if (!names[id]) { names[id] = true; nextPlaces.push(p); }
        }
        if (JSON.stringify(places) !== JSON.stringify(nextPlaces)) places = nextPlaces;
        for (var i = tileModel.count - 1; i >= 0; i--) {
            var entry = tileModel.get(i);
            if (!wanted[entry.key]) { tileModel.remove(i); continue; }
            if (wanted[entry.key].path !== entry.path) tileModel.setProperty(i, "path", wanted[entry.key].path);
            delete wanted[entry.key];
        }
        for (var key2 in wanted) tileModel.append(wanted[key2]);
    }
    Repeater {
        model: tileModel
        ShaderEffect {
            // Roles avoid Item's own x, y, and z.
            required property string key
            visible: level === map.displayedLevel
            required property int level
            required property int column
            required property int row
            required property string path
            readonly property real n: Math.pow(2, level)
            // Edges round to whole pixels so neighbouring tiles meet without
            // a seam, at the cost of up to half a pixel of drift.
            readonly property real edgeX: map.sx(column / n)
            readonly property real edgeY: map.sy(row / n)
            x: Math.round(edgeX)
            y: Math.round(edgeY)
            width: Math.round(map.sx((column + 1) / n)) - Math.round(edgeX)
            height: Math.round(map.sy((row + 1) / n)) - Math.round(edgeY)
            property var mask: Image {
                source: map.tileRoot + path
                visible: false
                smooth: true
                mipmap: false
                cache: false
                asynchronous: true
                onStatusChanged: map.imageReady(key, path, status === Image.Ready)
                Component.onCompleted: map.imageReady(key, path, status === Image.Ready)
            }
            property color boundaries: Qt.alpha(map.theme.foreground, .32)
            property color water: Qt.alpha(map.theme.accent, .55)
            property color minorRoads: Qt.alpha(map.theme.foreground, .24)
            property color majorRoads: Qt.alpha(map.theme.foreground, .48)
            fragmentShader: "shaders/tile.frag.qsb"
            onStatusChanged: if (status === ShaderEffect.Error) map.error = "Basemap GPU shader failed: " + log
        }
    }

    // Place labels from the displayed tiles, ranked before collision layout.
    // Laid out in site-relative pixels. Pan translates the parent; collision
    // runs on zoom, resize, theme, data change, or leaving the padded region.
    property var places: []
    property var labels: []
    property var siteLabels: []
    onSitesChanged: scheduleLayout()
    onSiteIdChanged: scheduleLayout()
    onCoverageChanged: scheduleLayout()
    // Same turn as the parent jump: a callLater left labels at the old
    // dish for a frame and the scene bounced on every radar hand-off.
    property bool overlayLive: false
    onSiteMxChanged: overlayLive ? rebuildLabels() : scheduleLayout()
    onSiteMyChanged: overlayLive ? rebuildLabels() : scheduleLayout()
    onWidthChanged: { scheduleLayout(); settle.restart(); }
    onHeightChanged: { scheduleLayout(); settle.restart(); }
    onWorldPixelsChanged: { scheduleScaleLayout(); Qt.callLater(refreshOverlay); }
    onLabelSizeChanged: scheduleLayout()
    onPlacesChanged: scheduleLayout()
    onMetarsChanged: scheduleLayout()
    onMetarModeChanged: scheduleLayout()
    onMetarMarkChanged: scheduleLayout()
    onThemeChanged: scheduleLayout()
    Component.onCompleted: { overlayLive = true; rebuildLabels(); }
    function scheduleLayout() { Qt.callLater(rebuildLabels); }
    // A settled pan changes kmPerUnit and compensates span in the same turn.
    // Check after bindings settle so that transient worldPixels values do not
    // trigger an otherwise identical label layout.
    property real laidOutWorldPixels: NaN
    function scheduleScaleLayout() {
        Qt.callLater(function() {
            // Re-layout only when the scale change can move an overlay point
            // by at least a quarter pixel across the viewport.
            var tolerance = Math.max(1, map.laidOutWorldPixels)
                * 0.25 / Math.max(1, map.width, map.height);
            if (!isFinite(map.laidOutWorldPixels)
                || Math.abs(map.worldPixels - map.laidOutWorldPixels) > tolerance)
                map.rebuildLabels();
        });
    }
    // A padded viewport bounds text and dashed-path work at every zoom.
    // Small pans only translate the scene; replenish before the padding runs
    // out. Keep the anchor fixed between replenishments.
    property real overlayX: siteMx
    property real overlayY: siteMy
    property real overlayScale: 1
    readonly property real overlayHalfX: (width/2 + 256) / overlayScale
    readonly property real overlayHalfY: (height/2 + 256) / overlayScale
    function refreshOverlay() {
        if (Math.abs(viewCenterX-overlayX)*worldPixels > 128
            || Math.abs(viewCenterY-overlayY)*worldPixels > 128
            || worldPixels/overlayScale > 2
            || (width/2+128)*unitsPerPixel > overlayHalfX
            || (height/2+128)*unitsPerPixel > overlayHalfY) {
            overlayX = viewCenterX; overlayY = viewCenterY; overlayScale = worldPixels;
            rebuildLabels();
        }
    }
    TextMetrics {
        id: labelMetrics
        font.family: map.theme.font
        font.pixelSize: map.labelSize
    }
    function rebuildLabels() {
        var started = Date.now();
        // Idle / lean start still lays out station markers and place labels;
        // only radar-specific rings and tags wait for a selection.
        labelMetrics.text = siteId || "";
        var occupied = siteId
            ? [{x:-7, y:-7, w:14, h:14}, {x:7, y:4, w:labelMetrics.advanceWidth+6, h:16}]
            : [], result = [], stations = [];
        // Station IDs take priority over place names. Reserve every marker
        // first; co-located archived/test stations must not cover each other.
        var nearby = sites.filter(s => s.id !== siteId
            && Math.abs(mercatorX(s.lon)-overlayX) <= overlayHalfX
            && Math.abs(mercatorY(s.lat)-overlayY) <= overlayHalfY).sort((a, b) => a.id.localeCompare(b.id));
        for (var s of nearby) {
            var mx = (mercatorX(s.lon) - siteMx) * worldPixels;
            var my = (mercatorY(s.lat) - siteMy) * worldPixels;
            occupied.push({x:mx-4, y:my-4, w:8, h:8});
        }
        var metarOverlay = map.metarMode && map.metars && map.metars.length;
        var overlay = metarOverlay
            ? map.metars.map(function(m) {
                return { name: m.id, lat: m.lat, lon: m.lon, category: m.category || "",
                         raw: m.raw || "", obsTime: m.obsTime || "", id: m.id };
            })
            : places;
        // METAR chips replace city names and must still land next to the
        // selected radar (KLZK/KLIT). Place them before other dish IDs.
        if (metarOverlay) {
            for (var p of overlay) {
                labelMetrics.text = p.name;
                var mtx = (mercatorX(p.lon) - siteMx) * worldPixels, mty = (mercatorY(p.lat) - siteMy) * worldPixels;
                var mtw = labelMetrics.advanceWidth, chosen = null;
                for (var q of [{x:mtx+7,y:mty-8},{x:mtx-mtw-24,y:mty-8},
                               {x:mtx-mtw/2,y:mty+22},{x:mtx-mtw/2,y:mty-28},
                               {x:mtx+10,y:mty+22},{x:mtx+10,y:mty-24},
                               {x:mtx-mtw-24,y:mty+22},{x:mtx-mtw-24,y:mty-28}]) {
                    if (occupied.some(o => q.x<o.x+o.w+5 && q.x+mtw+11>o.x && q.y<o.y+o.h+4 && q.y+20>o.y)) continue;
                    chosen = q; break;
                }
                if (!chosen) continue;
                occupied.push({x:chosen.x,y:chosen.y,w:mtw+6,h:16});
                result.push({name:p.name, x:chosen.x, y:chosen.y,
                             width:mtw+6, markerX:mtx, markerY:mty,
                             category: p.category || "", raw: p.raw || "",
                             obsTime: p.obsTime || "", id: p.id || ""});
            }
        }
        for (var s of nearby) {
            var tx = (mercatorX(s.lon) - siteMx) * worldPixels;
            var ty = (mercatorY(s.lat) - siteMy) * worldPixels;
            labelMetrics.text = s.id;
            var tw = labelMetrics.advanceWidth, chosen = null;
            for (var q of [{x:tx+10,y:ty+4}, {x:tx-tw-16,y:ty+4},
                           {x:tx+10,y:ty-20}, {x:tx-tw-16,y:ty-20}]) {
                if (occupied.some(o => q.x<o.x+o.w+5 && q.x+tw+11>o.x && q.y<o.y+o.h+4 && q.y+20>o.y)) continue;
                chosen = q; break;
            }
            if (!chosen) continue;
            occupied.push({x:chosen.x,y:chosen.y,w:tw+6,h:16});
            stations.push({name:s.id, x:chosen.x, y:chosen.y, width:tw+6});
        }
        if (JSON.stringify(siteLabels) !== JSON.stringify(stations)) siteLabels = stations;
        if (!metarOverlay) {
            for (var p of overlay) {
                labelMetrics.text = p.name;
                var tx = (mercatorX(p.lon) - siteMx) * worldPixels, ty = (mercatorY(p.lat) - siteMy) * worldPixels;
                var tw = labelMetrics.advanceWidth, chosen = null;
                for (var q of [{x:tx+7,y:ty-8},{x:tx-tw-12,y:ty-8},
                               {x:tx-tw/2,y:ty+7},{x:tx-tw/2,y:ty-23}]) {
                    if (occupied.some(o => q.x<o.x+o.w+5 && q.x+tw+11>o.x && q.y<o.y+o.h+4 && q.y+20>o.y)) continue;
                    chosen=q; break;
                }
                if (!chosen) continue;
                occupied.push({x:chosen.x,y:chosen.y,w:tw+6,h:16});
                result.push({name:p.name, x:chosen.x, y:chosen.y,
                             width:tw+6, markerX:tx, markerY:ty,
                             category: p.category || "", raw: p.raw || "",
                             obsTime: p.obsTime || "", id: p.id || ""});
            }
        }
        if (JSON.stringify(labels) !== JSON.stringify(result)) labels = result;
        laidOutWorldPixels = worldPixels;
        if (Quickshell.env("OMASTORM_PROFILE")) console.log("OVERLAY_MS", Date.now()-started);
    }

    // Great-circle destinations on the radar's 6371 km sphere, projected to
    // Mercator. A screen-space ellipse is wrong at high latitudes. Geometry
    // stays fixed inside the padded region; zoom scales points and pan
    // translates the parent. Unwrap longitude about each station.
    function coverageRadiusKm(s) {
        if (s && s.radiusKm !== undefined && s.radiusKm !== null) return s.radiusKm;
        return coverageKm;
    }
    function mercatorOffset(lat, lon) {
        return Qt.point(mercatorX(lon) - siteMx, mercatorY(lat) - siteMy);
    }
    function densifyEdge(lat0, lon0, lat1, lon1, steps, points) {
        for (var i = 0; i < steps; i++) {
            var t = i / steps;
            points.push(mercatorOffset(lat0 + (lat1 - lat0) * t, lon0 + (lon1 - lon0) * t));
        }
    }
    function coverageOutline(cov) {
        if (!cov) return [];
        if (cov.kind === "circle") {
            return coveragePoints({lat: cov.lat, lon: cov.lon, radiusKm: cov.radiusKm});
        }
        var points = [];
        if (cov.kind === "box") {
            densifyEdge(cov.north, cov.west, cov.north, cov.east, 32, points);
            densifyEdge(cov.north, cov.east, cov.south, cov.east, 32, points);
            densifyEdge(cov.south, cov.east, cov.south, cov.west, 32, points);
            densifyEdge(cov.south, cov.west, cov.north, cov.west, 32, points);
            points.push(mercatorOffset(cov.north, cov.west));
            return points;
        }
        if (cov.kind === "polygon" && cov.vertices && cov.vertices.length >= 3) {
            var verts = cov.vertices;
            for (var i = 0; i < verts.length; i++) {
                var a = verts[i], b = verts[(i + 1) % verts.length];
                densifyEdge(a.lat, a.lon, b.lat, b.lon, 16, points);
            }
            points.push(mercatorOffset(verts[0].lat, verts[0].lon));
        }
        return points;
    }
    function coveragePoints(s) {
        if (s && s.coverage) return coverageOutline(s.coverage);
        var phi = s.lat * Math.PI / 180, lambda = s.lon * Math.PI / 180;
        var d = coverageRadiusKm(s) / 6371, points = [];
        for (var i = 0; i <= 360; i++) {
            var bearing = (i % 360) * Math.PI / 180;
            var lat = Math.asin(Math.sin(phi)*Math.cos(d) + Math.cos(phi)*Math.sin(d)*Math.cos(bearing));
            var dl = Math.atan2(Math.sin(bearing)*Math.sin(d)*Math.cos(phi), Math.cos(d)-Math.sin(phi)*Math.sin(lat));
            points.push(Qt.point(mercatorX((lambda+dl)*180/Math.PI)-siteMx, mercatorY(lat*180/Math.PI)-siteMy));
        }
        return points;
    }
    function coverageInReach(s) {
        if (s && s.coverage && s.coverage.kind !== "circle") return true;
        var distance = distanceKm(latitude(overlayY), longitude(overlayX), s.lat, s.lon);
        // Mercator ground scale never exceeds its equatorial scale. This
        // conservative padded-view radius cannot cull an arc crossing the view.
        var radius = Math.hypot(overlayHalfX, overlayHalfY)*2*Math.PI*6371;
        return Math.abs(distance - coverageRadiusKm(s)) <= radius;
    }
    // Clip before Qt tessellates dashes, including circles whose centres are
    // outside the view. Coordinates remain relative to the active-site copy.
    function clippedCoverage(points) {
        var lines = [], line = [], previous = null;
        for (var i=1; i<points.length; i++) {
            var a = points[i-1], b = points[i], lo = 0, hi = 1;
            for (var axis=0; axis<2; axis++) {
                var start = axis ? a.y : a.x, delta = axis ? b.y-a.y : b.x-a.x;
                var middle = axis ? overlayY-siteMy : overlayX-siteMx;
                var reach = axis ? overlayHalfY : overlayHalfX;
                if (Math.abs(delta)<1e-15) {
                    if (Math.abs(start-middle)>reach) hi = -1;
                } else {
                    var t0 = (middle-reach-start)/delta, t1 = (middle+reach-start)/delta;
                    lo = Math.max(lo, Math.min(t0,t1)); hi = Math.min(hi, Math.max(t0,t1));
                }
            }
            if (lo>hi) { previous = null; continue; }
            var p = Qt.point((a.x+(b.x-a.x)*lo)*100000, (a.y+(b.y-a.y)*lo)*100000);
            var q = Qt.point((a.x+(b.x-a.x)*hi)*100000, (a.y+(b.y-a.y)*hi)*100000);
            if (!previous || Math.hypot(p.x-previous.x,p.y-previous.y)>1e-7) {
                line = [p]; lines.push(line);
            }
            line.push(q); previous = q;
        }
        return lines;
    }
    readonly property var coverageSites: {
        if (mosaic && coverage)
            return [{id: sourceId || "mosaic", coverage: coverage, mosaic: true}];
        if (!drawable || !site) return [];
        // Only the active radar gets a footprint; overlapping network circles
        // obscure geography at continental zoom. Use the measured scan site.
        return [{id:siteId, lat:site.lat, lon:site.lon, radiusKm: coverageKm}];
    }
    function crsKindCode(crs) {
        if (!crs) return 0;
        if (crs.kind === "geographic") return 0;
        if (crs.kind === "mercator") return 1;
        if (crs.kind === "transverseMercator") return 2;
        if (crs.kind === "polarStereographic") return 3;
        if (crs.kind === "lambertConformalConic") return 4;
        if (crs.kind === "lambertAzimuthalEqualArea") return 5;
        return -1;
    }
    readonly property var gridCrs: renderMosaic ? renderScan.crs : null
    readonly property var gridEllipsoid: gridCrs && gridCrs.ellipsoid ? gridCrs.ellipsoid : null
    readonly property var gridHelmert: gridCrs && gridCrs.datumTransform && gridCrs.datumTransform.kind === "helmert7"
        ? gridCrs.datumTransform : null
    readonly property var gridAffine: renderMosaic ? renderScan.geotransform : null

    // Keep geometry, palette, sweep, and azimuth lookup together. A new
    // texture decodes off-thread into the back buffer; the visible buffer
    // changes only when both images are ready. Never retain another source.
    readonly property string sweepKey: sourceId + "/" + siteId + "/"
        + (scan && scan.site ? scan.site.id : "") + "/" + (mosaic ? "mosaic" : "polar")
    property var frontBuffer: null
    property var loadingBuffer: null
    readonly property bool radarReady: drawable && !!frontBuffer
        && !!frontBuffer.snapshot && frontBuffer.snapshot.key === sweepKey
    readonly property var renderScan: radarReady ? frontBuffer.snapshot.scan : null
    readonly property bool renderMosaic: !!renderScan && renderScan.kind === "mosaic"
    readonly property int renderBands: renderScan ? renderScan.palette.length : 0
    readonly property int renderWeakBelow: renderScan && weakFloor !== null && renderScan.scale > 0
        ? Math.max(0, Math.min(256, Math.ceil(weakFloor * renderScan.scale + renderScan.offset))) : 0
    property real sweepOpacity: radarReady ? radarOpacity : 0
    Behavior on sweepOpacity { NumberAnimation { duration: 160; easing.type: Easing.OutCubic } }
    onTextureChanged: Qt.callLater(stageSweep)
    onAzimuthLutChanged: Qt.callLater(stageSweep)
    onSweepKeyChanged: Qt.callLater(stageSweep)
    function stageSweep() {
        if (!drawable || !texture) {
            frontBuffer = null;
            loadingBuffer = null;
            sweepA.snapshot = null;
            sweepB.snapshot = null;
            return;
        }
        if (frontBuffer && frontBuffer.snapshot.key !== sweepKey) {
            frontBuffer = null;
        }
        function matches(buffer) {
            return buffer && buffer.snapshot && buffer.snapshot.key === sweepKey
                && buffer.snapshot.texture === texture && buffer.snapshot.lut === azimuthLut;
        }
        if (matches(frontBuffer)) return;
        if (matches(loadingBuffer)) { presentSweep(); return; }
        loadingBuffer = frontBuffer === sweepA ? sweepB : sweepA;
        loadingBuffer.snapshot = { key: sweepKey, scan: scan, texture: texture, lut: azimuthLut };
        Qt.callLater(presentSweep);
    }
    function presentSweep() {
        var buffer = loadingBuffer;
        if (!buffer || !buffer.snapshot || !drawable
            || buffer.snapshot.key !== sweepKey || buffer.snapshot.texture !== texture
            || buffer.snapshot.lut !== azimuthLut || !buffer.ready) return;
        frontBuffer = buffer;
        loadingBuffer = null;
    }
    component SweepBuffer: Item {
        property var snapshot: null
        property alias sweep: pixels
        property alias lut: azimuth
        readonly property bool ready: pixels.status === Image.Ready
            && (snapshot && snapshot.scan.kind === "mosaic" || azimuth.status === Image.Ready)
        onReadyChanged: Qt.callLater(map.presentSweep)
        Image {
            id: pixels
            source: parent.snapshot ? parent.snapshot.texture : ""
            visible: false
            smooth: false
            mipmap: false
            asynchronous: true
            cache: true
        }
        Image {
            id: azimuth
            source: parent.snapshot ? parent.snapshot.lut : ""
            visible: false
            smooth: false
            mipmap: false
            asynchronous: true
            cache: true
        }
    }
    SweepBuffer { id: sweepA }
    SweepBuffer { id: sweepB }
    // The frame's palette as a bands x 1 strip; the shader samples
    // texel centers, so radar and legend share the socket palette.
    // The strip stays visible so its children get scene-graph nodes
    // (a hidden Item's children never render into a layer); the
    // source hides it on screen while rendering it into the texture.
    Item {
        id: paletteStrip
        width: Math.max(1, map.renderBands); height: 1
        Repeater {
            model: map.renderScan ? map.renderScan.palette : []
            Rectangle {
                required property string modelData
                required property int index
                x: index; width: 1; height: 1; color: modelData
            }
        }
    }
    ShaderEffectSource {
        id: paletteTexture
        sourceItem: paletteStrip
        hideSource: true
        visible: false
        smooth: false
        mipmap: false
    }
    ShaderEffect {
        id: radarEffect
        visible: map.radarReady && !map.renderMosaic
        // The radar alone, not the basemap: .6 under UNAVAILABLE.
        opacity: map.sweepOpacity
        anchors.fill: parent
        onStatusChanged: if (status === ShaderEffect.Error) map.error = "Radar GPU shader failed: " + log
        Component.onCompleted: if (GraphicsInfo.api === GraphicsInfo.Software) map.error = "Radar requires GPU rendering (OpenGL/Vulkan)."
        property var sweep: map.frontBuffer ? map.frontBuffer.sweep : sweepA.sweep
        property var azimuthLut: map.frontBuffer ? map.frontBuffer.lut : sweepA.lut
        property var swatches: paletteTexture
        property int bands: map.renderBands
        // Sweep geometry travels as uniforms; the frame's numbers are the
        // only radar values QML ever touches, and they are geometry, not data.
        // Mosaic frames omit polar fields — do not read them while a mosaic
        // is selected or ShaderEffect warns on undefined→int/real.
        property int rays: map.renderMosaic || !map.renderScan || map.renderScan.rays === undefined ? 0 : map.renderScan.rays
        property int gates: map.renderMosaic || !map.renderScan || map.renderScan.gates === undefined ? 0 : map.renderScan.gates
        property real firstGateM: map.renderMosaic || !map.renderScan || map.renderScan.firstGateM === undefined ? 0 : map.renderScan.firstGateM
        property real gateSpacingM: map.renderMosaic || !map.renderScan || map.renderScan.gateSpacingM === undefined ? 1 : map.renderScan.gateSpacingM
        property real elevationDeg: map.renderMosaic || !map.renderScan || map.renderScan.elevationDeg === undefined ? 0 : map.renderScan.elevationDeg
        property int weakBelow: map.renderWeakBelow
        property vector2d viewport: Qt.vector2d(width, height)
        // The camera as the shader wants it: the view centre relative to the
        // site in Mercator units, the scale, and the site's latitude.
        property vector2d centerOffset: Qt.vector2d(map.viewCenterX - map.mercatorX(map.renderScan && map.renderScan.site ? map.renderScan.site.lon : 0), map.viewCenterY - map.mercatorY(map.renderScan && map.renderScan.site ? map.renderScan.site.lat : 0))
        property real unitsPerPixel: map.unitsPerPixel
        property real siteLatDeg: map.renderScan && map.renderScan.site ? map.renderScan.site.lat : 0
        property int treatment: map.treatment === "PIXELS" ? 0 : map.treatment === "GLYPHS" ? 1 : 2
        fragmentShader: "shaders/radar.frag.qsb"
    }
    ShaderEffect {
        id: gridEffect
        visible: map.radarReady && map.renderMosaic
        opacity: map.sweepOpacity
        anchors.fill: parent
        onStatusChanged: if (status === ShaderEffect.Error) map.error = "Grid GPU shader failed: " + log
        property var sweep: map.frontBuffer ? map.frontBuffer.sweep : sweepA.sweep
        property var swatches: paletteTexture
        property int bands: map.renderBands
        property int treatment: map.treatment === "PIXELS" ? 0 : map.treatment === "GLYPHS" ? 1 : 2
        property int weakBelow: 0
        property vector2d viewport: Qt.vector2d(width, height)
        property vector2d viewCenter: Qt.vector2d(map.viewCenterX, map.viewCenterY)
        property real unitsPerPixel: map.unitsPerPixel
        property int crsKind: map.crsKindCode(map.gridCrs)
        property int gridWidth: map.renderScan && map.renderScan.width ? map.renderScan.width : 0
        property int gridHeight: map.renderScan && map.renderScan.height ? map.renderScan.height : 0
        property int hasDatum: map.gridHelmert ? 1 : 0
        property real semiMajorM: map.gridEllipsoid ? map.gridEllipsoid.semiMajorM : 6378137
        property real invFlattening: map.gridEllipsoid ? map.gridEllipsoid.inverseFlattening : 298.257223563
        property real lon0Deg: map.gridCrs && map.gridCrs.lon0Deg !== undefined ? map.gridCrs.lon0Deg : 0
        property real lat0Deg: map.gridCrs && map.gridCrs.lat0Deg !== undefined ? map.gridCrs.lat0Deg : 0
        property real stdParallel1Deg: map.gridCrs && map.gridCrs.standardParallel1Deg !== undefined ? map.gridCrs.standardParallel1Deg : 0
        property real stdParallel2Deg: map.gridCrs && map.gridCrs.standardParallel2Deg !== undefined ? map.gridCrs.standardParallel2Deg : 0
        property real projScale: map.gridCrs && map.gridCrs.scale !== undefined ? map.gridCrs.scale : 1
        property real falseEastingM: map.gridCrs && map.gridCrs.falseEastingM !== undefined ? map.gridCrs.falseEastingM : 0
        property real falseNorthingM: map.gridCrs && map.gridCrs.falseNorthingM !== undefined ? map.gridCrs.falseNorthingM : 0
        property real helmertS: map.gridHelmert ? map.gridHelmert.scalePpm : 0
        property vector3d helmertT: map.gridHelmert
            ? Qt.vector3d(map.gridHelmert.translationM[0], map.gridHelmert.translationM[1], map.gridHelmert.translationM[2])
            : Qt.vector3d(0, 0, 0)
        property vector3d helmertR: map.gridHelmert
            ? Qt.vector3d(map.gridHelmert.rotationArcSeconds[0], map.gridHelmert.rotationArcSeconds[1], map.gridHelmert.rotationArcSeconds[2])
            : Qt.vector3d(0, 0, 0)
        property vector3d geoA: map.gridAffine
            ? Qt.vector3d(map.gridAffine[0], map.gridAffine[1], map.gridAffine[2])
            : Qt.vector3d(0, 1, 0)
        property vector3d geoB: map.gridAffine
            ? Qt.vector3d(map.gridAffine[3], map.gridAffine[4], map.gridAffine[5])
            : Qt.vector3d(0, 0, -1)
        fragmentShader: "shaders/grid.frag.qsb"
    }
    Item {
        id: overlayCamera
        x: map.sx(map.siteMx)
        y: map.sy(map.siteMy)
        // Always on: idle and lean-start still show the network and places.
        // Radar rings, crosshair, and the selection tag gate on siteId/mosaic.
        visible: true
        Repeater {
            id: coverageRepeater
            model: map.coverageSites
            Loader {
                id: footprint
                required property var modelData
                active: map.coverageInReach(modelData)
                // Shape geometry is vector-backed; no map-sized texture or
                // canvas is allocated for a coverage circle at close zoom.
                sourceComponent: Shape {
                    readonly property var vertices: map.coveragePoints(footprint.modelData)
                    // Keep the path fixed while zooming. Scale scene-graph
                    // geometry, not hundreds of JS points on every tick.
                    transform: Scale { xScale: map.worldPixels/100000; yScale: xScale }
                    ShapePath {
                        fillColor: "transparent"
                        strokeColor: Qt.alpha(map.theme.foreground, .12)
                        strokeWidth: 100000/map.worldPixels
                        strokeStyle: ShapePath.DashLine
                        dashPattern: [3, 5]
                        PathMultiline {
                            paths: map.clippedCoverage(vertices)
                        }
                    }
                }
            }
        }
        // Range rings at the site's Mercator scale; over 200 km the scale
        // drifts by a couple of percent, which a ring drawn as a circle hides.
        Repeater {
            model: [50, 100, 150, 200]
            Rectangle {
                required property int modelData
                visible: map.siteId !== "" && !map.mosaic
                width: 2 * modelData * map.worldPixels
                    / (2 * Math.PI * 6371 * Math.cos(map.siteLat * Math.PI / 180))
                height: width
                x: -width / 2; y: -height / 2
                radius: width / 2
                color: "transparent"
                border.width: 1
                border.color: Qt.alpha(map.theme.foreground, .18)
                antialiasing: true
            }
        }
        Repeater {
            model: map.sites.filter(s => s.id !== map.siteId)
            Rectangle {
                required property var modelData
                x: (map.mercatorX(modelData.lon)-map.siteMx)*map.worldPixels-3
                y: (map.mercatorY(modelData.lat)-map.siteMy)*map.worldPixels-3
                width: 6; height: 6
                color: map.theme.background
                border.width: 1; border.color: Qt.alpha(map.theme.foreground, .7)
            }
        }
        Repeater {
            model: map.siteLabels
            Rectangle {
                required property var modelData
                x: modelData.x; y: modelData.y
                width: modelData.width; height: 16
                visible: overlayCamera.x+x >= 8 && overlayCamera.x+x+width <= map.width-8
                    && overlayCamera.y+y >= 26 && overlayCamera.y+y+height <= map.height-30
                color: Qt.alpha(map.theme.background, .88)
                Text {
                    x: 3; anchors.verticalCenter: parent.verticalCenter
                    text: modelData.name; color: Qt.alpha(map.theme.foreground, .75)
                    font.family: map.theme.font; font.pixelSize: map.labelSize
                }
            }
        }
        Repeater {
            model: map.labels
            Item {
                required property var modelData
                // Keep labels clear of the surface's corner annotations.
                visible: overlayCamera.x + modelData.x >= 8
                    && overlayCamera.x + modelData.x + modelData.width <= map.width - 8
                    && overlayCamera.y + modelData.y >= 26
                    && overlayCamera.y + modelData.y + 16 <= map.height - 30
                Rectangle {
                    readonly property bool pin: map.metarMode && map.metarMark === "pin" && !!modelData.category
                    readonly property int pinSize: pin ? 10 : 2
                    readonly property color pinInk: pin && Metar.color(modelData.category)
                        ? Metar.color(modelData.category) : map.theme.foreground
                    x: modelData.markerX - pinSize / 2
                    y: modelData.markerY - pinSize / 2
                    width: pinSize; height: pinSize; radius: pin ? pinSize / 2 : 0
                    color: pinInk
                }
                Rectangle {
                    readonly property bool ink: map.metarMode && map.metarMark === "ink" && !!Metar.color(modelData.category)
                    readonly property bool block: map.metarMode && map.metarMark !== "ink" && map.metarMark !== "pin" && !!Metar.color(modelData.category)
                    x: modelData.x; y: modelData.y
                    width: modelData.width; height: 16
                    color: block ? Qt.alpha(Metar.color(modelData.category), .88)
                         : ink ? "transparent"
                         : Qt.alpha(map.theme.background, .88)
                    Text {
                        x: 3; anchors.verticalCenter: parent.verticalCenter
                        text: modelData.name
                        color: parent.block ? "#ffffff"
                             : parent.ink ? Metar.color(modelData.category)
                             : map.theme.foreground
                        font.family: map.theme.font; font.pixelSize: map.labelSize
                        font.bold: parent.ink
                    }
                }
            }
        }
        Rectangle { x: -7; y: -.5; width: 14; height: 1; color: map.theme.foreground; visible: map.siteId !== "" && !map.mosaic }
        Rectangle { x: -.5; y: -7; width: 1; height: 14; color: map.theme.foreground; visible: map.siteId !== "" && !map.mosaic }
        // The lock: an accent 1 px frame on the marker and the tag.
        // Mosaics have no dish marker; the frame sits on the coverage centroid.
        Rectangle {
            x: -6; y: -6; width: 12; height: 12; color: "transparent"
            visible: map.locked && (map.siteId !== "" || map.mosaic)
            border.width: 1; border.color: map.theme.accent
        }
        Rectangle {
            x: 7; y: 4; width: Math.floor(siteTag.implicitWidth) + 6; height: 16; color: map.theme.background
            visible: map.siteId !== "" || (map.mosaic && map.sourceId !== "")
            border.width: map.locked ? 1 : 0; border.color: map.theme.accent
            Text {
                id: siteTag
                x: 3; anchors.verticalCenter: parent.verticalCenter
                text: map.siteId || (map.mosaic ? map.sourceId : "")
                color: map.theme.foreground
                font.family: map.theme.font; font.pixelSize: map.labelSize
            }
        }
    }
    function metarAt(mx, my) {
        if (!map.metarMode) return null;
        var ox = mx - overlayCamera.x, oy = my - overlayCamera.y;
        var pin = map.metarMark === "pin" ? 6 : 2;
        for (var i = labels.length - 1; i >= 0; i--) {
            var L = labels[i];
            if (!L.raw) continue;
            if (ox >= L.x && ox <= L.x + L.width && oy >= L.y && oy <= L.y + 16) return L;
            if (Math.abs(ox - L.markerX) <= pin && Math.abs(oy - L.markerY) <= pin) return L;
        }
        return null;
    }
    MouseArea {
        anchors.fill: parent
        enabled: map.interactive
        hoverEnabled: true
        cursorShape: pressed ? Qt.ClosedHandCursor
            : map.metarAt(mouseX, mouseY) ? Qt.PointingHandCursor : Qt.OpenHandCursor
        property real lastX
        property real lastY
        property bool dragged: false
        onPressed: mouse => { lastX=mouse.x; lastY=mouse.y; dragged=false; }
        onPositionChanged: mouse => {
            if(pressed && (mouse.x !== lastX || mouse.y !== lastY)) {
                dragged = true;
                map.look(map.viewCenterX - (mouse.x-lastX)*map.unitsPerPixel, map.viewCenterY - (mouse.y-lastY)*map.unitsPerPixel);
                lastX=mouse.x; lastY=mouse.y;
                map.navigated(map.centerLat, map.centerLon, map.span);
            }
        }
        onReleased: mouse => {
            if (!dragged) {
                var report = map.metarAt(mouse.x, mouse.y);
                if (report) map.metarPicked(report);
            }
        }
        onWheel: wheel => {
            // Zoom about the pointer: the ground under it stays put.
            var mx=map.viewCenterX+(wheel.x-width/2)*map.unitsPerPixel;
            var my=map.viewCenterY+(wheel.y-height/2)*map.unitsPerPixel;
            map.zoom(Math.min(map.span,map.maxSpan)*(wheel.angleDelta.y>0?.85:1/.85), false);
            map.look(mx-(wheel.x-width/2)*map.unitsPerPixel, my-(wheel.y-height/2)*map.unitsPerPixel);
            map.navigated(map.centerLat, map.centerLon, map.span);
        }
        onDoubleClicked: map.resetRequested()
    }
}
