# MRMS QC base decoding

`engine/src/mrms.rs` converts a NOAA MRMS CONUS
`MergedBaseReflectivityQC_00.50` gzip object into a complete `MosaicFrame`,
an RGBA8 class texture, and its observation time in Unix milliseconds.
The caller supplies the UTC timestamp parsed from the object name and the
engine's reflectivity palette and bounds. The decoder owns its input bytes
so large intermediates can be dropped promptly. It performs no retrieval
or persistence. Its adapter, `engine/src/mrms/live.rs`, registers it as the
manual-only `mrms-conus` source.

Measured data comes from the public
[NOAA MRMS S3 collection](https://noaa-mrms-pds.s3.amazonaws.com/):

```text
CONUS/MergedBaseReflectivityQC_00.50/YYYYMMDD/
MRMS_MergedBaseReflectivityQC_00.50_YYYYMMDD-HHMMSS.grib2.gz
```

## Live retrieval

Selecting MRMS in the existing picker locks it and centers on its contiguous-U.S.
domain. Startup follow, pans, unlock, and place selection never choose MRMS
automatically. The adapter displays the newest valid frame first, then fills
history newest-to-oldest. It performs no network work until explicitly selected.

Live discovery reads today’s UTC prefix first and publishes its newest valid
observation before consulting yesterday’s history. An empty current prefix or
a bounded fallback after corrupt first observations can consult yesterday for
live data. Historical discovery reads yesterday only when the accepted hour
crosses midnight; its failures cannot override a healthy live feed. Listings
strictly validate QC-base filenames, deduplicate timestamps, and sort them. Each listing is capped at 2 MiB and four pages per date; a truncated
result without a usable continuation token fails explicitly. Tokens are URL
encoded. Discovery is capped at 30 seconds; each object download has its own
30-second timeout and a 64 MiB allocation/byte cap. Live discovery is scheduled
every 30 seconds; history fills the time between those polls. A history GET
is limited to the remaining poll interval. An already running bounded decode
finishes before live work resumes; a second history job cannot jump ahead.

The reader tries at most three newest objects within the hour ending at the
newest listed observation. A bad newest object may fall back to an earlier
valid one. Already retained observations are not downloaded again, including
after a poll restart, and older replies cannot move the clock backward.
Empty feeds without an accepted frame report unavailable;
unreachable/unreadable feeds report offline. Successful discovery of a known
frame recovers the connection status while the normal observation-age rules
still apply: stale after 10 minutes, unavailable after 30. Failed polls keep
the selection and available loop. Historical download, decode, and storage
failures are isolated from the live connection status.

History stores metadata and encoded texture paths for at most 30 unique
observations in `(newest accepted − 60 minutes, newest accepted]`. The exact
lower bound is excluded; gaps remain gaps. A faster cadence still keeps only
the newest 30. Both live and late backfill arrivals enforce this adapter policy
through the registry, without changing OPERA's count-only policy. The window
does not advance with wall-clock time during an outage. Backfill inserts frames
chronologically and preserves the selected frame; if that frame expires, the
existing timeline behavior selects the oldest retained one. Playback reuses
published texture paths rather than decoding measurements again.

A shared adapter permit serializes downloads and decoding across poll
restarts. Blocking decode owns that permit until it exits, even if its caller
is cancelled. Grid replies carry a poll-session identity, so switching away
and back rejects old frames and status reports. The shared result queue has
one slot; MRMS reserves it before starting work and waits for publication
acknowledgement before marking an observation accepted. Storage failures
therefore retry the same observation on the next poll.

MRMS runtime textures, including retired and temporary files, have a 1 GiB
budget with 32 MiB reserved for an in-flight encoded result. Publication fails
before exceeding it, preserving existing data. Normal reference tracking and
the 30-second retirement grace period own deletion. The native footprint is
explicit in the map/search envelope and lies entirely inside its previous
coarse NEXRAD extent. The adapter's automatic-selection eligibility is false;
coverage remains available to the picker and lock indicator.

## Accepted profile

This is a source-specific reader, not a general GRIB implementation.
It uses the existing Rust `flate2` and `png` dependencies with no native
GRIB library. Unexpected profiles return errors so a caller can retain
previous valid frames instead of silently reinterpreting changed data.

| Field | Accepted value |
| --- | --- |
| Container | One gzip member; one GRIB edition 2 message; exact total length |
| Sections | 0, 1, 3, 4, 5, 6, 7, terminal `7777`; no local sections or extra fields |
| Product | Discipline/category/parameter 209/11/0 |
| Origin | Center 161, subcenter 0, master table 255, local table 1 |
| Time | Significance 3 (observation); valid UTC seconds matching the object name |
| Status/type | Production status 2, processed radar observations 7 |
| Product template | 4.0, no coordinates; generating process 8/0/97; zero cutoff and forecast lead, time unit 0 |
| Level | Surface 102, scale 0, value 500 m MSL; no second surface |
| Grid template | 3.0, source 0, no optional point list, earth shape 2 |
| Geometry | 7000 columns × 3500 rows, 24,500,000 cells, 0.01° increments |
| Angles | Basic angle 1 / subdivisions 1,000,000, or default 0 / missing subdivisions |
| Order | West to east, north to south; scan flags 0, resolution flags 48 |
| Centers | Northwest 230.005°, 54.995°; southeast 299.995°, 20.005° |
| Packing | 5.41, matching value count, unsigned 16-bit samples, original field type 0 |
| Bitmap | 255 (none) |
| PNG | Grayscale16, noninterlaced, matching dimensions; only IHDR, IDAT, IEND |

The observed last-center fields differ from arithmetic by up to 2
microdegrees. The reader allows 3 microdegrees of endpoint rounding and
keeps the canonical affine; it does not stretch the grid to those fields.
Shape 2 defines its own axes, so unused explicit-axis fields are ignored.
The scale/value fields of an absent second surface are also ignored.

The profile follows NOAA's [product table](https://www.nssl.noaa.gov/projects/mrms/operational/tables.php),
[section 1](https://www.nco.ncep.noaa.gov/pmb/docs/grib2/grib2_doc/grib2_sect1.shtml),
[grid template 3.0](https://www.nco.ncep.noaa.gov/pmb/docs/grib2/grib2_doc/grib2_temp3-0.shtml),
[product template 4.0](https://www.nco.ncep.noaa.gov/pmb/docs/grib2/grib2_doc/grib2_temp4-0.shtml),
and [packing template 5.41](https://www.nco.ncep.noaa.gov/pmb/docs/grib2/grib2_doc/grib2_temp5-41.shtml).
Both verified public samples carry production-status code 2 (research
products); requiring code 0 would reject them. The filename's `00.50`
describes height metadata, not a dish tilt.

## Values and display

The physical value is `(R + X * 2^E) / 10^D`, using the parsed IEEE reference
and GRIB sign-magnitude scale exponents. E and D must lie in −32..32 and
the reference and resulting values must be finite. PNG samples stay
unsigned big-endian 16-bit values; no gamma or color transformation applies.

NOAA defines −99 dBZ as missing and −999 as no coverage. These remain
distinct decoded states until both deliberately map to the existing grid
texture's missing flag: `[R=0, G=1, B=0, A=255]`. Neither means undetect.
All other values, including negative measurements, use the supplied
`[lo, hi)` palette bins, clamped at the extremes. Measured pixels contain
`[class+1, 0, 0, 255]`. No weak-echo floor is added.

Frames preserve actual observation seconds, use an `mrms-conus-<UTC>`
identity, and name the product `QC Base Reflectivity` in dBZ. They carry
no site, dish elevation, or polar range geometry.

## Coordinate normalization

The native pixel-corner affine is `[-130, 0.01, 0, 55, 0, -0.01]` in
longitude/latitude degrees. The northwest cell center is
−129.995°, 54.995°; the southeast center is −60.005°, 20.005°.
There is no row reversal, resampling, or half-cell shift.

GRIB earth-shape code 2 is the
[IAU 1965 spheroid](https://www.nco.ncep.noaa.gov/pmb/docs/grib2/grib2_doc/grib2_table3-2.shtml),
not WGS84. This decoder applies a **provider-specific normalization** to
NOAA's independently declared WGS84 GIS grid. NOAA identifies `BREF_QCD`
as the matching quality-controlled base-reflectivity GeoTIFF product in
[SCN 23-61](https://www.weather.gov/media/notification/pdf_2023_24/scn23-61_radar_change_aac.pdf).

Two same-time GRIB/GeoTIFF pairs were inspected: 2026-09-26 18:21:39 and
05:02:10 UTC. Both GeoTIFFs declare EPSG:4326, WGS84 ellipsoid,
PixelIsArea, 0.01° pixels, and the same northwest corner and dimensions.
Across all 49 million cell positions, every distinct GRIB value mapped
to a consistent GeoTIFF color at the same cell, with zero disagreements.
The GeoTIFFs are precolored RGBA imagery, used only to corroborate placement.

This evidence supports reproducing NOAA's declared GIS placement at the
delivered grid resolution. It does not establish a general IAU1965 datum
transformation or finer geodetic accuracy. The frame therefore uses
`Crs::wgs84_geographic()` with no Helmert transform, only after validating
this source identity and grid. A changed profile requires new verification.

The first pair's primary artifacts are the
[GRIB object](https://noaa-mrms-pds.s3.amazonaws.com/CONUS/MergedBaseReflectivityQC_00.50/20260926/MRMS_MergedBaseReflectivityQC_00.50_20260926-182139.grib2.gz)
and [BREF_QCD GeoTIFF](https://mrms.ncep.noaa.gov/data/RIDGEII/L2/CONUS/BREF_QCD/CONUS_L2_BREF_QCD_20260926_182139.tif.gz).
GeoTIFF retention is limited; later verification can use a fresh matched pair.
Compressed SHA256 checksums, in timestamp order:

| UTC time | GRIB gzip SHA256 | GeoTIFF gzip SHA256 |
| --- | --- | --- |
| 18:21:39 | `b53518085eabd62503cb5540e0e7da6ca66b9f9cabea0b2b7a0de3cdebc6ae16` | `4e525d675b04f27e9b6e1df25a803344c5b7f14eb050af1967f37a1e25ae8fcc` |
| 05:02:10 | `a44ab7240513c02c20737751be797694675c52b1e42f67058514ea4152f3f6d1` | `a3dbeaa033c8ac566058c5133636763c3ed3dbb9745ae9eb1fcb75f6b166f734` |

## Bounds and validation

Compressed and inflated bodies are each capped at 64 MiB. Grid and PNG
dimensions are checked before raster allocation. A 65,536-entry lookup
table classifies samples row by row into one 98,000,000-byte RGBA plane;
there is no full uint16 or float raster allocation. PNG internal allocations
have a 16 MiB library budget. Ancillary chunks, including compressed
metadata and animations, are rejected before library decoding.

Gzip CRC/length, PNG critical-chunk CRCs, zlib checksum, exact filtered-byte
count, and stream endings are checked. A bounded preliminary zlib pass
enforces the strict ending rules that the PNG pixel reader intentionally
relaxes. The output PNG streams through a 32 MiB capped writer, with a
64 KiB chunk buffer, so even intermediate compressed output stays bounded.
Compressed input is dropped before classification, and GRIB before encoding.
The caller must serialize full-size decodes when integrating retrieval.

Ordinary tests are offline and synthesize all inputs, including a full-size
constant CONUS field. They cover scale/sign/endian handling, sentinel and
palette semantics, geography, metadata, corrupt/truncated containers,
unsupported PNG variants, and allocation/expansion limits:

```sh
mise exec -- cargo test --offline --locked mrms::tests
mise check
```

The ignored runtime smoke test uses an explicitly supplied local object.
For the first verified sample:

```sh
OMASTORM_MRMS_SAMPLE=/tmp/omastorm-mrms-qcbase.grib2.gz \
OMASTORM_MRMS_STAMP=20260926-182139 \
OMASTORM_MRMS_OUTPUT=target/research/mrms-decoder \
mise exec -- cargo test --offline --locked --release --bin omastorm-engine \
  mrms::tests::mrms_runtime_sample -- --ignored --exact --nocapture
```

It reports decode/classify/encode elapsed time. The optional output directory
receives `frame.json`, the encoded `texture.png`, and a palette-colored
`radar.png` for inspection. Preview generation and file writes are excluded
from decoder timing. For isolated peak RSS, build the test binary first,
then run it directly under `/usr/bin/time -v` with the same test arguments
and sample variables, omitting `OMASTORM_MRMS_OUTPUT`. The reference-machine
acceptance targets are at most 2 seconds and 384 MiB incremental memory
per full decode. Provider payloads and generated images stay outside git.
