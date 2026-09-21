# Grid radar adapters

The contract for georeferenced radar mosaics in the live picture.

Related: [DESIGN.md](../DESIGN.md), [protocol.md](protocol.md),
[radar-fetch.md](radar-fetch.md), and source research
[#38](https://github.com/wesleygrimes/omastorm/issues/38).

This file defines settled behavior. Implementation status, source research,
and delivery order belong in issues and pull requests, not here.

## Problem

Omastorm draws two kinds of radar data: NOAA NEXRAD Level II as one polar
sweep at a time, and georeferenced grid mosaics such as EUMETNET OPERA. The
GPU samples gates for polar data and raster cells for mosaics. Site search
lists the station catalog; place search uses the bundled GeoNames cities
clipped to the build-time service envelopes.

Many radar networks publish a **georeferenced composite**, not open
polar volumes. Those products require a grid path; a new polar decoder
cannot represent them.

The same path supports free national and continental mosaics without a
country mode or a second app. Searchable places come from GeoNames and may
exist outside a source's actual coverage.

## Non-goals

- An international mode, region picker, or settings page for "where".
- Vendoring provider GeoTIFFs, COGs, or HDF5 into this MIT tree or
  Releases.
- Choosing HDF5 when the same provider product is available as COG or
  GeoTIFF.
- Precolored consumer tiles or any paid / key-gated feed as a default
  source.
- Polar ODIM / DX / ORD `PVOL` readers. Those stay PolarFamily.
- Faking NEXRAD Level II history, tilts, or a painting sweep on a
  mosaic that has none.
- Forecasts or a second chrome.

## Source vs site

Today a **site** is a dish. `hello.sites` lists them. `select_site`
starts the live loop on one. The map centre and that source are
independent: loading a frame never moves the camera.

A **source** is compiled into a `SourceRegistry`. It is one adapter:
either a PolarFamily site feed or a GridFamily mosaic. A **selection**
is the exact thing being viewed: a source id plus either one polar site
target or the source's mosaic target. The live loop has one active
selection. Switching cancels the previous poller, as `select_site` does
now.

| | PolarFamily | GridFamily |
|---|---|---|
| Kind | `site` | `mosaic` |
| What it is | One dish, polar sweep | Georeferenced raster |
| Coverage | Range around a lat/lon | The mosaic's ground box |
| Geometry | Gates, rays, tilt when real | Grid, CRS, affine |
| Frames | May be `partial` while painting | Complete snapshots |
| History | NEXRAD: last two hours, 60 max | Whatever the adapter actually has |

Map and search pick a **covering** source, where covering means the
source's coverage contains the centre, not that its dish is nearest.
No separate international switch. Prefer a covering PolarFamily site
when one exists. Otherwise pick the covering GridFamily mosaic. A centre
with no covering source is the map without radar, as a station with no
frame is today.

A mosaic is not a fake dish. Do not invent a station id, rings, or a
tilt so it fits `hello.sites`. Lock pins the exact selection: one dish
for polar, one mosaic for grid. Unlock returns to covering-source
selection from the centre. Choosing a polar site in search still locks
that dish and centres on it. Live mosaic sources from `hello.sources`
appear in that same radar list (covering mosaics on an empty browse, or
by id or name when typed); choosing one locks that mosaic and centres
on its coverage. They are not fake dishes and there is no separate
provider picker. Unlocked follow still selects automatically from the
centre.

## On screen

Same chrome, timeline, treatments, and keyboard. Radar color still
comes only from `frame.palette`. Pixels, Glyphs, and Stipple stay.

- **No international mode.** Geography and search already choose.
- **Product line.** Mosaic frames show `frame.productName`, not a
  station sweep name. Reflectivity is preferred whenever a covering
  source publishes it. A rain-rate fallback says that it is an estimate
  and uses its real units; never turn it back into fake dBZ. Attribution
  belongs on screen with the data, as OSM and Natural Earth already do.
- **Tilt.** Show an elevation only when the product is a real tilt.
  Mosaics omit it. Do not send a dummy `0.5`.
- **Rings / lock.** Polar sites keep rings. Mosaics have none, and
  stroke their coverage edge where a dish strokes its footprint arc.
  The lock means the exact selection is pinned, and is yellow on one
  predicate for both families.
- **Timeline.** One tick per real frame, no empty pads. Mosaic
  frames are complete; there is no outlined in-progress sweep unless
  the adapter truly publishes one.
- **History.** Adapters declare their depth. Many mosaics keep
  minutes, not two hours. Loop what is there. Do not pad, repeat, or
  label a short catalog as a 2 h Level II scrubber.
- **Age.** LIVE / STALE / UNAVAILABLE still follow the newest
  complete frame's age, not the join. While the first mosaic or polar
  frame is in flight, “Loading...” is centered on the map; the camera
  does not move.

## Adapter interface

Adapters are compiled in and are not discovered at runtime.
`SourceRegistry` is enum dispatch: polar NEXRAD or a `GridRef` mosaic.
The live loop never names a mosaic id. `select_source`, poll restart,
history depth, and the loading placeholder all go through the registry.
OPERA is one `GridRef` variant, the same shape as the next national
mosaic.

A new live mosaic is:

1. A module that polls (`GridEvent`s stamped with its `source_id`),
   publishes a loading placeholder, and declares coverage, product
   class, `selection_priority`, and history depth.
2. A `GridRef` variant and a field on `SourceRegistry`.
3. Its service box in `engine/src/envelope.rs` (`LIVE_MOSAICS`), so the
   gazetteer and 1:10m tiles grow with follow.

A synthetic fixture may implement `select_source` without polling and
with `covering: false` so follow never picks it.

Hello lists the adapters as `sources`, not one row per dish. A
PolarFamily adapter is one feed with many sites (`hello.sites`). A
GridFamily adapter is one mosaic. PolarFamily NEXRAD keeps today's
chunk join ([radar-fetch.md](radar-fetch.md)); GridFamily adapters
fetch their own objects. Coverage is stored by the adapter and borrowed
by the registry. Selection does not allocate a polygon on every settled
center. `AdapterTarget` makes the selected polar site explicit instead
of hiding mutable selection inside the adapter. Poll tasks are `Send`
because the live loop runs them through Tokio.

Hot path: unsigned HTTPS, no API key, no account. Timeouts and body
caps stay. A source that needs a key on every poll does not ship as a
default.

## GridFrame

GridFamily frames use this wire shape. PolarFamily frames keep the
sweep + azimuth lookup in [protocol.md](protocol.md).

A grid frame is measured values on a georeferenced raster, not a
precolored map and not polar gates.

| Field | Role |
|---|---|
| `id`, `scanTime` | Same job as today |
| `status` | `complete` (mosaics do not paint a sweep) |
| `product`, `productName`, `units` | Engine vocabulary; UI lays it out |
| `palette`, `bounds` | Shared color classes; v2 bounds are JSON numbers, including fractional values |
| `texture` | Runtime RGBA8 PNG of classified values, `tex/` rule unchanged |
| `width`, `height` | Raster size |
| `crs`, `geotransform` | Normalized native georeference below; the shader maps a map cell to a pixel |

No `azimuthLut`. No rays/gates. The decoder classifies each native
measured value against `bounds` before writing the texture: R is palette
class + 1, G bit 0 is missing, G bit 1 is undetect, and B/A are zero/255.
This texture is a display product, not a lossless export of provider
measurements. Grid frames omit polar `scale` / `offset`, so the dBZ
weak-return floor is disabled. Every adapter defines bounds and a palette
in its own units. The engine still owns product, unit, and color; radar
arrays still never enter JSON or QML.

The UI samples a map cell through the grid's georeference, not through
site-relative slant range. That is a second sampling path. It is not
an overlay of someone else's JPEG.

`geotransform` is GDAL's six-number, pixel-corner affine
`[x0, dx, rx, y0, ry, dy]`:

```text
x = x0 + column * dx + row * rx
y = y0 + column * ry + row * dy
```

Texture row zero is the first raster row and PNG rows are stored top to
bottom. Pixel `(column, row)` is centred at
`(column + 0.5, row + 0.5)` in that affine. After applying the inverse
affine, the shader rejects coordinates outside
`[0, width) × [0, height)` before sampling; sampler clamping must never
smear an edge pixel beyond the raster.

`crs` is a tagged object, not a free-form PROJ string. Coordinates
enter it as WGS84 longitude/latitude degrees and leave as x/y, east then
north: metres for projected CRSs and degrees only for `geographic`. It
carries `kind`, an ellipsoid as `semiMajorM` and `inverseFlattening`, and
the named parameters for that projection:

| `kind` | Required parameters |
|---|---|
| `geographic` | none; affine x/y are longitude/latitude degrees |
| `mercator` | `lon0Deg`, `scale`, `falseEastingM`, `falseNorthingM` |
| `transverseMercator` | `lat0Deg`, `lon0Deg`, `scale`, `falseEastingM`, `falseNorthingM` |
| `polarStereographic` | `lat0Deg`, `lon0Deg`, `scale`, `falseEastingM`, `falseNorthingM` |
| `lambertConformalConic` | `lat0Deg`, `lon0Deg`, `standardParallel1Deg`, `standardParallel2Deg`, `falseEastingM`, `falseNorthingM` |
| `lambertAzimuthalEqualArea` | `lat0Deg`, `lon0Deg`, `falseEastingM`, `falseNorthingM` |

Projection parameters are direct members of `crs`; `ellipsoid` and
`datumTransform` are nested objects. A WGS84 geographic fixture is:

```json
{"crs":{"kind":"geographic",
        "ellipsoid":{"semiMajorM":6378137,
                     "inverseFlattening":298.257223563}},
 "geotransform":[-1,0.01,0,1,0,-0.01]}
```

A non-WGS84 datum also carries `datumTransform`. The initial supported
transform is an EPSG position-vector seven-parameter Helmert transform:
`{"kind":"helmert7","translationM":[x,y,z],
"rotationArcSeconds":[x,y,z],"scalePpm":s}`, from the grid datum to
WGS84. Sampling applies its inverse before the projection. British
National Grid therefore transforms WGS84 to OSGB36 before its Airy
transverse-Mercator projection; using the Airy ellipsoid alone is not
conforming.

## Protocol v2

Grid frames are not additive to protocol v1. A v1 client requires the
polar `azimuthLut` path and sweep geometry, so omitting those fields for
a mosaic would make it reject the whole state. Grid support uses protocol
v2; a v1 client reports the existing incompatible-version error instead
of trying to draw a grid.

`hello` lists compiled sources. Polar sites remain in `hello.sites`.
Mosaics do not masquerade as sites. `defaultProductClass` is
`reflectivity`, `precipitationRate`, or `other`.

```json
{"type":"hello","v":2,"engine":"0.1.13",
 "sites":[{"id":"KTLX","sourceId":"nexrad",
           "name":"Oklahoma City","state":"OK",
           "lat":35.33306,"lon":-97.27748,"altM":388.0,
           "coverage":{"kind":"circle","radiusKm":460}}],
 "sources":[{"id":"nexrad","family":"polar","kind":"site",
             "defaultProductClass":"reflectivity",
             "name":"NOAA NEXRAD","attribution":"NOAA NEXRAD"},
            {"id":"opera","family":"grid","kind":"mosaic",
             "defaultProductClass":"reflectivity",
             "name":"EUMETNET OPERA","attribution":"EUMETNET OPERA",
             "selectionPriority":10,
             "coverage":{"kind":"box","north":70,"south":32,
                         "east":50,"west":-30}},
            {"id":"fixture-mosaic","family":"grid","kind":"mosaic",
             "defaultProductClass":"reflectivity",
             "name":"Fixture mosaic",
             "attribution":"Omastorm fixture",
             "selectionPriority":100,
             "coverage":{"kind":"box","north":1,"south":0,
                         "east":1,"west":0}}]}
```

Wire keys use camelCase exactly as shown. Every source carries `id`,
`family`, `kind`, `defaultProductClass`, `name`, and `attribution`; grid
sources also carry `coverage` and `selectionPriority`. Every site carries
`sourceId` and its circle `coverage`; dispatch never infers a source from
a station-id prefix.

In v2, `state.mode` is `archived` or `live`; it takes over the job of
v1's `state.source`. `state.selection` is either the exact selection or
`null`. Navigation flags are valid even with no selection:

```json
{"type":"state","v":2,
 "mode":"live",
 "navigation":{"follow":true,"locked":false},
 "selection":{"sourceId":"nexrad",
              "target":{"kind":"site","siteId":"KTLX"}},
 "connection":{"status":"ok","ageSeconds":24},
 "frame":{"kind":"polar"},
 "timeline":[],
 "basemap":{},
 "playing":false}
```

A grid target is `{"kind":"mosaic"}`. Lock pins the whole `selection`,
not merely its adapter; `locked: true` therefore requires a non-null
selection. With no covering source while unlocked, the engine cancels
the poller and publishes `selection: null`,
`connection: {"status":"idle","ageSeconds":0}`, `frame: null`, an empty
timeline, and `playing: false`. No stale source remains active or hidden.

Mosaic `frame` objects carry `kind: "mosaic"` and georeference fields and
omit polar-only keys. The affine, CRS, width, and height define the texture
extent. Attribution and selection coverage come from the active source in
`hello.sources`; they are not repeated on every frame. Polar frames carry
`kind: "polar"` and keep today's sweep fields. These are the wire forms
of `AdapterFrame`. Protocol v2 adds `idle` to `connection.status`; the
other statuses retain their v1 meanings.

Remembered `state.json` locks use the same exact identity:

```json
{"lock":{"sourceId":"nexrad",
         "target":{"kind":"site","siteId":"KJAX"}}}
```

A mosaic lock has `target: {"kind":"mosaic"}`. The existing string lock
is read as a NEXRAD site for one migration release and rewritten in the
object form. The deliberate `config.toml` `locked_radar` setting remains
a polar site override; mosaics are chosen from the ordinary radar list,
not a config key or a separate provider picker.

`select_site` remains polar-only. A mosaic is selected by its source id
through `select_source`, used to restore an exact remembered mosaic lock,
when the user chooses a live mosaic from the radar list, and by tests.
`select_source` rejects a polar source; use `select_site` to identify its
exact target. Unknown ids error against their own table. Follow / lock /
`view_center` keep their jobs: the engine never moves the camera;
unlocked follow uses the deterministic selection rule below.

The gazetteer and map envelope include every compiled live source.
`search_places` still returns gazetteer places. The UI radar list adds live
mosaic sources from `hello.sources`; a mosaic is not a `hello.sites` row.
Enter on a place centres and unlocks; source selection then follows that
centre. Enter on a mosaic locks it and centres on its coverage.

## Attribution and license

Every adapter carries an attribution string. Show it on screen with
the data and record the provider and license in README. OSM (ODbL) and
Natural Earth stay.

| Rule | Why |
|---|---|
| Live fetch, runtime cache only | Do not vendor government rasters under MIT |
| No GeoTIFF / COG / HDF5 in git or Releases | Same; fixtures are synthetic and tiny |
| App code stays MIT | Provider share-alike applies to redistributed data, not this tree |
| Provider and license recorded | Attribution and redistribution terms are source-specific |
| No key on the hot path | A token in config is not a default source |

An adapter is not compiled into `SourceRegistry` until its access and
license terms satisfy these rules. Unconfirmed providers remain in
external research, not this specification.

## Rules

Settled. Ask before violating.

### Lock and coverage

The lock is yellow when the source is pinned and the camera centre is
outside that source's coverage. One predicate for both families: a
distance test for a dish, a point-in-box or point-in-polygon test for a
mosaic.

Coverage is adapter-declared; `coverageKm` lives in the NEXRAD adapter,
not `ui/RadarMap.qml`. Mosaics stroke their coverage edge where dishes
stroke the footprint arc, densified in Mercator, and have no rings.
Grid coverage is the provider's declared service footprint, not the
validity of each pixel in the current frame. `nodata` draws nothing but
does not trigger a source switch; selection must not flap as contributors
temporarily appear or disappear.

### CRS

Grids sample their native CRS in the shader: lon/lat, then the grid's
CRS, then `inverse(geotransform)` to a pixel. One resample, from
provider pixels. The engine does not warp rasters to Mercator.

A CRS ships only when its forward projection is implemented in the grid
shader. Geographic lon/lat, Mercator, transverse Mercator, polar
stereographic, Lambert conformal conic, and Lambert azimuthal equal area
are the supported set. Anything else fails the build.

A CRS entry carries its ellipsoid; the grid path does not assume the
6371 km sphere the polar path uses. A non-WGS84 datum requires the
defined transform; merely naming a datum-shift error is not sufficient.

### Source selection

Covering means the source's coverage contains the centre. On a settled
centre:

1. If the held polar site contains the centre, today's nearest-site
   hand-off remains: another covering polar site must beat it by
   `HANDOFF_RATIO` 0.8 and `HANDOFF_MARGIN_KM` 1 km.
2. If the held polar site no longer contains the centre, bypass
   hysteresis. Choose the nearest covering polar site immediately.
3. If any polar site covers, PolarFamily wins. Otherwise choose among
   covering grids: reflectivity before precipitation rate before other,
   then highest `selection_priority`, then adapter id.
4. A grid with a better product class or priority preempts the held grid
   as soon as both cover. Equal-ranked grids keep the held one so a
   boundary does not flap.
5. With no covering source, clear the active selection and show the map
   without radar.

Grid priority is compiled adapter metadata, not a country mode or a user
preference. Within one product class, a national product normally ranks
above a continental fallback. Every adapter declares its value, and
overlapping adapters have a selection test. This makes the result
independent of the direction the user entered from while preferring
reflectivity whenever it is available.

### Commands

`select_site` is polar-only: a station from `hello.sites`, implying its
source. Internal hand-off uses it.

`select_source` names a mosaic id from `hello.sources`; that mosaic is
the whole target. A polar id errors with an instruction to use
`select_site`, because a feed with many dishes is not an exact selection.
Unknown ids error against their own table. Protocol v2 retains
`select_site` and adds `select_source`.

## See also

Parent research: [#38](https://github.com/wesleygrimes/omastorm/issues/38).
Provider candidates, access findings, and delivery status stay there
until they become settled source contracts.
