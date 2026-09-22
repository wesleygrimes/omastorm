# Engine

Build and run with [CONTRIBUTING.md](../CONTRIBUTING.md). Distribution and
versioning are in [docs/RELEASING.md](../docs/RELEASING.md).

## Architecture

The binary embeds Natural Earth geography, `data/sites.json`, and
`data/product.json` (the product, palette, and frame template). It embeds no
archived radar. Compiled radar sources live in `src/source.rs`: PolarFamily
NEXRAD and each GridFamily mosaic (OPERA today) are registry entries. A
daemon starts with no selection; `select_site` or `select_source` starts
the matching poller. An `OMASTORM_ARCHIVE` scan is decoded at startup and
labeled archived.
Missing build data
produces an error naming `scripts/extract-fixtures.sh`; vendored archives and checksums
are described in [data/README.md](../data/README.md).

## Runtime and storage

`XDG_RUNTIME_DIR` must name an absolute directory. The daemon owns
`omastorm/engine.sock`, uses `engine.lock` to serialize startup, and logs to
`omastorm/engine.log`. `ensure` starts a background daemon and waits up to
10 seconds for its hello. Concurrent launches share it; it outlives windows.

Hello includes the PID, protocol version, and executable fingerprint.
`ensure` replaces a daemon whose build or protocol differs, and clients
reconnect. `stop` ends the answering daemon and waits up to two seconds for
its socket and lock to be released. With no daemon, it exits successfully
without creating files. Use `stop` instead of killing the daemon.

The server uses a current-thread Tokio runtime, with reader and writer tasks
per client. Startup archive decoding precedes the runtime; tile rendering and
live texture encoding use the blocking pool. Launcher operations use standard
sockets. A stalled client is disconnected after its eight-message output queue
fills or its write exceeds two seconds.

Textures are written, synced, and renamed to unique paths. Cleanup checks
state references once a second and removes textures unreferenced for 30 seconds.
The grace period starts when observed, so a restart preserves recently served
textures. Transport, commands, state, and texture encoding are defined in
[docs/protocol.md](../docs/protocol.md).

## NEXRAD

`src/sweep.rs` decodes Level II with the pinned `nexrad-data`, `nexrad-decode`,
and `nexrad-model` dependencies. It reads through the lowest cut, sorts rays
stably by azimuth, and publishes a polar sweep texture and azimuth lookup.
The UI samples these directly; radar arrays never enter JSON or QML JavaScript.

Fetch is [docs/radar-fetch.md](../docs/radar-fetch.md). `src/live_index.rs`
reads the last archive volume header, then lists the slots after it in the
chunk bucket. `src/live.rs` polls dated chunks, replays the current
volume's lowest cut, and assembles incoming radials.
Each chunk that grows the cut publishes a partial frame; the cut's final
radial or the next cut completes it. Gaps beyond 0.75° from any ray remain
blank. Selecting another station cancels the poller and discards its late events.
A background backfill fetches up to twelve earlier volumes, skipping cached ones.
Backfill reads the day's archive listing and each file's header for its slot.
SAILS and MRLE extra low-level cuts are not separate frames.

The poller bounds requests with timeouts, a streamed byte cap on each body,
and retries with backoff. Four
failed chunk fetches without a chunk in between restart discovery, and two
report offline; a connection reset while connecting counts toward the
restart but not toward offline. An empty listing is normal between chunks,
but 90 seconds without a recent chunk (higher cuts of a live volume still
arrive every 4–12 s) restarts discovery. The poller ignores leftover chunks
from an earlier volume cycle on both join and transition, and backfill uses
only the matching dated generation. Independently, the engine
respawns a poller whose task has exited, or whose newest radial is thirty
minutes old and has not been rediscovered since. A rediscovery that finds
only a sweep already in the catalog does not republish it or clear
unavailable. Reselecting the live station is a no-op while the poller is
running; if the task has ended, the reselect starts it again. A reachable
feed becomes stale at ten minutes and unavailable at thirty minutes without
new radials; an empty station is
unavailable, and an unreachable bucket is offline. Cached frames remain
usable under every condition.

`src/catalog.rs` stores the newest 60 complete frames per station from the
last two hours in `$XDG_CACHE_HOME/omastorm/frames/`: a SQLite WAL catalog
and PNG files. Older frames are evicted, files included, when a station is
selected and after each complete frame.
Entries retain scan geometry, times, and source provenance. The UI never reads
this store. The timeline serves cached frames through new runtime textures.
Playback loops complete frames over about ten seconds, bounded to 250 ms–1 s
per frame. New live sweeps take the screen only while the newest entry is selected.

The station table's source, retrieval date, and caveats are in `data/sites.json`
and hello. It includes archived and test sites; membership does not imply live
availability. An archived scan retains its measured coordinates.

## European mosaic

`src/opera.rs` fetches EUMETNET OPERA maximum-reflectivity composites from
Open Radar Data's public 24-hour cache. It lists COMP DBZH GeoTIFF objects,
loads the newest frame, and backfills earlier frames. History is capped at
12 frames. `src/cog.rs` decodes the raster; the UI samples its native grid.
Mosaic frames are complete images, with no polar sweep or elevation selector.
Source selection and the grid contract are in
[docs/grid-adapters.md](../docs/grid-adapters.md).

## METAR

`src/metar.rs` answers `metar_query` with airport observations around the
given lat/lon (the selected radar). NOAA/NWS Aviation Weather Center JSON
is fetched with the same bounded HTTP pattern as OSM tiles (timeout, body
cap, User-Agent, four in flight, 30-second backoff after a 429, a 5xx, or
a transport failure). Coverage is US, Canada, Hawaii, Guam, and Puerto
Rico / USVI, not the coarse NEXRAD clip (RKJK and LPLA do not fetch). A
query whose radar sits outside that area (OPERA Europe) is a no-op: empty
`metars`, no fetch. A newer query from the same client drops an older
reply. Results keep ICAO `K`, `C`, `P`, `TI`, `TJ`, and `M`. The default
is the nearest stations, at most 16, inside a 250 km circle. Optional
`pick=priority` ranks AWC `stationinfo` priority (1 is a hub) inside the
view box the UI sends; a stationinfo failure is a fetch failure. `limit`
shrinks the pool; `always_on` pins listed ICAO ids first when they are in
that pool. The reply keeps the raw METAR and FAA flight category only; a
station with no category is omitted, so the UI draws no chip. It does not
decode English. The fetched feed is cached for ten minutes and until the
UTC hour rolls. A view box is fetched with a quarter-view margin, so a
later query inside a cached box (a toggle, a slightly moved view) does not
fetch; a wider one does. A backoff still serves the latest feed for that
radar. Station priorities cache for a day. HTTP 204 is an
empty result.
`OMASTORM_METAR_URL` / `OMASTORM_METAR_FIXTURE` and
`OMASTORM_STATIONS_URL` / `OMASTORM_STATIONS_FIXTURE` override the
endpoints for checks. Nothing is fetched until a client asks. The UI never
reads this cache. The engine never reads `config.toml`.

## Basemap

`build.rs` converts Natural Earth lines to a compact polyline blob and embeds
populated places for map labels. GeoNames cities with population ≥ 5000,
clipped to the same envelope, are the location-picker gazetteer. The 1:50m
set is global; the 1:10m set is clipped to the compiled live-source envelope
(`src/envelope.rs`). `src/tiles.rs` rasterizes these with `tiny-skia`,
using 1:50m below z5 and 1:10m from z5. Segments outside a tile are skipped.

`src/osm.rs` serves OpenMapTiles vector data from z7 through z14, with Natural
Earth fallback. `OMASTORM_TILES_URL` overrides the default OpenFreeMap TileJSON
URL for development. Requests use a ten-second timeout and at most four
concurrent fetches. Transport errors, 429s, and 5xx responses trigger a
30-second backoff; cached tiles still serve.

Vector tiles persist under
`$XDG_CACHE_HOME/omastorm/vt/<source>/<version>/<z>/<x>/<y>.pbf`.
The two newest data versions are retained. Above 512 MB, eviction removes the
least recently read tiles until usage falls below 448 MB. Rendered masks are
runtime files capped at 4,096; their names include build/data generation tags.
The protocol documents mask channels, labels, attribution, and path validation.

## Verification

Required checks and capture commands are in
[CONTRIBUTING.md](../CONTRIBUTING.md#verify-and-submit).

The decoder tests compare every moment byte, ray angle, timestamp, and gate
geometry with `golden/ktlx-20130520/`. Other tests cover partial sweep assembly,
catalog retention, deterministic tile rendering, cache eviction, protocol
validation, client isolation, daemon replacement, and texture retirement.
Recorded vector-tile fixtures have provenance in `tests/fixtures/vt/tiles.json`.

The rendering check compares all three treatments against the shader's sampling
rule replayed in Rust over golden codes. It checks default and zoomed views,
weak-return filtering, folded and below-threshold codes, and coverage edges.
Samples near gate or azimuth boundaries allow either neighbor to account for
GPU precision; other mismatches fail. A second test pans the full map by whole
3 px cells and checks that radar, tiles, and overlays shift together within
premultiplied rounding tolerance, without relaying out labels or crossing a
tile edge. Captures and validation reports are written to `review/`.
