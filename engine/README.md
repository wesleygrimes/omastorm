# Engine

From a fresh checkout, run `bash scripts/setup-fixture.sh` once to download
the Natural Earth files the binary embeds and the archived Level II volume the
tests and `OMASTORM_ARCHIVE` read (`data/raw/`
is ignored; `build.rs` stops with a message naming the script when one is
missing), then explicitly fetch/build dependencies with `bash scripts/cargo.sh
build --locked`. Run `bash run.sh` thereafter; launch builds offline, ensures a
shared daemon exists, and starts Quickshell. Rust 1.89+ is required for a
checkout build. Plugin installs use `scripts/install-engine.sh` and the pin
in `engine/release.pin` instead of Rust; `bash scripts/build-engine-release.sh`
produces a native x86_64 or aarch64 asset under `target/dist/` and does not
publish it (the pinned release holds the current file).
`scripts/cargo.sh` uses Cargo on PATH or, if present, an isolated
`.tools/{cargo,rustup}` toolchain. No Python runs at launch.

### Native engine releases

Run `bash scripts/build-engine-release.sh` on Linux x86_64 or ARM64 after
fetching the build dependencies and fixture data. It explicitly targets the
Rust host and emits `omastorm-engine-<host>` plus `SHA256SUMS`. If both native
assets are collected in `target/dist/`, the checksum manifest covers both.
Cross-compilation is not configured by this script.

`--write-pin` prepares the host's pin: `engine/release.pin` for x86_64,
`engine/release-aarch64.pin` for ARM64. It never overwrites the other
architecture's pin. Publish the exact asset on the named upstream engine
release before committing its pin. The installer checks both the architecture
in the asset name and its SHA256. An unpublished ARM64 pin is intentionally
absent; source builds work while an upstream ARM64 release is being prepared.

The engine embeds `data/fixture.json` and `data/sites.json` and nothing
archived. `fixture.json` is the frame template (product, palette, bounds,
protocol v1 shape) every live frame is built from. A shipped daemon starts
with no frame and goes live on the first `select_site`; with
`OMASTORM_ARCHIVE=data/raw/KTLX20130520_201643_V06.gz` (development, the
checks, the captures) it decodes that volume at startup and shows it as
`archived`, everything geometric coming from the volume. Radar values reach the UI only as textures;
none are read or transmitted as numbers.

With an archive named, at startup `src/sweep.rs` decodes the volume's first elevation cut with
`nexrad-data` (archive reader only, no `aws` feature), `nexrad-decode`, and
`nexrad-model`, pinned at the release candidates current on crates.io on
2026-09-06. Records are decompressed one at a time and decoding stops at the
first radial of the next cut. Startup decode and texture encoding take about
0.3 s (logged to `engine.log`); the workspace `Cargo.toml` optimizes
dependency crates even in debug builds, without which PNG encoding alone took
over a second, and `ensure` allows 10 s before it gives up on a daemon. Rays are stably sorted by
azimuth, exactly as the pyart golden files were produced. The engine publishes
the sweep texture (`docs/protocol.md`: RGBA, gates × rays, R class+1, G status
bits, B raw code) as `frame.texture` and the 3600 × 1 azimuth lookup as
`frame.azimuthLut`, with `rays`, `gates`, `firstGateM`, and `gateSpacingM`
beside them; the UI shader does the polar-to-screen lookup per 3 px cell.

Basemap tiles (phase 3, `src/tiles.rs`; DESIGN.md, basemap tiles). `build.rs`
converts the eight Natural Earth line files in `data/raw/` (coastline, lakes,
country and state lines at 1:50m and 1:10m) into one polyline blob in
`OUT_DIR`: the 1:50m set whole, the 1:10m set clipped to the network envelope
(5–75° N, west of 20° W or east of 120° E, with one vertex kept past each
edge so lines leave the envelope rather than stopping short), coordinates
quantized to 1e-5° as zigzag varint deltas, plus the populated places as
JSON. It reruns only when an input changes and adds about 2 MB to the
binary. A `tiles_needed` command (an inclusive rectangle of at most 64 tiles
at one zoom up to 22) is answered to its sender alone, tile by tile,
centre-out, by a task per client: a tile rendered this session is announced
at once, another is drawn on tokio's blocking pool and published, then
announced with `tile_ready` carrying `set: "ne"`, `z`, `x`, `y`, the path
`tiles/ne/<z>/<x>/<y>-<gen>.png` (`<gen>` the first eight hex digits of the
build fingerprint; the daemon deletes other generations at startup and caps
the directory at 4,096 files, oldest announced first), and `labels`, the
Natural Earth places inside the tile whose `min_zoom` is at most `z + 1`. A
newer request from the same client supersedes its pending tiles outside the
new rectangle. Rendering is `tiny-skia` 0.12 without its PNG feature: the
1:50m set below z5 and the 1:10m set from z5, each layer stroked with round
caps and joins into an antialiased coverage mask (boundaries 1.5 px into R,
coast and lake shores 1.25 px into G, widths accepted as a starting point on
the tile captures; B stays zero and A is 255, since A is 255 minus
major-road coverage and roads exist only in `osm` data), interleaved into a
512 × 512 RGBA PNG. A is inverted because Qt premultiplies textures on
upload (DESIGN.md, basemap tiles). Segments that cannot reach the tile are skipped, so a
continent-long coastline costs its local part. The same tile always produces
the same bytes, so a concurrent render replaces a file with itself. In a
debug build the z1 world tile takes about 130 ms and a z5 tile about 25 ms.
The tile path rule (`tiles/<set>/<z>/<x>/<file>`) is checked before
publishing, as the texture rule is.

The `osm` set (`src/osm.rs`, since 2026-09-06). From z7 to z14 a requested
tile is first sought as an OpenMapTiles-schema vector tile: in the cache
`$XDG_CACHE_HOME/omastorm/vt/<source>/<version>/<z>/<x>/<y>.pbf` (`<source>`
an FNV tag of the configured TileJSON URL), else fetched with `reqwest`
(rustls, `User-Agent: omastorm/<version> (https://omastorm.com)`, 10 s
timeout, four in flight across all clients) and cached by temp-and-rename,
empty tiles included. The first fetch of a daemon's life reads TileJSON
(`https://tiles.openfreemap.org/planet` by default; `OMASTORM_TILES_URL`
overrides it for development until phase 4's `config.toml`) for the URL
template, the data version (the path segment before `{z}`), the source name,
and the attribution; the two newest version directories are kept. Launch
fetches nothing. A 429, a 5xx, or a transport failure backs the whole fetch
path off for 30 s and sets `state.basemap.osm.status` to `offline` (or
`unavailable` when no version is known yet); meanwhile cached tiles serve
and other tiles are answered as `ne`, then asked for again and announced as
`osm` once the back-off passes. Every read touches the file's mtime; after a
fetch batch that pushed the cache past 512 MB, the least recently touched
files go until it is under 448 MB. Decoding is `mvt-reader` 2.5: `boundary`
features with `admin_level` 2 or 4 and `maritime` 0 stroke R, `water`
polygon rings and `waterway` lines G, `transportation` secondary and
tertiary roads B (1.0 px), motorway, trunk, and primary roads A as 255 −
coverage (1.5 px), all through the `Strokes` rasterizer `ne` tiles use, which
also drops segments with both ends past one tile edge (where clipped polygons
run along the tile buffer). `place` cities, towns, and villages inside the
tile become the tile's `labels` (`name:en` over `name`, national capitals
classed `capital`, OpenMapTiles `rank` or 20 when unranked). `osm` masks are
named `tiles/osm/<z>/<x>/<y>-<gen>.png` with `<gen>` a tag over the build
fingerprint and the data version; the store forgets them at startup and on a
version change. Tiles deeper than z14 fall back to `ne`.

`XDG_RUNTIME_DIR` must be set to an absolute directory. The daemon owns
`omastorm/engine.sock` under it; `engine.lock` uses an OS file lock to serialize
startup and recover a stale socket after a crash. `ensure` starts `serve` in
the background and waits for its hello. Concurrent launches share one engine.
It stays running after the last window closes. Its hello includes PID and a
build fingerprint, a hash of the executable image taken once at startup, so any
rebuild that changes the binary counts as a new build. A daemon of another
build or protocol is replaced: `ensure` ends it, logs its PID to stderr, and
starts its own; open windows reconnect within a second (decided 2026-09-06;
until then the launcher refused it and the launch key stayed dead after every
rebuild until a manual stop). `stop` does the same on request: it reads the
hello of whatever daemon answers (any build), sends it SIGTERM, waits up to
2 s until the socket refuses connections and `engine.lock` is free, and
prints the stopped PID; with no daemon it exits 0 without output or creating
anything. The daemon has no signal
handler: the OS releases its lock and `serve` recovers the leftover socket file.
Daemon diagnostics go to `omastorm/engine.log`.

The daemon runs on a current-thread tokio runtime (since 2026-09-06, the
first task of phase 3; DESIGN.md, engine runtime): `tokio::net::UnixListener`,
a reader task and a writer task per client, and the texture cleanup on a
`tokio::time` interval. Startup decoding and publishing stay synchronous and
run before the runtime starts. Tokio is built with `rt`, `net`, `time`,
`sync`, and `io-util` only (the last holds the async read and write traits).
The launcher paths, `ensure` and `stop`, stay on std sockets.

Textures are published by write/sync/rename to unique revision paths. A cleanup
task reads the paths the current state references once a second and retires
any other file under `tex/` 30 seconds after it was first seen unreferenced.
The clock starts at observation, not at the file's mtime, so a file left by an
earlier daemon counts from this daemon's start and a texture served immediately
before a crash survives the restart. The current texture is never removed. A
stalled client has a bounded output queue (eight messages) and a 2 s write
timeout, after which its connection is closed; malformed/unknown commands
cannot terminate the daemon.

State is a set of serde structs in `src/protocol.rs` mirroring
`docs/protocol.md`; each command handler reports whether it changed anything,
and only a change is broadcast. Commands parse into a tagged enum; a known
command with a bad field is answered with an `error` event to its sender only,
an unknown type is logged. State carries no error field: lasting conditions
live in `connection`, and no client command can clear them.

The timeline (`Timeline` in `main.rs`, since 2026-09-07) is the selected
station's catalogued frames oldest first plus the sweep in progress as a
`partial` entry; the archived fixture is a one-frame timeline. `step` and
`seek` pause playback and show the frame they land on, read back from the
catalog (or from memory for the sweep in progress) and republished under new
`tex/` revisions with the frame's real `scanTime`; a seek outside the
timeline answers the sender with an `error`. `play` advances one complete
frame per 250 ms and loops, on a task woken by the command; it changes
nothing with fewer than two complete frames. A live sweep replaces the
frame on screen only while the newest entry is shown; once a client stepped
back, sweeps extend the timeline until a step or seek lands on the newest
entry again. Follow/lock flags are shared, but view-center auto-selection
waits for its later session. Age is computed at each snapshot, and while
live the daemon re-judges once a second and broadcasts only when the state
text changed.

Live data (phase 4, `src/live.rs`, since 2026-09-06). `select_site <ID>` for
a station in the table goes live: the station's newest frame from the
catalog (below) or a one-row placeholder that draws nothing is published at
once under `connection.status: loading`, the previous station's poller task
is aborted, and a new one starts. The poller is `nexrad-data`'s pull-based
`ChunkIterator` over the `unidata-nexrad-level2-chunks` bucket (the crate's
`aws` feature; the same `reqwest`/rustls stack the tile fetcher uses): it
finds the latest volume, then replays that volume's lowest cut by
downloading the Start chunk and every earlier chunk the VCP maps to
elevation 1 (up to twelve blindly when the VCP cannot be read), and from
then on asks for the next chunk, sleeping the iterator's estimate clamped to
1–10 s (2 s without one) when it is not there yet. The iterator enters the
next volume at its newest chunk, so a poll that arrives after more than the
Start chunk landed fetches the skipped ones the same way first, and the
assembler treats any chunk of a new volume as its beginning. Every call sits under a
30 s timeout (60 s for discovery); a failure reports `offline` and backs off
5 s, four in a row restart from discovery, and discovery failures back off
5 s doubling to 60 s. Discovery finding no volume for the station at all is
the bucket's answer, not a failure: it reports `unavailable` and retries on
the same back-off. Once a second `main.rs` re-judges a reachable feed from
the newest radial received: `ok`, `stale` at 10 min, `unavailable` at
30 min. Each chunk's radials feed an `Assembler` that keeps the
radials of elevation number 1 in arrival order; the cut ends when a radial
says `ElevationEnd` or the next cut's first radial arrives, and a Start chunk
begins a new volume. Every chunk that grows or ends the cut becomes an event;
`main.rs` encodes the sweep texture and lookup on the blocking pool (about
30–200 ms for 120–720 rays in a debug build), publishes them under new
revision paths, and broadcasts the frame as `partial` or `complete`. A sweep
with a gap carries one blank row that the lookup names for azimuths farther
than 0.75° from any ray (`src/sweep.rs`, `GAP_DEG`), so a sweep in progress
paints only where the antenna has been. Events for a station that is no
longer selected are dropped. The station's coordinates come from the table;
scan times from the radials. SAILS and MRLE cuts (extra low-level sweeps
mid-volume) are not yet separate frames.

The frame catalog (`src/catalog.rs`) is the per-station ring buffer:
`$XDG_CACHE_HOME/omastorm/frames/catalog.sqlite` (rusqlite, bundled SQLite,
WAL) with one row per complete frame (id, station, product, elevation, start
time, scan and sweep-end strings, provenance naming the bucket, volume, and
first and last chunk, stored time, and the frame's JSON without runtime
paths) and the sweep and lookup PNGs beside it under `<SITE>/`, written by
temp-and-rename. Each store keeps the newest 60 frames per station and
deletes the rest with their files. The UI never reads it; the timeline
serves stepped frames from it.

Live mode is the only network use besides tiles, and nothing starts it but
a client's `select_site`; launch still fetches nothing. The window selects
`$OMASTORM_SITE` when its state arrives (a development hook until the picker
and `config.toml` land). Nothing in the test suite selects a table station.

The station table is a 2026-09-05 snapshot of [NOAA NCEI HOMR](https://www.ncei.noaa.gov/access/homr/file/nexrad-stations.txt),
using its [fixed-width layout](https://www.ncei.noaa.gov/access/homr/file/NexRad_Table.txt).
All 163 NEXRAD rows are retained, including archived/test sites; TDWR rows are
excluded. Availability is not implied. Elevation is ground + tower + feed horn,
converted from feet to meters by 0.3048; overseas state fields remain empty.
The archived frame retains its own measured site geometry, not today's table.
Source URL, date, and caveats are embedded and exposed with hello.

Checks (socket and GPU checks need desktop access outside the sandbox):

```sh
bash scripts/cargo.sh test --offline --locked
bash scripts/cargo.sh test --offline --locked -- --ignored rendering   # GPU, needs Quickshell
bash scripts/cargo.sh clippy --offline --locked --all-targets -- -D warnings
bash scripts/cargo.sh fmt --check
bash scripts/check-engine-ui.sh
bash scripts/check-map-tiles.sh              # delayed zoom tiles and ranked labels
bash scripts/check-map-sites.sh              # site overlay geometry, layout, and pan
bash scripts/capture-review.sh
```

Install, launch, and every check need no Python.

`tests/rendering.rs` is the GPU rendering check, ignored by default because it
needs Quickshell and a desktop OpenGL context. Its first test starts a daemon
in a private runtime directory (private cache, tile URL pointing at a refused
loopback port, so nothing fetches), copies `ui/RadarMap.qml` into a harness
with its shader item exposed, renders the map alone at 900 × 420 through
Quickshell offscreen (RHI OpenGL) and grabs the shader item to a PNG, for
each treatment at the default view and at span 30 over Moore. Expectations replay the shader's rule in Rust:
3 px cell center in Web Mercator to a latitude and longitude difference from
the site, great-circle distance and bearing on the 6371 km sphere, ground
distance to slant range on the 4/3 earth, nearest gate with half-gate margins,
row from the azimuth lookup
the daemon published, class from `bounds`; the classes come from the golden
codes in `golden/ktlx-20130520/sweep0.u8`, and the daemon's texture is checked
against them channel for channel on the way. Alpha must match within 1/255
everywhere, opaque pixels carry the swatch exactly, and partially covered
stipple pixels carry it within Qt's premultiplied rounding. A pixel whose sample
sits within 0.03 gate or 0.005 entry of a boundary may show either neighbour
(GPU transcendental precision), and is counted separately as `boundary` in the
report; anything else is a mismatch and fails. Three more captures render a
hand-built eight-sector sweep through a harness-only frame: classes 1 and 12,
folded (the two-tone X), below threshold, two middle groups, a per-gate class
ramp, a class past the palette, and an outside-coverage status, with the view
reaching past the last gate. Captures land in `review/render-*.png` and
`review/folded-*.png`, the report in `review/render-validation.json`.

The second test (`rendering_pans_radar_tiles_and_overlay_together`, phase 4,
the replacement for the Python profiler's pan check) drives the whole map
through the real socket client: 960 × 680 in Glyphs at span 560 km (z6, so
the tiles are `ne` and nothing fetches), waits until every tile of the
requested rectangle is drawn at its level and both radar textures are
uploaded, grabs the map with tiles and overlay, pans the camera by 48 px
east and 27 px north (whole 3 px cells, so the shader samples the same
gates), and grabs again. The second capture must equal the first shifted by
the pan everywhere inside the overlap shrunk by 128 px (labels near an edge
appear and vanish with the viewport's own clipping rule), within
premultiplied rounding (one unit of alpha; of colour, one unit plus `256 /
alpha`, since the GPU blends at a new offset); anything larger fails, and a
layer that lagged would show as whole rows of misplaced content. The
harness also reports whether the pan re-laid out the labels or crossed a
tile edge, both of which fail. The region must hold radar swatches, basemap
or overlay pixels, and content that moved. Captures are
`review/pan-before.png` and `review/pan-after.png`, the report
`review/pan-validation.json`. Quickshell 0.3.1 logs to stdout when it is
not a terminal, so both harnesses capture stdout and stderr together (the
log file was empty before 2026-09-06 and the timeout check vacuous).

`src/osm.rs` carries the `osm` tests against two tiles recorded from
OpenFreeMap on 2026-09-06 (`data/vt/`, provenance in `tiles.json`; the daemon
never reads them): the z7 and z11 tiles holding KTLX render deterministically
with water in G, interstates in A with partial coverage at their edges (the
premultiplication case the UI shader divides out), secondary roads in B at
z11, no admin boundary (both tiles lie inside Oklahoma), and sixteen labels
at z7 including Oklahoma City as a rank 4 city and Moore as a town; an empty
tile is opaque and blank and garbage is refused; TileJSON parsing yields the
version, template, name, and attribution text; eviction drops the least
recently touched files and version pruning keeps two. They write
`review/tile-ktlx-z7-osm*.png` and `review/tile-ktlx-z11-osm*.png`. Nothing
in the suite reaches the network.

`src/tiles.rs` carries the tile tests: the embedded blob decodes to the
expected vertex counts with every 1:10m vertex in or next to the envelope;
tile math (the tile holding KTLX at z5 is 7/12, the Mercator limits, centre-out
order, request validation); and a render check that draws the world at z1 and
KTLX at z5 and z8 twice each, asserts B is zero and A 255 everywhere, R and G
non-zero where boundaries and coasts cross (z1 and z5; the z8 tile around
KTLX holds no Natural Earth feature, which is why `osm` starts at z7), edge
pixels with intermediate coverage, and byte-identical renders, and writes
`review/tile-<name>.png` (the raw masks) and `review/tile-<name>-preview.png`
(the masks tinted over an opaque ground, since the raw masks read as a red
and green blur in a viewer). The store's cap and naming are unit-tested. The integration suite
sends `tiles_needed` for the four z5 tiles around KTLX and checks four
`tile_ready` replies with valid paths, 512 × 512 RGBA files, and labels; a
repeat answered under the same names; rejections for too many tiles, an
inverted rectangle, a column past the pyramid, and a missing field; and that
the other client hears none of it. The UI check (`scripts/check-engine-ui.sh`)
applies the tile path rule on the client and asks the real daemon for the
four z5 tiles around KTLX.

`src/sweep.rs` carries the golden test: it hashes the fixture, decodes it, and
compares ray count, gate geometry, scale and offset, every azimuth and
elevation (to the four decimals the golden file holds), every ray time
relative to the cut's first radial, and every moment byte against
`golden/ktlx-20130520/sweep0.{json,u8}`; a second test checks the texture
channels, the lookup's nearest-ray rule, and a PNG round trip; a third keeps
the fixture's rays between 30° and 120° and checks the blank row and which
lookup entries name it. `src/live.rs` feeds the fixture's 8,280 radials to
the assembler in slices of 100 (the archive is one uncompressed record, so
records cannot stand in for chunks) and checks the partial sizes, the
complete sweep against the fixture decoder, a new volume discarding the
cut, completion by the next cut when the last radial is missing, and that
the whole file decodes as a Start chunk. `src/catalog.rs` fills a station's
ring past its size and checks what stays, the files, another station's
independence, and reopening.
The Rust tests also cover real sockets, two-client broadcasts, fragmented and invalid
commands, size limits, duplicate daemons, replacement of a stale build by
`ensure`, `stop` of a live daemon, `stop` with no daemon, crash recovery, unique textures, the
30-second retirement grace period (unit tests for the reference clock, an
integration test against a restarted daemon), byte-exact PNG publication, and
site metadata.
The UI check verifies socket metadata, invalid JSON, a rejection that sits
beside state until the client's next command (including one answered by the
real daemon), the azimuth lookup and tile path rules, a `tiles_needed` round
trip, and rejecting an unknown protocol version.
