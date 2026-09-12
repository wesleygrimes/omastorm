# Engine

Build and run with [CONTRIBUTING.md](../CONTRIBUTING.md). Distribution and
versioning are in [docs/RELEASING.md](../docs/RELEASING.md).

## Architecture

The binary embeds Natural Earth geography, `data/sites.json`, and
`data/fixture.json` (the product, palette, and frame template). It embeds no
archived radar. A daemon starts with no station; `select_site` starts live
polling. An `OMASTORM_ARCHIVE` scan is decoded at startup and labeled archived.
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

## Radar

`src/sweep.rs` decodes Level II with the pinned `nexrad-data`, `nexrad-decode`,
and `nexrad-model` dependencies. It reads through the lowest cut, sorts rays
stably by azimuth, and publishes a polar sweep texture and azimuth lookup.
The UI samples these directly; radar arrays never enter JSON or QML JavaScript.

`src/dwd.rs` decodes DWD DX single-site sweeps (0.8°, 360x128, RLE uint16
over HTTPS) into the same `Sweep` by mapping dBZ onto NEXRAD-style codes, and
polls each station's `-latest-` file with a listing backfill. No new
dependencies; DX frames are always complete and never range-folded.

`src/live.rs` polls the real-time chunk bucket through `ChunkIterator`,
replays the current volume's lowest cut, and assembles incoming radials.
Each chunk that grows the cut publishes a partial frame; the cut's final
radial or the next cut completes it. Gaps beyond 0.75° from any ray remain
blank. Selecting another station cancels the poller and discards its late events.
A background backfill fetches up to twelve earlier volumes, skipping cached ones.
SAILS and MRLE extra low-level cuts are not separate frames.

The poller bounds requests with timeouts and retries with backoff. Four
failed chunk fetches restart discovery. `try_next` returning `None` is not a
failure, but 90 seconds with no chunk at all (higher cuts of a live volume
still arrive every 4–12 s) means the iterator is parked on a volume the
bucket has rotated off, so discovery starts over. Independently, the engine
respawns a poller whose task has exited, or whose newest radial is thirty
minutes old and has not been rediscovered since. A rediscovery that finds
only a sweep already in the catalog does not republish it or clear
unavailable. Reselecting the live station is a no-op while the poller is
running; if the task has ended, the reselect starts it again. A reachable
feed becomes stale at ten minutes and unavailable at thirty minutes without
new radials; an empty station is
unavailable, and an unreachable bucket is offline. Cached frames remain
usable under every condition.

`src/catalog.rs` stores the newest 60 complete frames per station in
`$XDG_CACHE_HOME/omastorm/frames/`: a SQLite WAL catalog and PNG files.
Entries retain scan geometry, times, and source provenance. The UI never reads
this store. The timeline serves cached frames through new runtime textures.
Playback loops complete frames over about ten seconds, bounded to 250 ms–1 s
per frame. New live sweeps take the screen only while the newest entry is selected.

The station table's source, retrieval date, and caveats are in `data/sites.json`
and hello. It includes archived and test sites; membership does not imply live
availability. An archived scan retains its measured coordinates.

## Basemap

`build.rs` converts Natural Earth lines to a compact polyline blob and embeds
populated places for map labels. GeoNames cities with population ≥ 5000,
clipped to the same envelope, are the location-picker gazetteer. The 1:50m
set is global; the 1:10m set is clipped to the station envelope (NEXRAD
network plus Germany). `src/tiles.rs` rasterizes these with `tiny-skia`,
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
Recorded vector-tile fixtures have provenance in `data/vt/tiles.json`.

The rendering check compares all three treatments against the shader's sampling
rule replayed in Rust over golden codes. It checks default and zoomed views,
weak-return filtering, folded and below-threshold codes, and coverage edges.
Samples near gate or azimuth boundaries allow either neighbor to account for
GPU precision; other mismatches fail. A second test pans the full map by whole
3 px cells and checks that radar, tiles, and overlays shift together within
premultiplied rounding tolerance, without relaying out labels or crossing a
tile edge. Captures and validation reports are written to `review/`.
