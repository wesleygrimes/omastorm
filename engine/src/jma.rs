//! Japan Meteorological Agency high-resolution precipitation nowcast
//! (高解像度降水ナウキャスト), observed frames only, as a GridFamily
//! precipitation-rate adapter (`docs/grid-adapters.md`). Live fetch only;
//! nothing from JMA is vendored.
//!
//! JMA publishes the product for its own web viewer as 256 px Web Mercator
//! tiles colored with an eight-band legend. Each exact legend color maps
//! back to its rain-rate band, so the texture carries classes in mm/h, never
//! a derived dBZ. A color outside the legend is missing, not a guess.
//! `hrpns_nd` is the same frame's no-data polygon (outside radar range or
//! an outage); it marks those cells missing and decides which tiles to
//! fetch at all.

use crate::{
    live_index,
    protocol::{
        AdapterTarget, Coverage, Crs, Ellipsoid, Family, FrameStatus, Kind, MosaicFrame,
        ProductClass,
    },
    source::{GridEvent, MosaicMeta, SourceMetadataBorrowed},
    sweep,
};
use chrono::{NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashSet,
    f64::consts::PI,
    future::Future,
    io::Read,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Semaphore, mpsc::Sender},
    task::{JoinHandle, JoinSet, spawn_blocking},
    time::{sleep, timeout},
};

pub const ID: &str = "jma";
pub const NAME: &str = "JMA Nowcast";
pub const ATTRIBUTION: &str = "Japan Meteorological Agency";
/// A national product. It ranks above a continental fallback of the same
/// product class, and any reflectivity mosaic still outranks it.
pub const SELECTION_PRIORITY: i32 = 50;

/// JMA's own web-viewer data root (`www.jma.go.jp/bosai/nowc/`). Not a
/// documented API: the paths are what that viewer requests.
pub const HOST: &str = "https://www.jma.go.jp/bosai/jmatile/data/nowc";
/// Observed frames, newest first. `targetTimes_N2.json` and `_N3` are the
/// forecast steps, which Omastorm does not show.
pub const INDEX: &str = "targetTimes_N1.json";

/// Service footprint from `envelope::JMA` (the extent of the product's
/// observed area).
pub const COVERAGE_NORTH: f64 = crate::envelope::JMA.north;
pub const COVERAGE_SOUTH: f64 = crate::envelope::JMA.south;
pub const COVERAGE_WEST: f64 = crate::envelope::JMA.west;
pub const COVERAGE_EAST: f64 = crate::envelope::JMA.east;

/// JMA renders this product at even zooms only (4, 6, 8, 10); odd zooms
/// answer with blank tiles. z6 is about 2 km a pixel at 35°N: 42 tiles
/// over the coverage box, of which the no-data polygon leaves about half
/// to fetch. z8 would be 0.5 km at sixteen times the requests and a
/// 5376 × 5888 texture per frame.
pub const ZOOM: u32 = 6;
pub const TILE_PX: u32 = 256;

pub const PRODUCT: &str = "RATE";
pub const PRODUCT_NAME: &str = "Precipitation rate estimate";
pub const UNITS: &str = "mm/h";
/// JMA's legend colors, class order (lightest rain first), as the tiles
/// carry them: `PLTE` entries 2–9 of the indexed tiles, and the same RGB at
/// alpha 255 in the RGBA ones. Only an exact match is a measured value.
pub const LEGEND: [[u8; 3]; 8] = [
    [0xf2, 0xf2, 0xff],
    [0xa0, 0xd2, 0xff],
    [0x21, 0x8c, 0xff],
    [0x00, 0x41, 0xff],
    [0xfa, 0xf5, 0x00],
    [0xff, 0x99, 0x00],
    [0xff, 0x28, 0x00],
    [0xb4, 0x00, 0x68],
];
/// JMA's bands in mm/h: under 1, 1–5, 5–10, 10–20, 20–30, 30–50, 50–80,
/// and 80 or more. The last edge closes the open top band for the wire
/// (`bounds` has one more entry than `palette`); the legend labels that
/// band `80+` and never shows it.
pub const BOUNDS: [f64; 9] = [0.0, 1.0, 5.0, 10.0, 20.0, 30.0, 50.0, 80.0, 200.0];
/// Engine-owned colors from the reflectivity ramp in `data/product.json`,
/// eight steps from its twelve: the picture keeps one visual language on
/// the dark themes. Ramp position only; no rain-rate/dBZ equivalence.
pub const PALETTE: [&str; 8] = [
    "#426b88", "#4098a5", "#51b897", "#85c76b", "#f0cd61", "#eda24c", "#d84c64", "#b55096",
];

/// Timeline depth: the same dozen as OPERA (an hour at JMA's five-minute
/// step). The index lists about three hours.
pub const HISTORY_MAX: usize = 12;
/// The index is served with `max-age=60`; polling faster only rereads the
/// CDN's copy.
const IDLE: Duration = Duration::from_secs(60);
const START_TIMEOUT: Duration = Duration::from_secs(45);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_DELAY: Duration = Duration::from_millis(500);
/// After the newest frame is on screen, pull earlier ones (OPERA and NEXRAD
/// wait the same beat so a hand-off mid-pan does not spend requests).
const BACKFILL_DELAY: Duration = Duration::from_secs(3);
/// Every JMA request of one poller, live and backfill together. Backfill
/// loads one frame at a time inside this.
const REQUESTS_IN_FLIGHT: usize = 4;
const INDEX_MAX: usize = 256 << 10;
/// Tiles are a few hundred bytes to a few KiB.
const TILE_MAX: usize = 512 << 10;
/// The no-data GeoJSON is about 170 KiB gzipped, 1 MiB as JSON.
const NODATA_MAX: usize = 8 << 20;
const NODATA_JSON_MAX: u64 = 64 << 20;

const EARTH_RADIUS_M: f64 = 6_378_137.0;

pub use crate::source::GridEvent as Event;

fn ev_frame(frame: MosaicFrame, texture: Vec<u8>, start_ms: i64) -> GridEvent {
    GridEvent::Frame {
        source_id: ID.to_owned(),
        frame: Box::new(frame),
        texture,
        start_ms,
    }
}

fn ev_backfill(frame: MosaicFrame, texture: Vec<u8>, start_ms: i64) -> GridEvent {
    GridEvent::Backfill {
        source_id: ID.to_owned(),
        frame: Box::new(frame),
        texture,
        start_ms,
    }
}

fn ev_offline(reason: String) -> GridEvent {
    GridEvent::Offline {
        source_id: ID.to_owned(),
        reason,
    }
}

fn ev_silent(reason: String) -> GridEvent {
    GridEvent::Silent {
        source_id: ID.to_owned(),
        reason,
    }
}

pub struct Jma {
    pub id: &'static str,
    coverage: Coverage,
}

impl Jma {
    pub fn new() -> Self {
        Self {
            id: ID,
            coverage: Coverage::Box {
                north: COVERAGE_NORTH,
                south: COVERAGE_SOUTH,
                east: COVERAGE_EAST,
                west: COVERAGE_WEST,
            },
        }
    }

    pub fn metadata(&self) -> SourceMetadataBorrowed<'_> {
        SourceMetadataBorrowed {
            id: self.id,
            family: Family::Grid,
            kind: Kind::Mosaic,
            default_product_class: ProductClass::PrecipitationRate,
            name: NAME,
            attribution: ATTRIBUTION,
            mosaic: Some(MosaicMeta {
                coverage: &self.coverage,
                selection_priority: SELECTION_PRIORITY,
                covering: true,
            }),
        }
    }

    pub fn loading_placeholder(&self) -> Option<(MosaicFrame, Vec<u8>)> {
        Some((loading_frame(), loading_texture().ok()?))
    }

    pub fn poll(
        &self,
        target: &AdapterTarget,
        events: Sender<Event>,
        known: HashSet<String>,
    ) -> Option<JoinHandle<()>> {
        match target {
            AdapterTarget::Mosaic => Some(tokio::spawn(poll_loop(events, known))),
            AdapterTarget::Site { .. } => None,
        }
    }
}

/// Web Mercator (EPSG:3857): the `mercator` CRS on a 6,378,137 m sphere.
/// An inverse flattening of 0 is a sphere, so WGS84 latitudes project as
/// they do on the basemap.
pub fn crs() -> Crs {
    Crs::Mercator {
        ellipsoid: Ellipsoid {
            semi_major_m: EARTH_RADIUS_M,
            inverse_flattening: 0.0,
        },
        lon0_deg: 0.0,
        scale: 1.0,
        false_easting_m: 0.0,
        false_northing_m: 0.0,
        datum_transform: None,
    }
}

fn palette() -> Vec<String> {
    PALETTE.iter().map(|&c| c.to_owned()).collect()
}

/// Shown the instant JMA is selected, the same job as `opera-loading`:
/// chrome stays up while the first frame is in flight.
pub fn loading_frame() -> MosaicFrame {
    MosaicFrame {
        id: format!("{ID}-loading"),
        product: PRODUCT.into(),
        product_name: PRODUCT_NAME.into(),
        units: UNITS.into(),
        scan_time: String::new(),
        sweep_end: None,
        status: FrameStatus::Partial,
        texture: String::new(),
        width: 1,
        height: 1,
        crs: crs(),
        geotransform: [0.0, 1.0, 0.0, 0.0, 0.0, -1.0],
        palette: palette(),
        bounds: BOUNDS.to_vec(),
    }
}

pub fn loading_texture() -> Result<Vec<u8>, String> {
    sweep::png(1, 1, &[0, 0, 0, 0]).map_err(|e| format!("encoding JMA placeholder: {e}"))
}

// ---- Tile geometry ---------------------------------------------------------

/// Web Mercator metres of a WGS84 longitude and latitude.
pub fn mercator_m(lon: f64, lat: f64) -> (f64, f64) {
    let x = EARTH_RADIUS_M * lon.to_radians();
    let y = EARTH_RADIUS_M * (PI / 4.0 + lat.to_radians() / 2.0).tan().ln();
    (x, y)
}

/// Metres across one tile at zoom `z`.
fn tile_span_m(z: u32) -> f64 {
    2.0 * PI * EARTH_RADIUS_M / f64::from(1u32 << z)
}

/// An inclusive block of XYZ tiles, stitched row-major into one raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRange {
    pub z: u32,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl TileRange {
    /// The tiles that hold a lat/lon box.
    pub fn covering(z: u32, north: f64, south: f64, west: f64, east: f64) -> Self {
        let n = f64::from(1u32 << z);
        let span = tile_span_m(z);
        let half = PI * EARTH_RADIUS_M;
        let (xw, yn) = mercator_m(west, north);
        let (xe, ys) = mercator_m(east, south);
        let tile = |m: f64| (m.floor().max(0.0) as u32).min((n as u32) - 1);
        Self {
            z,
            x0: tile((xw + half) / span),
            x1: tile((xe + half) / span),
            y0: tile((half - yn) / span),
            y1: tile((half - ys) / span),
        }
    }

    /// The compiled Japan raster.
    pub fn japan() -> Self {
        Self::covering(
            ZOOM,
            COVERAGE_NORTH,
            COVERAGE_SOUTH,
            COVERAGE_WEST,
            COVERAGE_EAST,
        )
    }

    pub fn columns(&self) -> u32 {
        self.x1 - self.x0 + 1
    }

    pub fn rows(&self) -> u32 {
        self.y1 - self.y0 + 1
    }

    pub fn width(&self) -> u32 {
        self.columns() * TILE_PX
    }

    pub fn height(&self) -> u32 {
        self.rows() * TILE_PX
    }

    /// Every tile, row by row.
    pub fn tiles(&self) -> Vec<(u32, u32)> {
        (self.y0..=self.y1)
            .flat_map(|y| (self.x0..=self.x1).map(move |x| (x, y)))
            .collect()
    }

    /// GDAL pixel-corner affine of the stitched raster in Web Mercator
    /// metres: origin at the top-left tile's north-west corner.
    pub fn geotransform(&self) -> [f64; 6] {
        let span = tile_span_m(self.z);
        let half = PI * EARTH_RADIUS_M;
        let pixel = span / f64::from(TILE_PX);
        [
            -half + f64::from(self.x0) * span,
            pixel,
            0.0,
            half - f64::from(self.y0) * span,
            0.0,
            -pixel,
        ]
    }
}

// ---- Frame index -----------------------------------------------------------

/// One observed frame from `targetTimes_N1.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetTime {
    /// `YYYYMMDDhhmmss` UTC as JMA spells it in paths.
    pub basetime: String,
    pub stamp: NaiveDateTime,
}

#[derive(Deserialize)]
struct RawTargetTime {
    basetime: String,
    validtime: String,
    #[serde(default)]
    elements: Vec<String>,
}

/// Parse the index, keep observed `hrpns` frames (`basetime` equal to
/// `validtime`; a forecast step would differ), oldest first.
pub fn parse_target_times(body: &str) -> Result<Vec<TargetTime>, String> {
    let raw: Vec<RawTargetTime> =
        serde_json::from_str(body).map_err(|e| format!("reading JMA index: {e}"))?;
    let mut times: Vec<TargetTime> = raw
        .into_iter()
        .filter(|t| t.basetime == t.validtime && t.elements.iter().any(|e| e == "hrpns"))
        .filter_map(|t| {
            let digits = t.basetime.len() == 14 && t.basetime.bytes().all(|b| b.is_ascii_digit());
            let stamp = NaiveDateTime::parse_from_str(&t.basetime, "%Y%m%d%H%M%S").ok()?;
            digits.then_some(TargetTime {
                basetime: t.basetime,
                stamp,
            })
        })
        .collect();
    times.sort_by_key(|t| t.stamp);
    times.dedup_by(|a, b| a.basetime == b.basetime);
    Ok(times)
}

/// Path of the no-data GeoJSON under [`HOST`].
pub fn nodata_path(t: &TargetTime) -> String {
    let b = &t.basetime;
    format!("{b}/none/{b}/surf/hrpns_nd/data.geojson")
}

/// Path of one tile under [`HOST`].
pub fn tile_path(t: &TargetTime, z: u32, x: u32, y: u32) -> String {
    let b = &t.basetime;
    format!("{b}/none/{b}/surf/hrpns/{z}/{x}/{y}.png")
}

/// Newest `limit` frames that are not already `known`, oldest first so the
/// timeline fills in order like OPERA's backfill.
pub fn backfill_targets(
    times: &[TargetTime],
    known: &HashSet<String>,
    limit: usize,
) -> Vec<TargetTime> {
    let start = times.len().saturating_sub(limit);
    times[start..]
        .iter()
        .filter(|t| !known.contains(&t.basetime))
        .cloned()
        .collect()
}

// ---- Colors to classes -----------------------------------------------------

/// One tile pixel read back from JMA's legend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    /// Rain-rate class, index into [`BOUNDS`] / [`PALETTE`].
    Class(u8),
    /// Transparent: JMA draws no precipitation here.
    Undetect,
    /// Not a legend color, or no data.
    Missing,
}

pub fn cell_of(rgba: [u8; 4]) -> Cell {
    match rgba[3] {
        0 => Cell::Undetect,
        255 => LEGEND
            .iter()
            .position(|c| c[..] == rgba[..3])
            .map_or(Cell::Missing, |i| Cell::Class(i as u8)),
        _ => Cell::Missing,
    }
}

/// The grid texture encoding (`docs/protocol.md`, mosaic texture).
fn texel(cell: Cell) -> [u8; 4] {
    match cell {
        Cell::Class(c) => [c + 1, 0, 0, 255],
        Cell::Missing => [0, 1, 0, 255],
        Cell::Undetect => [0, 2, 0, 255],
    }
}

/// Decode one 256 px tile, indexed or RGBA, into cells.
pub fn decode_tile(bytes: &[u8]) -> Result<Vec<Cell>, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| format!("tile: {e}"))?;
    let (width, height) = reader.info().size();
    if (width, height) != (TILE_PX, TILE_PX) {
        return Err(format!("tile is {width}×{height}, not {TILE_PX} px"));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "tile: output too large".to_string())?;
    let mut buf = vec![0; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("tile: {e}"))?;
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("tile: palette was not expanded".into()),
    };
    let pixels = (TILE_PX * TILE_PX) as usize;
    let mut cells = Vec::with_capacity(pixels);
    for row in 0..TILE_PX as usize {
        let line = &buf[row * info.line_size..][..TILE_PX as usize * channels];
        for px in line.chunks_exact(channels) {
            let rgba = match channels {
                1 => [px[0], px[0], px[0], 255],
                2 => [px[0], px[0], px[0], px[1]],
                3 => [px[0], px[1], px[2], 255],
                _ => [px[0], px[1], px[2], px[3]],
            };
            cells.push(cell_of(rgba));
        }
    }
    Ok(cells)
}

// ---- No-data mask ----------------------------------------------------------

/// Rings of the `hrpns_nd` polygons as lon/lat pairs. JMA serves the file
/// gzip-encoded whatever the request says; a plain body is accepted too.
pub fn parse_nodata(bytes: &[u8]) -> Result<Vec<Vec<(f64, f64)>>, String> {
    let json: Value = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut text = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .take(NODATA_JSON_MAX + 1)
            .read_to_end(&mut text)
            .map_err(|e| format!("no-data gzip: {e}"))?;
        if text.len() as u64 > NODATA_JSON_MAX {
            return Err("no-data GeoJSON over the size limit".into());
        }
        serde_json::from_slice(&text)
    } else {
        serde_json::from_slice(bytes)
    }
    .map_err(|e| format!("no-data GeoJSON: {e}"))?;
    let mut rings = Vec::new();
    let features = json["features"]
        .as_array()
        .ok_or("no-data GeoJSON has no features")?;
    for feature in features {
        let geometry = &feature["geometry"];
        let polygons: Vec<&Value> = match geometry["type"].as_str() {
            Some("Polygon") => vec![&geometry["coordinates"]],
            Some("MultiPolygon") => geometry["coordinates"]
                .as_array()
                .map(|p| p.iter().collect())
                .unwrap_or_default(),
            _ => continue,
        };
        for polygon in polygons {
            for ring in polygon.as_array().into_iter().flatten() {
                let points: Vec<(f64, f64)> = ring
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| Some((p[0].as_f64()?, p[1].as_f64()?)))
                    .collect();
                if points.len() >= 3 {
                    rings.push(points);
                }
            }
        }
    }
    Ok(rings)
}

/// Rasterize no-data rings onto `range`'s pixel grid, even-odd, sampling
/// each pixel's centre: `true` is no data.
pub fn rasterize_nodata(rings: &[Vec<(f64, f64)>], range: &TileRange) -> Vec<bool> {
    let width = range.width() as usize;
    let height = range.height() as usize;
    let [x0, dx, _, y0, _, dy] = range.geotransform();
    // Crossings of each row's centre line, bucketed by row.
    let mut crossings: Vec<Vec<f64>> = vec![Vec::new(); height];
    for ring in rings {
        let pixels: Vec<(f64, f64)> = ring
            .iter()
            .map(|&(lon, lat)| {
                // Web Mercator is undefined at the poles; the rings stop at 85°.
                let (x, y) = mercator_m(lon, lat.clamp(-85.06, 85.06));
                ((x - x0) / dx, (y - y0) / dy)
            })
            .collect();
        for i in 0..pixels.len() {
            let (ax, ay) = pixels[i];
            let (bx, by) = pixels[(i + 1) % pixels.len()];
            if ay == by {
                continue;
            }
            let (lo, hi) = if ay < by { (ay, by) } else { (by, ay) };
            // Rows whose centre r + 0.5 lies in [lo, hi).
            let first = (lo - 0.5).ceil().max(0.0);
            let last = (hi - 0.5).ceil().min(height as f64);
            let mut row = first;
            while row < last {
                let yc = row + 0.5;
                let x = ax + (yc - ay) * (bx - ax) / (by - ay);
                crossings[row as usize].push(x);
                row += 1.0;
            }
        }
    }
    let mut mask = vec![false; width * height];
    for (row, xs) in crossings.iter_mut().enumerate() {
        xs.sort_by(f64::total_cmp);
        for [enter, leave] in xs.as_chunks::<2>().0 {
            // Columns whose centre c + 0.5 lies in [enter, leave).
            let from = (enter - 0.5).ceil().clamp(0.0, width as f64) as usize;
            let to = (leave - 0.5).ceil().clamp(0.0, width as f64) as usize;
            if from < to {
                mask[row * width + from..row * width + to].fill(true);
            }
        }
    }
    mask
}

/// Tiles worth fetching: every tile without a mask, otherwise the ones with
/// a covered pixel within two pixels of them, so a rounding difference at
/// the coverage edge cannot drop a tile with rain in it.
pub fn needed_tiles(range: &TileRange, nodata: Option<&[bool]>) -> Vec<(u32, u32)> {
    let Some(mask) = nodata else {
        return range.tiles();
    };
    let width = range.width() as usize;
    let height = range.height() as usize;
    const MARGIN: usize = 2;
    range
        .tiles()
        .into_iter()
        .filter(|&(x, y)| {
            let c0 = ((x - range.x0) * TILE_PX) as usize;
            let r0 = ((y - range.y0) * TILE_PX) as usize;
            let cols = c0.saturating_sub(MARGIN)..(c0 + TILE_PX as usize + MARGIN).min(width);
            let rows = r0.saturating_sub(MARGIN)..(r0 + TILE_PX as usize + MARGIN).min(height);
            rows.into_iter()
                .any(|r| mask[r * width + cols.start..r * width + cols.end].contains(&false))
        })
        .collect()
}

// ---- Frame assembly --------------------------------------------------------

/// Stitch decoded tiles into the frame's RGBA texture. A tile that was not
/// fetched is missing throughout. A measured class always shows; a
/// transparent pixel is undetect inside coverage and missing outside it.
pub fn assemble(
    range: &TileRange,
    nodata: Option<&[bool]>,
    tiles: &[((u32, u32), Vec<Cell>)],
) -> Vec<u8> {
    let width = range.width() as usize;
    let height = range.height() as usize;
    let mut pixels = texel(Cell::Missing).repeat(width * height);
    let tile = TILE_PX as usize;
    for ((x, y), cells) in tiles {
        let c0 = ((x - range.x0) * TILE_PX) as usize;
        let r0 = ((y - range.y0) * TILE_PX) as usize;
        for (i, &cell) in cells.iter().enumerate().take(tile * tile) {
            let (r, c) = (r0 + i / tile, c0 + i % tile);
            let at = r * width + c;
            let cell = match cell {
                Cell::Undetect if nodata.is_some_and(|m| m[at]) => Cell::Missing,
                other => other,
            };
            pixels[at * 4..at * 4 + 4].copy_from_slice(&texel(cell));
        }
    }
    pixels
}

/// The wire frame for one stitched raster.
pub fn frame_for(t: &TargetTime, range: &TileRange) -> MosaicFrame {
    MosaicFrame {
        id: format!("{ID}-{}", t.stamp.format("%Y%m%dT%H%M%SZ")),
        product: PRODUCT.into(),
        product_name: PRODUCT_NAME.into(),
        units: UNITS.into(),
        scan_time: t.stamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        sweep_end: None,
        status: FrameStatus::Complete,
        texture: String::new(),
        width: range.width(),
        height: range.height(),
        crs: crs(),
        geotransform: range.geotransform(),
        palette: palette(),
        bounds: BOUNDS.to_vec(),
    }
}

struct Loaded {
    target: TargetTime,
    frame: MosaicFrame,
    texture: Vec<u8>,
    start_ms: i64,
    tiles: usize,
    bytes: usize,
    elapsed: Duration,
}

fn jma_log(message: impl std::fmt::Display) {
    eprintln!("{} JMA {message}", Utc::now().to_rfc3339());
}

fn log_load(kind: &str, loaded: &Loaded) {
    let stamp = loaded.target.stamp.format("%Y%m%dT%H%M");
    let sep = if kind.is_empty() { "" } else { " " };
    jma_log(format_args!(
        "{kind}{sep}{stamp}: {} tiles, {} KiB in {:.0?}",
        loaded.tiles,
        loaded.bytes >> 10,
        loaded.elapsed
    ));
}

/// One GET bounded by [`REQUEST_TIMEOUT`] and a request slot, retried once.
async fn get_bounded<G, GF>(path: String, get: &G, slots: &Semaphore) -> Result<Vec<u8>, String>
where
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
{
    let _permit = slots.acquire().await.map_err(|e| format!("{path}: {e}"))?;
    let mut last = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            sleep(RETRY_DELAY).await;
        }
        match timeout(REQUEST_TIMEOUT, get(path.clone())).await {
            Ok(Ok(bytes)) => return Ok(bytes),
            Ok(Err(e)) => last = e,
            Err(_) => last = format!("{path}: timed out"),
        }
    }
    Err(last)
}

/// Fetch, classify, and encode one frame. Any tile that cannot be read
/// fails the frame, so a hole is never drawn as dry ground; the caller
/// tries again on its next pass. A missing no-data file only costs the
/// missing bits and the tile pruning.
async fn load_frame<G, GF>(
    target: TargetTime,
    get: Arc<G>,
    slots: Arc<Semaphore>,
) -> Result<Loaded, String>
where
    G: Fn(String) -> GF + Send + Sync + 'static,
    GF: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
{
    let started = Instant::now();
    let range = TileRange::japan();
    let mut bytes = 0;
    let nodata = match get_bounded(nodata_path(&target), &*get, &slots).await {
        Ok(body) => {
            bytes += body.len();
            spawn_blocking(move || {
                parse_nodata(&body).map(|rings| rasterize_nodata(&rings, &range))
            })
            .await
            .map_err(|e| format!("no-data: {e}"))?
            .map_err(|e| jma_log(format_args!("{}: {e}", target.basetime)))
            .ok()
        }
        Err(e) => {
            jma_log(format_args!("{}: no-data: {e}", target.basetime));
            None
        }
    };
    let wanted = needed_tiles(&range, nodata.as_deref());
    let mut fetches = JoinSet::new();
    for (x, y) in wanted {
        let path = tile_path(&target, range.z, x, y);
        let get = Arc::clone(&get);
        let slots = Arc::clone(&slots);
        fetches.spawn(async move {
            let body = get_bounded(path.clone(), &*get, &slots).await?;
            let len = body.len();
            let cells = spawn_blocking(move || decode_tile(&body))
                .await
                .map_err(|e| format!("{path}: {e}"))?
                .map_err(|e| format!("{path}: {e}"))?;
            Ok::<_, String>(((x, y), cells, len))
        });
    }
    let mut tiles = Vec::new();
    while let Some(joined) = fetches.join_next().await {
        let (xy, cells, len) = joined.map_err(|e| format!("tile task: {e}"))??;
        bytes += len;
        tiles.push((xy, cells));
    }
    let count = tiles.len();
    let frame = frame_for(&target, &range);
    let texture = spawn_blocking(move || {
        let pixels = assemble(&range, nodata.as_deref(), &tiles);
        sweep::png(range.width(), range.height(), &pixels)
    })
    .await
    .map_err(|e| format!("encoding JMA texture: {e}"))?
    .map_err(|e| format!("encoding JMA texture: {e}"))?;
    let start_ms = target.stamp.and_utc().timestamp_millis();
    Ok(Loaded {
        target,
        frame,
        texture,
        start_ms,
        tiles: count,
        bytes,
        elapsed: started.elapsed(),
    })
}

/// Read the index and load its newest frame unless it is `known`.
async fn load_newest<I, IF, G, GF>(
    known: &HashSet<String>,
    index: I,
    get: Arc<G>,
    slots: Arc<Semaphore>,
) -> Result<Option<Loaded>, String>
where
    I: Fn() -> IF,
    IF: Future<Output = Result<Vec<TargetTime>, String>>,
    G: Fn(String) -> GF + Send + Sync + 'static,
    GF: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
{
    let times = index().await?;
    let Some(newest) = times.last().cloned() else {
        return Ok(None);
    };
    if known.contains(&newest.basetime) {
        return Ok(None);
    }
    Ok(Some(load_frame(newest, get, slots).await?))
}

/// Load `targets` one frame at a time and emit them as backfill, oldest
/// first. A frame that fails is skipped; a closed receiver ends the fill.
async fn fill_history<G, GF>(
    events: Sender<Event>,
    targets: Vec<TargetTime>,
    get: Arc<G>,
    slots: Arc<Semaphore>,
) where
    G: Fn(String) -> GF + Send + Sync + 'static,
    GF: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
{
    for target in targets {
        if events.is_closed() {
            return;
        }
        let basetime = target.basetime.clone();
        match load_frame(target, Arc::clone(&get), Arc::clone(&slots)).await {
            Ok(loaded) => {
                log_load("backfill", &loaded);
                if events
                    .send(ev_backfill(loaded.frame, loaded.texture, loaded.start_ms))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(reason) => jma_log(format_args!("backfill {basetime}: {reason}")),
        }
    }
}

async fn index_http() -> Result<Vec<TargetTime>, String> {
    let body = get_http(INDEX.to_owned()).await?;
    let text = String::from_utf8(body).map_err(|e| format!("reading JMA index: {e}"))?;
    parse_target_times(&text)
}

async fn get_http(path: String) -> Result<Vec<u8>, String> {
    let max = if path.ends_with(".png") {
        TILE_MAX
    } else if path.ends_with(".geojson") {
        NODATA_MAX
    } else {
        INDEX_MAX
    };
    let url = format!("{HOST}/{path}");
    let response = live_index::http_client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fetching JMA {path}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("fetching JMA {path}: {e}"))?;
    live_index::take_body(response, max)
        .await
        .map_err(|e| format!("reading JMA {path}: {e}"))
}

async fn index_bounded() -> Result<Vec<TargetTime>, String> {
    timeout(REQUEST_TIMEOUT, index_http())
        .await
        .unwrap_or_else(|_| Err("JMA index timed out".into()))
}

async fn poll_loop(events: Sender<Event>, mut known: HashSet<String>) {
    let get = Arc::new(get_http);
    let slots = Arc::new(Semaphore::new(REQUESTS_IN_FLIGHT));
    let first = timeout(
        START_TIMEOUT,
        load_newest(&known, index_bounded, Arc::clone(&get), Arc::clone(&slots)),
    )
    .await;
    match first {
        Ok(Ok(Some(loaded))) => {
            log_load("", &loaded);
            known.insert(loaded.target.basetime.clone());
            let _ = events
                .send(ev_frame(loaded.frame, loaded.texture, loaded.start_ms))
                .await;
        }
        Ok(Ok(None)) => {
            let _ = events
                .send(ev_silent("JMA index lists no observed frame".into()))
                .await;
        }
        Ok(Err(reason)) => {
            let _ = events.send(ev_offline(reason)).await;
        }
        Err(_) => {
            let _ = events.send(ev_offline("JMA fetch timed out".into())).await;
        }
    }
    // Newest is on screen; the rest of the dozen fills in behind it. The set
    // is dropped with this task, which aborts the backfill with it.
    let mut backfill = JoinSet::new();
    backfill.spawn({
        let events = events.clone();
        let known = known.clone();
        let get = Arc::clone(&get);
        let slots = Arc::clone(&slots);
        async move {
            sleep(BACKFILL_DELAY).await;
            match index_bounded().await {
                Ok(times) => {
                    let targets = backfill_targets(&times, &known, HISTORY_MAX);
                    fill_history(events, targets, get, slots).await;
                }
                Err(reason) => jma_log(format_args!("backfill index: {reason}")),
            }
        }
    });
    loop {
        sleep(IDLE).await;
        match load_newest(&known, index_bounded, Arc::clone(&get), Arc::clone(&slots)).await {
            Ok(Some(loaded)) => {
                log_load("", &loaded);
                known.insert(loaded.target.basetime.clone());
                if known.len() > HISTORY_MAX * 4 {
                    known = HashSet::from([loaded.target.basetime.clone()]);
                }
                if events
                    .send(ev_frame(loaded.frame, loaded.texture, loaded.start_ms))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(None) => {}
            Err(reason) => {
                if events.send(ev_offline(reason)).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{GeoPoint, Selection};
    use crate::source::{SourceRegistry, inverse_affine};
    use std::collections::HashMap;

    const INDEX_FIXTURE: &str = include_str!("../tests/fixtures/jma/targetTimes_N1.json");
    /// Synthetic, encoded as JMA serves its tiles: 4-bit indexed, JMA's
    /// ten-entry `PLTE` and `tRNS`, 16 px columns of indices 0–9.
    const STRIPES_4BIT: &[u8] = include_bytes!("../tests/fixtures/jma/hrpns-stripes-4bit.png");
    /// Synthetic `hrpns_nd`: a world ring with a hole over 136–144°E, 33–36°N.
    const NODATA_FIXTURE: &str = include_str!("../tests/fixtures/jma/hrpns_nd-synthetic.geojson");

    fn current_thread() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn target(basetime: &str) -> TargetTime {
        TargetTime {
            basetime: basetime.into(),
            stamp: NaiveDateTime::parse_from_str(basetime, "%Y%m%d%H%M%S").unwrap(),
        }
    }

    fn rgba_tile(pixel: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((TILE_PX * TILE_PX * 4) as usize);
        for y in 0..TILE_PX as usize {
            for x in 0..TILE_PX as usize {
                pixels.extend_from_slice(&pixel(x, y));
            }
        }
        sweep::png(TILE_PX, TILE_PX, &pixels).unwrap()
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn index_keeps_observed_frames_oldest_first() {
        let times = parse_target_times(INDEX_FIXTURE).unwrap();
        assert_eq!(
            times
                .iter()
                .map(|t| t.basetime.as_str())
                .collect::<Vec<_>>(),
            ["20260925071500", "20260925072000", "20260925072500"]
        );
        assert_eq!(
            times[2].stamp,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 25)
                .unwrap()
                .and_hms_opt(7, 25, 0)
                .unwrap()
        );
        // Forecast steps (N2's shape), other elements, and junk stamps drop.
        let mixed = r#"[
            {"basetime":"20260925073500","validtime":"20260925083500","elements":["hrpns","hrpns_nd"]},
            {"basetime":"20260925073000","validtime":"20260925073000","elements":["thns"]},
            {"basetime":"2026092507300","validtime":"2026092507300","elements":["hrpns"]},
            {"basetime":"20260925073000","validtime":"20260925073000","elements":["hrpns"]}
        ]"#;
        let times = parse_target_times(mixed).unwrap();
        assert_eq!(times, vec![target("20260925073000")]);
        assert!(parse_target_times("{}").is_err());
    }

    #[test]
    fn paths_follow_the_viewer_layout() {
        let t = target("20260925072500");
        assert_eq!(
            tile_path(&t, 6, 56, 25),
            "20260925072500/none/20260925072500/surf/hrpns/6/56/25.png"
        );
        assert_eq!(
            nodata_path(&t),
            "20260925072500/none/20260925072500/surf/hrpns_nd/data.geojson"
        );
    }

    #[test]
    fn backfill_skips_known_and_keeps_a_dozen() {
        let times: Vec<TargetTime> = (0..37)
            .map(|i| {
                let stamp = chrono::NaiveDate::from_ymd_opt(2026, 9, 25)
                    .unwrap()
                    .and_hms_opt(4, 25, 0)
                    .unwrap()
                    + chrono::Duration::minutes(5 * i);
                target(&stamp.format("%Y%m%d%H%M%S").to_string())
            })
            .collect();
        let known = HashSet::from(["20260925072500".to_string()]);
        let targets = backfill_targets(&times, &known, HISTORY_MAX);
        assert_eq!(targets.len(), HISTORY_MAX - 1);
        assert_eq!(targets[0].basetime, "20260925063000");
        assert_eq!(targets[10].basetime, "20260925072000");
    }

    #[test]
    fn japan_raster_is_the_z6_block_over_the_coverage_box() {
        let range = TileRange::japan();
        assert_eq!(
            range,
            TileRange {
                z: 6,
                x0: 53,
                y0: 22,
                x1: 58,
                y1: 28
            }
        );
        assert_eq!(range.tiles().len(), 42);
        assert_eq!((range.width(), range.height()), (1536, 1792));
        let [x0, dx, rx, y0, ry, dy] = range.geotransform();
        let half = PI * EARTH_RADIUS_M;
        assert!((half - 20_037_508.342_789_244).abs() < 1e-6);
        assert!((x0 - (-half + 53.0 * 2.0 * half / 64.0)).abs() < 1e-6);
        assert!((y0 - (half - 22.0 * 2.0 * half / 64.0)).abs() < 1e-6);
        assert!((dx - 2.0 * half / 64.0 / 256.0).abs() < 1e-9);
        assert_eq!((rx, ry), (0.0, 0.0));
        assert_eq!(dy, -dx);
    }

    #[test]
    fn geotransform_puts_tokyo_in_its_xyz_tile_pixel() {
        // Tokyo Station. Standard XYZ math: x = (lon+180)/360·2^z,
        // y = (1 − ln(tan φ + sec φ)/π)/2·2^z, 256 px per tile.
        let (lon, lat) = (139.7671_f64, 35.6812_f64);
        let n = 64.0;
        let tx = (lon + 180.0) / 360.0 * n;
        let phi = lat.to_radians();
        let ty = (1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / PI) / 2.0 * n;
        let range = TileRange::japan();
        let (mx, my) = mercator_m(lon, lat);
        let (col, row) = inverse_affine(range.geotransform(), mx, my).unwrap();
        assert!((col - (tx - 53.0) * 256.0).abs() < 1e-6, "{col}");
        assert!((row - (ty - 22.0) * 256.0).abs() < 1e-6, "{row}");
        // Tile 56/25 at z6, as JMA and every XYZ server number it.
        assert_eq!((tx as u32, ty as u32), (56, 25));
        assert_eq!(
            ((col / 256.0) as u32 + 53, (row / 256.0) as u32 + 22),
            (56, 25)
        );
        // The raster's corners are the tile block's corners.
        let (col, row) = inverse_affine(
            range.geotransform(),
            mercator_m(-180.0 + 53.0 * 360.0 / 64.0, 0.0).0,
            0.0,
        )
        .unwrap();
        assert!(col.abs() < 1e-6 && row > 0.0);
    }

    #[test]
    fn crs_is_web_mercator_on_a_sphere() {
        let Crs::Mercator {
            ellipsoid,
            lon0_deg,
            scale,
            false_easting_m,
            false_northing_m,
            datum_transform,
        } = crs()
        else {
            panic!("mercator");
        };
        assert_eq!(ellipsoid.semi_major_m, 6_378_137.0);
        assert_eq!(ellipsoid.inverse_flattening, 0.0);
        assert_eq!((lon0_deg, scale), (0.0, 1.0));
        assert_eq!((false_easting_m, false_northing_m), (0.0, 0.0));
        assert!(datum_transform.is_none());
        let json = serde_json::to_value(crs()).unwrap();
        assert_eq!(json["kind"], "mercator");
        assert_eq!(json["ellipsoid"]["inverseFlattening"], 0.0);
    }

    #[test]
    fn legend_colors_map_back_to_their_bands() {
        assert_eq!(PALETTE.len(), LEGEND.len());
        assert_eq!(BOUNDS.len(), PALETTE.len() + 1);
        assert!(BOUNDS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(&BOUNDS[..8], &[0.0, 1.0, 5.0, 10.0, 20.0, 30.0, 50.0, 80.0]);
        for (i, rgb) in LEGEND.iter().enumerate() {
            assert_eq!(cell_of([rgb[0], rgb[1], rgb[2], 255]), Cell::Class(i as u8));
            // The same color, not fully opaque, is not JMA's legend.
            assert_eq!(cell_of([rgb[0], rgb[1], rgb[2], 128]), Cell::Missing);
        }
        assert_eq!(cell_of([255, 255, 255, 0]), Cell::Undetect);
        assert_eq!(cell_of([0, 0, 0, 0]), Cell::Undetect);
        // One step off a legend color is unknown, never the nearest band.
        assert_eq!(cell_of([0xf2, 0xf2, 0xfe, 255]), Cell::Missing);
        assert_eq!(cell_of([255, 255, 255, 255]), Cell::Missing);
        assert_eq!(texel(Cell::Class(0)), [1, 0, 0, 255]);
        assert_eq!(texel(Cell::Class(7)), [8, 0, 0, 255]);
        assert_eq!(texel(Cell::Missing), [0, 1, 0, 255]);
        assert_eq!(texel(Cell::Undetect), [0, 2, 0, 255]);
    }

    #[test]
    fn indexed_fixture_tile_decodes_to_classes() {
        let info = png::Decoder::new(std::io::Cursor::new(STRIPES_4BIT))
            .read_info()
            .unwrap()
            .info()
            .clone();
        assert_eq!(info.color_type, png::ColorType::Indexed);
        assert_eq!(info.bit_depth, png::BitDepth::Four);
        let cells = decode_tile(STRIPES_4BIT).unwrap();
        assert_eq!(cells.len(), 256 * 256);
        for row in [0, 100, 255] {
            for col in 0..256 {
                let want = match col / 16 {
                    // Indices 0 and 1 are JMA's two transparent entries.
                    0 | 1 => Cell::Undetect,
                    i @ 2..=9 => Cell::Class(i as u8 - 2),
                    _ => Cell::Undetect,
                };
                assert_eq!(cells[row * 256 + col], want, "row {row} col {col}");
            }
        }
    }

    #[test]
    fn rgba_tile_decodes_like_the_indexed_one() {
        let indexed = decode_tile(STRIPES_4BIT).unwrap();
        let rgba = rgba_tile(|x, _| match x / 16 {
            i @ 2..=9 => {
                let c = LEGEND[i - 2];
                [c[0], c[1], c[2], 255]
            }
            _ => [0, 0, 0, 0],
        });
        assert_eq!(decode_tile(&rgba).unwrap(), indexed);
        // Unknown and half-transparent colors are missing.
        let odd = rgba_tile(|x, _| match x {
            0 => [0x12, 0x34, 0x56, 255],
            1 => [0xff, 0x99, 0x00, 200],
            _ => [0xff, 0x99, 0x00, 255],
        });
        let cells = decode_tile(&odd).unwrap();
        assert_eq!(cells[0], Cell::Missing);
        assert_eq!(cells[1], Cell::Missing);
        assert_eq!(cells[2], Cell::Class(5));
        // A tile of the wrong size is refused.
        let small = sweep::png(2, 2, &[0; 16]).unwrap();
        assert!(decode_tile(&small).is_err());
        assert!(decode_tile(b"not a png").is_err());
    }

    fn inside_hole(lon: f64, lat: f64) -> bool {
        (136.0..144.0).contains(&lon) && (33.0..36.0).contains(&lat)
    }

    /// Longitude and latitude of a raster pixel's centre.
    fn pixel_lonlat(range: &TileRange, col: usize, row: usize) -> (f64, f64) {
        let [x0, dx, _, y0, _, dy] = range.geotransform();
        let x = x0 + (col as f64 + 0.5) * dx;
        let y = y0 + (row as f64 + 0.5) * dy;
        let lon = (x / EARTH_RADIUS_M).to_degrees();
        let lat = (2.0 * (y / EARTH_RADIUS_M).exp().atan() - PI / 2.0).to_degrees();
        (lon, lat)
    }

    #[test]
    fn nodata_mask_samples_pixel_centres_even_odd() {
        let rings = parse_nodata(NODATA_FIXTURE.as_bytes()).unwrap();
        assert_eq!(rings.len(), 2);
        let range = TileRange::japan();
        let mask = rasterize_nodata(&rings, &range);
        let width = range.width() as usize;
        assert_eq!(mask.len(), width * range.height() as usize);
        let mut covered = 0;
        for row in (0..range.height() as usize).step_by(7) {
            for col in (0..width).step_by(5) {
                let (lon, lat) = pixel_lonlat(&range, col, row);
                assert_eq!(
                    !mask[row * width + col],
                    inside_hole(lon, lat),
                    "col {col} row {row} at {lon},{lat}"
                );
                covered += usize::from(!mask[row * width + col]);
            }
        }
        assert!(covered > 0);
        // gzip, as JMA serves it, reads the same.
        let gz = gzip(NODATA_FIXTURE.as_bytes());
        assert_eq!(parse_nodata(&gz).unwrap(), rings);
        assert!(parse_nodata(b"{}").is_err());
    }

    #[test]
    fn only_tiles_touching_coverage_are_fetched() {
        let range = TileRange::japan();
        let rings = parse_nodata(NODATA_FIXTURE.as_bytes()).unwrap();
        let mask = rasterize_nodata(&rings, &range);
        let wanted = needed_tiles(&range, Some(&mask));
        // 136–144°E is x 56 and 57 (135–140.625–146.25°E at z6); 33–36°N
        // is y 25 (32.8–36.6°N).
        assert_eq!(wanted, vec![(56, 25), (57, 25)]);
        assert_eq!(needed_tiles(&range, None).len(), 42);
        // Coverage one pixel past a tile edge still fetches that tile.
        let mut edge = vec![true; mask.len()];
        let width = range.width() as usize;
        edge[300 * width + 256] = false; // first column of tile x 54, row of y 23
        assert_eq!(needed_tiles(&range, Some(&edge)), vec![(53, 23), (54, 23)]);
    }

    #[test]
    fn assemble_keeps_measured_classes_and_marks_missing() {
        let range = TileRange {
            z: 6,
            x0: 56,
            y0: 25,
            x1: 57,
            y1: 25,
        };
        let width = range.width() as usize;
        let mut cells = vec![Cell::Undetect; 256 * 256];
        cells[0] = Cell::Class(3);
        cells[1] = Cell::Missing;
        cells[2] = Cell::Class(7);
        let mut nodata = vec![false; width * 256];
        nodata[2] = true; // measured beats the mask
        nodata[3] = true; // transparent + no data = missing
        let pixels = assemble(&range, Some(&nodata), &[((56, 25), cells)]);
        assert_eq!(pixels.len(), width * 256 * 4);
        let px = |i: usize| {
            [
                pixels[i * 4],
                pixels[i * 4 + 1],
                pixels[i * 4 + 2],
                pixels[i * 4 + 3],
            ]
        };
        assert_eq!(px(0), [4, 0, 0, 255]);
        assert_eq!(px(1), [0, 1, 0, 255]);
        assert_eq!(px(2), [8, 0, 0, 255]);
        assert_eq!(px(3), [0, 1, 0, 255]);
        assert_eq!(px(4), [0, 2, 0, 255]);
        // Tile 57/25 was not fetched: missing throughout.
        assert_eq!(px(256), [0, 1, 0, 255]);
        assert_eq!(px(width * 256 - 1), [0, 1, 0, 255]);
    }

    type Reply = std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send>>;

    /// Serve fixture bytes by path; count requests.
    fn mock_server(
        routes: HashMap<String, Vec<u8>>,
        hits: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> impl Fn(String) -> Reply + Send + Sync + 'static {
        let routes = Arc::new(routes);
        move |path: String| {
            let routes = Arc::clone(&routes);
            let hits = Arc::clone(&hits);
            Box::pin(async move {
                hits.lock().unwrap().push(path.clone());
                routes
                    .get(&path)
                    .cloned()
                    .ok_or_else(|| format!("404 {path}"))
            })
        }
    }

    #[test]
    fn frame_loads_from_mocked_http() {
        let t = target("20260925072500");
        let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut routes = HashMap::new();
        routes.insert(nodata_path(&t), gzip(NODATA_FIXTURE.as_bytes()));
        routes.insert(tile_path(&t, 6, 56, 25), STRIPES_4BIT.to_vec());
        routes.insert(tile_path(&t, 6, 57, 25), rgba_tile(|_, _| [0, 0, 0, 0]));
        let get = Arc::new(mock_server(routes, Arc::clone(&hits)));
        let loaded = current_thread()
            .block_on(load_frame(t.clone(), get, Arc::new(Semaphore::new(4))))
            .unwrap();
        assert_eq!(loaded.tiles, 2);
        assert_eq!(hits.lock().unwrap().len(), 3);
        let frame = &loaded.frame;
        assert_eq!(frame.id, "jma-20260925T072500Z");
        assert_eq!(frame.scan_time, "2026-09-25T07:25:00Z");
        assert_eq!(frame.status, FrameStatus::Complete);
        assert_eq!((frame.width, frame.height), (1536, 1792));
        assert_eq!(frame.product_name, PRODUCT_NAME);
        assert_eq!(frame.units, "mm/h");
        assert_eq!(frame.bounds.len(), frame.palette.len() + 1);
        assert_eq!(loaded.start_ms, t.stamp.and_utc().timestamp_millis());
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&loaded.texture))
            .read_info()
            .unwrap();
        let mut buf = vec![0; decoder.output_buffer_size().unwrap()];
        decoder.next_frame(&mut buf).unwrap();
        let width = 1536;
        let at = |col: usize, row: usize| {
            let i = (row * width + col) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        // Tile 56/25 starts at column 768, row 768; column 40 of it is
        // index 2, the first legend color.
        assert_eq!(at(768 + 40, 768 + 128), [1, 0, 0, 255]);
        assert_eq!(at(768 + 159, 768 + 128), [8, 0, 0, 255]);
        // Tile 53/22 was pruned: missing.
        assert_eq!(at(0, 0), [0, 1, 0, 255]);
    }

    #[test]
    fn missing_nodata_fetches_every_tile_and_a_failed_tile_fails_the_frame() {
        let t = target("20260925072500");
        let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut routes = HashMap::new();
        for (x, y) in TileRange::japan().tiles() {
            routes.insert(tile_path(&t, 6, x, y), rgba_tile(|_, _| [0, 0, 0, 0]));
        }
        let loaded = current_thread()
            .block_on(load_frame(
                t.clone(),
                Arc::new(mock_server(routes.clone(), Arc::clone(&hits))),
                Arc::new(Semaphore::new(4)),
            ))
            .unwrap();
        assert_eq!(loaded.tiles, 42);
        routes.remove(&tile_path(&t, 6, 55, 24));
        let failed = current_thread().block_on(load_frame(
            t,
            Arc::new(mock_server(routes, hits)),
            Arc::new(Semaphore::new(4)),
        ));
        assert!(failed.is_err());
    }

    #[test]
    fn newest_unseen_frame_is_loaded_once() {
        let times = parse_target_times(INDEX_FIXTURE).unwrap();
        let newest = times.last().unwrap().clone();
        let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut routes = HashMap::new();
        routes.insert(nodata_path(&newest), NODATA_FIXTURE.as_bytes().to_vec());
        for (x, y) in [(56, 25), (57, 25)] {
            routes.insert(tile_path(&newest, 6, x, y), STRIPES_4BIT.to_vec());
        }
        let get = Arc::new(mock_server(routes, Arc::clone(&hits)));
        let slots = Arc::new(Semaphore::new(4));
        let runtime = current_thread();
        let index = || {
            let times = times.clone();
            async move { Ok(times) }
        };
        let loaded = runtime
            .block_on(load_newest(
                &HashSet::new(),
                index,
                Arc::clone(&get),
                Arc::clone(&slots),
            ))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.target, newest);
        let known = HashSet::from([newest.basetime.clone()]);
        let before = hits.lock().unwrap().len();
        let again = runtime
            .block_on(load_newest(&known, index, get, slots))
            .unwrap();
        assert!(again.is_none());
        assert_eq!(hits.lock().unwrap().len(), before);
    }

    #[test]
    fn requests_stay_within_the_slots() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let t = target("20260925072500");
        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let tile = rgba_tile(|_, _| [0, 0, 0, 0]);
        let get = {
            let inflight = Arc::clone(&inflight);
            let peak = Arc::clone(&peak);
            Arc::new(move |path: String| {
                let inflight = Arc::clone(&inflight);
                let peak = Arc::clone(&peak);
                let tile = tile.clone();
                async move {
                    let n = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    sleep(Duration::from_millis(2)).await;
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    if path.ends_with(".geojson") {
                        Err("404".to_string())
                    } else {
                        Ok(tile)
                    }
                }
            })
        };
        current_thread()
            .block_on(load_frame(
                t,
                get,
                Arc::new(Semaphore::new(REQUESTS_IN_FLIGHT)),
            ))
            .unwrap();
        assert_eq!(peak.load(Ordering::SeqCst), REQUESTS_IN_FLIGHT);
    }

    #[test]
    fn loading_frame_is_blank_mosaic_chrome() {
        let frame = loading_frame();
        assert_eq!(frame.id, "jma-loading");
        assert!(frame.scan_time.is_empty());
        assert_eq!(frame.status, FrameStatus::Partial);
        assert_eq!((frame.width, frame.height), (1, 1));
        assert_eq!(frame.bounds.len(), frame.palette.len() + 1);
        assert!(!loading_texture().unwrap().is_empty());
    }

    #[test]
    fn metadata_says_rain_rate_and_credits_jma() {
        let jma = Jma::new();
        let meta = jma.metadata();
        assert_eq!(meta.id, "jma");
        assert_eq!(meta.attribution, "Japan Meteorological Agency");
        assert_eq!(meta.default_product_class, ProductClass::PrecipitationRate);
        let mosaic = meta.mosaic.unwrap();
        assert_eq!(mosaic.selection_priority, SELECTION_PRIORITY);
        assert!(mosaic.covering);
    }

    #[test]
    fn jma_covers_japan_and_polar_still_wins_where_it_covers() {
        let registry = SourceRegistry::compiled();
        let pick = |lat, lon| {
            registry
                .covering_selection(GeoPoint { lat, lon }, None)
                .map(|s: Selection| s.source_id)
        };
        for (lat, lon) in [(35.68, 139.77), (34.69, 135.50), (43.06, 141.35)] {
            assert_eq!(pick(lat, lon).as_deref(), Some(ID), "{lat},{lon}");
        }
        // Kadena (RODN) covers Okinawa and Kunsan (RKJK) reaches Fukuoka:
        // PolarFamily wins wherever a dish covers.
        assert_eq!(pick(26.21, 127.68).as_deref(), Some("nexrad"));
        assert_eq!(pick(33.59, 130.40).as_deref(), Some("nexrad"));
        // Europe stays OPERA.
        assert_eq!(pick(51.5, -0.1).as_deref(), Some(crate::opera::ID));
    }
}
