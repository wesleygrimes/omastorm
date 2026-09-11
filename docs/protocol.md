# Engine to UI protocol

Version 1. The Rust engine (`omastorm-engine`) is the server. The Quickshell UI is a thin client. Radar values never
travel over this protocol; they go to the GPU as texture files.

## Transport

- Unix stream socket at `$XDG_RUNTIME_DIR/omastorm/engine.sock`.
- Newline-delimited JSON, UTF-8, one object per line, no pretty printing.
- Multiple clients may connect (window and popover). Every client receives every
  broadcast. Commands from any client apply to the shared state.
- Every message has `"type"`. Engine messages also carry `"v": 1`. A client that
  sees an unknown `v` shows an error and stops rendering radar.
- Key order within an object is not significant; clients read keys by name.

## Engine messages

`hello` is sent once on connect, followed immediately by a full `state`.

```json
{"type":"hello","v":1,"engine":"0.1.1",
 "sites":[{"id":"KTLX","name":"Oklahoma City","state":"OK",
           "lat":35.33306,"lon":-97.27748,"altM":388.0}]}
```

`state` is the complete current state, re-sent whenever anything in it changes.
It is small (a few KB) so clients replace rather than merge.

```json
{"type":"state","v":1,
 "source":"archived",
 "connection":{"status":"ok","ageSeconds":0},
 "site":{"id":"KTLX","follow":true,"locked":false},
 "frame":{"id":"KTLX-20130520T201643Z-e0",
          "product":"REF","productName":"Reflectivity","units":"dBZ","elevationDeg":0.48,
          "scanTime":"2013-05-20T20:16:43Z","sweepEnd":"2013-05-20T20:17:00Z",
          "status":"complete",
          "texture":"tex/sweep-KTLX-20130520T201643Z-e0-r3.png",
          "azimuthLut":"tex/azlut-KTLX-20130520T201643Z-e0-r3.png",
          "rays":720,"gates":1832,"firstGateM":2125,"gateSpacingM":250,
          "site":{"lat":35.33306,"lon":-97.27748,"altM":388.0},
          "palette":["#34465f","..."],"bounds":[-32,0,10,20,30,40,45,50,55,60,65,70,96]},
 "timeline":[{"id":"...","scanTime":"...","status":"complete"}],
 "basemap":{"ne":{"version":"5.2.0-pre"},"osm":{"status":"ok","source":"OpenFreeMap",
            "version":"20260830_080001_pt","attribution":"OpenFreeMap © OpenMapTiles Data from OpenStreetMap"}},
 "playing":false}
```

- `source`: `archived` | `live`. The daemon starts `live` with no station:
  `site.id` is empty, `connection.status` is `loading`, the frame is the
  placeholder below, and the first `select_site` goes live on a station.
  Started with `OMASTORM_ARCHIVE` naming a Level II volume (development and
  the checks) it starts `archived` on that scan instead.
- `connection.status`: `ok` | `stale` | `unavailable` | `offline` | `loading`.
  `ageSeconds` is the age of the newest complete frame (0 while there is
  none). `connection` is the only place a lasting error condition lives; it
  describes the engine's data path and no client command can clear it. In live
  mode, the status is `loading` from a `select_site` until the station's first sweep
  arrives or the poller reports; then, with the feed reachable, the status
  follows the age of the newest radial the station has published (the sweep
  in progress while one paints, else the newest complete frame): `ok` under
  10 minutes, `stale` from 10 minutes, `unavailable` from 30 minutes (the
  feed is up and the station is silent: maintenance or an outage), and at
  once when the bucket holds no volume for the station. `offline` is the
  bucket unreachable or unreadable, cleared by the next sweep. Cached frames
  stay in `timeline` under every status. While live the engine re-judges
  once a second and broadcasts when anything changed, so `ageSeconds` and
  the status move on a quiet feed. A `message` is added if a status ever
  needs words.
- `timeline` is every frame a client can `seek` to, oldest first:
  the station's complete frames from its catalog (the newest 60) and, while a
  sweep is painting, that sweep as the last entry with `status` `partial`
  (`complete` otherwise). `frame` is one of them. The engine owns the
  position: a new sweep replaces `frame` while it is the newest entry, and
  leaves it alone once a client stepped or sought elsewhere, until a step or
  seek lands on the newest entry again. Archived, the timeline is the one
  archived frame; before any `select_site` it is empty.
- `playing` is true while the engine advances `frame` one complete timeline
  entry at a time, oldest after newest, pacing the loop to about ten seconds
  (250 ms to 1 s per frame, by how many there are). The sweep in progress is not part of
  the loop.
- `frame.status`: `complete` | `partial`. Partial frames are live sweeps still
  being filled; the texture path changes on every republish (revision suffix).
  While a station's first live sweep loads and nothing is cached, the frame is
  a placeholder that draws nothing: `id` `<SITE>-loading`, `status` `partial`,
  `rays` 1, `gates` 1, empty `scanTime` and `sweepEnd`, the station table's
  coordinates. Before any station is selected the placeholder is `-loading`, sited
  at the middle of the contiguous network.
- Paths are relative to `$XDG_RUNTIME_DIR/omastorm/` and have the form
  `tex/<file>`: the literal prefix `tex/` and exactly one further segment that
  is not empty, `.`, or `..` and contains no `/`, backslash, or NUL. The file
  name is otherwise free and carries no meaning to the UI. Both ends apply this
  one rule: the engine refuses to publish a path that breaks it, and the UI
  rejects a `state` whose path breaks it.
- `frame.product` is the code commands use; `frame.productName` is its display
  name. The engine owns product, unit, and site vocabulary; the UI only cases
  and lays out what it receives, and looks the site name up in `hello.sites`.
- `frame.palette` has one color per class, in class order, and `frame.bounds`
  has one more entry than `palette`: class `i` covers `bounds[i]` up to
  `bounds[i+1]` in `units`. The UI uploads `palette` to the GPU as a
  `palette.length` × 1 texture and labels the legend from `bounds` (first band
  `<bounds[1]`, last band `bounds[n-1]+`), so any band count works and radar
  and legend share one source of color.
- `frame.scale` and `frame.offset` are the moment's own encoding:
  value = (code − offset) / scale in `units`. The UI uses them to place the
  weak-return floor, a view setting in `units`, in code units for the shader
  (lookup rule, below). Both are 0 on the loading placeholder, which
  therefore has no floor.

`error` answers one command from one client. It goes only to the client that
sent the command, `state` does not change, and nothing is broadcast, so a
mistake from the popover cannot erase or flash a condition on the window.
`command` is the command's `type` as the client wrote it; `message` is shown
verbatim. A client shows its latest rejection beside the state until it sends
its next command, ahead of any `connection` condition; a `tiles_needed` the
map sends on its own does not count as the user's next command.

```json
{"type":"error","v":1,"command":"select_site",
 "message":"Unknown site XXXX; stations are listed in hello."}
```

`tile_ready` answers one `tiles_needed` request, tile by tile, to the client that sent it. Like
`error` it is a reply, not shared state. A tile already rendered this session
is answered at once; one the engine cannot produce is not answered, and the
lasting condition shows in `state.basemap.osm.status`. `set` says which data
drew the tile: `osm` when the vector tile was cached or fetched, `ne` (Natural
Earth) otherwise, in which case the tile is announced again under a new path
when `osm` becomes available. `labels` are the tile's places for the overlay.

```json
{"type":"tile_ready","v":1,"set":"osm","z":11,"x":470,"y":808,
 "path":"tiles/osm/11/470/808-3f9a1c2e.png",
 "labels":[{"name":"Moore","lat":35.3395,"lon":-97.4867,"class":"city","rank":8}]}
```

- `path` is relative to `$XDG_RUNTIME_DIR/omastorm/` and has the form
  `tiles/<set>/<z>/<x>/<file>`: the literal prefix, `ne` or `osm`, two
  decimal integers, and one further segment under the texture rule (not
  empty, `.`, or `..`; no `/`, backslash, or NUL). The file name is otherwise
  opaque; it ends in a generation tag so pixels never change under a name the
  UI has cached. Both ends apply the rule.
- `labels[].class` is `capital`, `city`, `town`, or `village`; `rank` is
  lower for more important places (OpenMapTiles `rank`, Natural Earth
  `scalerank`). An `ne` tile carries the Natural Earth places inside it whose
  `min_zoom` is at most `z + 1`, since a 512 px tile shows the ground of four
  256 px tiles one level deeper.
- `state.basemap` describes the tile sources:
  `{"ne":{"version":"5.2.0-pre"},"osm":{"status":"ok","source":"OpenFreeMap",
  "version":"20260830_080001_pt","attribution":"OpenFreeMap © OpenMapTiles Data from OpenStreetMap"}}`.
  `osm.status` is `ok`, `offline` (fetching fails; cached tiles still serve),
  or `unavailable` (no data version known and nothing cached); `osm.version`
  is empty until one is known. `source` and `attribution` come from TileJSON
  (`name`, and `attribution` with its HTML reduced to text) once it has been
  read, and name OpenFreeMap by its host before that. The UI shows
  `osm.attribution` verbatim whenever an `osm` tile is on screen. It is
  shared state: a change is broadcast like any other.

## Client commands

```json
{"type":"select_site","id":"KTLX"}
{"type":"follow","enabled":true}
{"type":"lock","enabled":false}
{"type":"view_center","lat":35.4,"lon":-97.5}
{"type":"search_places","query":"norman","lat":35.4,"lon":-97.5}
{"type":"play"}  {"type":"pause"}  {"type":"step","delta":-1}  {"type":"seek","id":"..."}
{"type":"set_product","product":"REF","elevationIndex":0}
{"type":"tiles_needed","z":11,"x0":469,"y0":807,"x1":472,"y1":810}
```

- `select_site` names a station from `hello.sites`. The engine goes
  live on it: the newest frame in its catalog (the per-station ring buffer) or
  the loading placeholder shows at once with `connection.status` `loading`,
  and a poller replaces the previous station's; the same station again changes
  nothing. An id outside the table is answered with an `error`.
- `view_center` is sent when a pan or zoom settles and the centre moved, not
  per frame. With `follow` on and `lock` off, the engine hands off to the
  station nearest the centre by great-circle distance when that station beats
  the current one by the hysteresis rule (closer than 0.8 of the current
  station's distance and by at least 1 km, so a centre between two stations
  keeps whichever it has; the dead band is about a twentieth of the spacing
  either side of the midpoint); the hand-off is a `select_site`, so `state`
  is broadcast and an uncached station opens on the loading placeholder.
  Locked or not following, or when the current station stays nearest,
  nothing changes and nothing is sent. The engine never moves the camera:
  the centre is the user's. A latitude outside ±90 or a longitude outside
  ±180 is answered with an `error`. `lock` and `follow` are shared flags;
  releasing the lock hands off on the next settle, not at once.
- `search_places` ranks the embedded gazetteer (GeoNames populated places
  with population ≥ 5000, clipped to the NEXRAD network envelope) for the
  location picker and is answered with `places` to the sender only, like
  `tile_ready`. Map labels stay on Natural Earth. `query` is required;
  optional `lat` and `lon` order nearer matches first. Word-start matches
  beat substrings. At most eight results. A blank query returns no results.
  A latitude or longitude outside range is answered with an `error`. The
  reply is not shared state:

```json
{"type":"places","v":1,"query":"jacksonville",
 "results":[{"name":"Jacksonville","lat":30.3322,"lon":-81.6749,"class":"city","rank":8,
             "region":"Florida","country":"US"}]}
```
  `region` is the admin-1 name (a US state, a Canadian province);
  `country` is the ISO 3166-1 alpha-2 code. Either may be omitted when empty.
- `set_product` requests a product and elevation. An unsupported selection
  returns an `error` to its sender and retains the current frame.
- `step` moves `delta` entries along `timeline` from the frame shown, stopping
  at the ends; `seek` shows the entry with `id`. Both stop playback. A stepped
  frame's textures are republished under new `tex/` paths with the frame's
  real `scanTime`; an `id` outside the timeline is answered with an `error`,
  and a move that lands where it already is changes nothing. `play` starts
  the loop when the timeline holds at least two complete frames (otherwise
  nothing changes); `pause` stops it and leaves the frame shown.
- `tiles_needed` is the visible inclusive rectangle at one zoom, at
  most 64 tiles, sent when the viewport settles; it names no set (the engine
  chooses, see `tile_ready`). The engine serves it centre-out, and a newer
  request from the same client supersedes its pending tiles outside the new
  rectangle. Unknown commands are ignored and
  logged. A known command with a missing or mistyped field, or one asking for
  something this build cannot serve, is answered with an `error` event to its
  sender only. `state` is broadcast only when a command changed something, so
  repeating a command does not repeat the broadcast.

## Texture files

Directory `$XDG_RUNTIME_DIR/omastorm/tex/`. Every file is written to a temporary
name and renamed into place. Files are never modified after rename; a change
produces a new name with a bumped `-rN` revision, so Qt image caching can never
show stale pixels. The engine deletes files no `state` has referenced for 30 s.

**Sweep texture (`frame.texture`):** PNG, RGBA, width = gates, height = rays.
Rows are radials sorted by azimuth. Nearest sampling, no mipmaps. When the
radials leave a gap (a live sweep still being filled, a dropout), one blank
row (R, G, B zero) follows the radials and `rays` counts it; the azimuth
lookup names it for every entry farther than 0.75° from any radial, so
unscanned azimuths draw nothing instead of the nearest radial smeared around
the circle. A complete 0.5° or 1° cut needs no blank row.

| Channel | Meaning |
| --- | --- |
| R | palette class + 1; 0 means nothing to draw |
| G | status bits: 1 range folded, 2 below threshold, 4 outside coverage |
| B | raw Level II moment byte, for cursor inspection and the weak-return floor |
| A | 255 |

**Azimuth lookup (`frame.azimuthLut`):** PNG, RGBA, width 3600, height 1.
Entry `i` covers azimuth `i / 10` degrees and names the row whose azimuth is
nearest the entry's center, wrapping at 360, or the blank row when none is
within 0.75°; R and G hold the row index as a little-endian 16-bit value.

**Lookup rule (UI shader, `ui/shaders/radar.frag`):** each 3 px screen cell
becomes a site-relative ground distance and an azimuth clockwise from north:
the cell's centre goes from Web Mercator to longitude and latitude and then,
on a sphere of radius 6,371 km, to the great-circle distance and initial
bearing from the site. Ground distance converts to slant range on the
4/3 effective-radius earth,
`r = R sin(s/R) / cos(elevationDeg + s/R)`, the inverse of pyart's
`antenna_to_cartesian`, so gates land where the golden reference places them.
The nearest gate is `round((r - firstGateM) / gateSpacingM)`; more than half a
gate before the first or past the last draws nothing. The azimuth's entry gives
the row, and the sweep is sampled at `(gate + 0.5) / gates, (row + 0.5) / rays`.
`rays`, `gates`, `firstGateM`, `gateSpacingM`, and `elevationDeg` travel as
shader uniforms; they are geometry, not radar values.
The weak-return floor is the one view setting the shader applies to values: `weakBelow`, a code threshold the UI
derives from `frame.scale` and `frame.offset` for the floor in `units`
(`ceil(floor × scale + offset)`), and a measured code (2 and up) below it
draws nothing, exactly as a blank cell does; 0 is no floor. Folded and
below-threshold codes are never weak, and the legend names the hidden range.

**Tiles:** `$XDG_RUNTIME_DIR/omastorm/tiles/<set>/<z>/<x>/<y>-<gen>.png`,
Web Mercator XYZ numbering, 512 px, RGBA antialiased masks tinted by the
UI's shader (`ui/shaders/tile.frag`):

| Channel | Meaning |
| --- | --- |
| R | boundaries (country, state, province) |
| G | water (shorelines, lake shores, rivers) |
| B | minor roads |
| A | 255 − major-road coverage |

A is inverted because Qt Quick premultiplies an image by its alpha on upload,
so a texel with A = 0 loses R, G, and B. An empty tile is therefore fully
opaque, and the UI shader
recovers the straight channels by dividing by A and reads major roads as
1 − A. An `ne` tile has B zero and A 255 everywhere, since roads exist only
in `osm` data. Sets: `ne` (Natural Earth, embedded in the binary, any zoom) and
`osm` (OpenMapTiles-schema vector tiles fetched lazily from z7; roads exist
only here). Masks are rasterized on demand into the runtime directory, never
modified, and dropped oldest-first past 4,096 files. The fetched vector tiles,
not the masks, are what persists: `$XDG_CACHE_HOME/omastorm/vt/<source>/<version>/<z>/<x>/<y>.pbf`,
512 MB ceiling, least-recently-read evicted.

## Golden files

Committed under `golden/<fixture>/`, produced with pyart and consumed by Rust
decoder tests. pyart is not part of the project;
`sweep0.json` records the exact pyart calls and version used so fixture generation
is reproducible.

- `golden/<fixture>/sweep0.json`: `rays`, `gates`, `moment`, `scale`, `offset`,
  `firstGateM`, `gateSpacingM`, `azimuthDeg[]`, `elevationDeg[]`,
  `rayTimeBase`, `rayTimeMs[]`, site coordinates, source file hash, row order,
  and provenance.
- `golden/<fixture>/sweep0.u8`: flat uint8, row-major `rays × gates`, raw
  Level II moment codes in ascending-azimuth row order (0 below threshold,
  1 range folded, 2..255 measured; dBZ = (code − offset) / scale).

`rayTimeMs` counts from the first radial in decoded (file) order, before the
azimuth sort; `rayTimeBase` names that instant to the second. Angles are
written with four decimals.

Current fixture `ktlx-20130520`: 720 × 1832, first gate 2125 m, 250 m spacing,
195,199 measured gates, no range-folded gates. The Rust decoder test
(`engine/src/sweep.rs`) matches every byte, angle, and ray time exactly.

## Live frames

A live frame is the lowest cut (elevation number 1) of the current volume of
the selected station, reflectivity, assembled from the real-time chunk bucket
as chunks arrive: `id` is `<SITE>-<scanTime compact>-e0`, `scanTime` and
`sweepEnd` are the collection times of the cut's first and last radial so
far, `elevationDeg` the rays' mean angle, `site` the station table's
coordinates, and `product`, `palette`, and `bounds` the engine's reflectivity
vocabulary shared with the fixture. Each chunk that grows the cut republishes
the texture as `partial`; the cut's last radial (or the next cut's first)
makes it `complete`, and complete frames enter the per-station catalog under
`$XDG_CACHE_HOME/omastorm/frames/` (SQLite catalog plus the PNGs; 60 per
station; the UI never reads it). Selecting a station shows its newest
catalogued frame while the poller replays the current volume's lowest cut
from the bucket, so a picture arrives within seconds and the next volume
paints live.

## Configuration

`~/.config/omastorm/config.toml` and
`$XDG_STATE_HOME/omastorm/state.json` are read by the UI, never by the engine.
Explicit preferences override remembered view state. The UI resolves the map
center and radar lock independently, then sends `select_site`, `lock`,
`follow`, and settled `view_center` commands as needed. Unlocked navigation
uses follow; locked navigation preserves the selected radar. Treatment and
the weak-return floor stay in the UI. File ownership, launch precedence,
onboarding, and validation are in [configuration.md](configuration.md).

## Implementation notes

The shell session keeps a status connection; each visible popover and expanded
window has its own connection so tile requests remain independent. Expand
uses the shared station, frame, and play state directly, sending no
select/seek/play commands, and preserves the map center and zoom. Closing
preserves the view rather than selecting another station. The session owns
remembered-view writes so surfaces do not overwrite one another's state.
On launch, the UI applies explicit config over remembered state. Reconnecting
to the engine restores the necessary selection and flags without resetting
the active camera. A change of frame or station never re-centers the map
except when the user picks a station in search, which the UI centres on.
Location picks write state.json. With no location, the popover offers the
prompt if no source supplies a center; approximate IP lookup is a UI `curl` to
wttr.in after an explicit click, not an engine command.

`hello` additionally includes `pid`, `build` (an opaque fingerprint),
`sitesSource`, `sitesRetrieved`, and `sitesNotes`. These allow the launcher to
identify an existing build and preserve the station snapshot's provenance.
The 163-site snapshot includes archived/test sites, not an availability list.
`frame.site` retains the scan's measured coordinates.

On disconnect the UI hides radar and retries; on an unknown version it
hides radar and latches the error until relaunch.
