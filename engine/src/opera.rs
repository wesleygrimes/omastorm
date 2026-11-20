//! EUMETNET OPERA COMP DBZH GridFamily adapter: anonymous ORD 24h COG
//! (`docs/grid-adapters.md`). Live fetch only; no vendored GeoTIFF.

use crate::{
    cog::{self, DecodedRaster},
    live_index,
    protocol::{
        Coverage, Crs, Ellipsoid, Family, FrameStatus, Kind, MeasuredCode, MosaicFrame,
        ProductClass,
    },
    source::{GridEvent, GridSender, MosaicMeta, SourceMetadataBorrowed},
    sweep,
};
use chrono::{NaiveDate, NaiveDateTime, TimeDelta, Utc};
use std::{
    collections::{HashSet, VecDeque},
    future::Future,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use tokio::{
    sync::Semaphore,
    task::{JoinHandle, spawn_blocking},
    time::{sleep, timeout},
};
use xml::reader::{EventReader, XmlEvent};

pub const ID: &str = "opera";
pub const NAME: &str = "EUMETNET OPERA";
pub const ATTRIBUTION: &str = "EUMETNET OPERA";
/// Continental fallback: a covering national mosaic ranks above this.
pub const SELECTION_PRIORITY: i32 = 10;

/// ORD 24h cache, then the ECMWF backup EUMETNET opened after the
/// Nov 2026 MeteoGate cloud outage.
pub const HOSTS: [&str; 2] = [
    "https://s3.waw3-1.cloudferro.com/openradar-24h",
    "https://object-store.os-api.cci1.ecmwf.int/openradar-24h-backup",
];
const LIST_PAGES_MAX: usize = 8;
/// Scans stamped past this are treated as bad metadata, not live data.
const FUTURE_SLACK: chrono::TimeDelta = chrono::TimeDelta::minutes(15);
/// Listings start here, so a host whose feed stopped looks empty.
const LOOKBACK_MINUTES: i64 = 90;
/// Service footprint from `envelope::OPERA` (ORD docs, approx corners).
pub const COVERAGE_NORTH: f64 = crate::envelope::OPERA.north;
pub const COVERAGE_SOUTH: f64 = crate::envelope::OPERA.south;
pub const COVERAGE_WEST: f64 = crate::envelope::OPERA.west;
pub const COVERAGE_EAST: f64 = crate::envelope::OPERA.east;

pub const LAT0_DEG: f64 = 55.0;
pub const LON0_DEG: f64 = 10.0;
pub const FALSE_EASTING_M: f64 = 1_950_000.0;
pub const FALSE_NORTHING_M: f64 = -2_100_000.0;

const BODY_MAX: usize = 12 << 20;
/// Timeline depth: matches NEXRAD's initial `BACKFILL_VOLUMES` dozen so
/// playback has the same loop length on first select. Live growth stays
/// capped here (not the polar catalog ring of 60) — each COMP is multi-MB.
pub const HISTORY_MAX: usize = 12;
const HOST_LIST_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(30);
/// After the newest COMP is on screen, pull earlier stamps (NEXRAD waits
/// the same beat so a hand-off mid-pan does not spend bandwidth).
const BACKFILL_DELAY: Duration = Duration::from_secs(3);
/// Concurrent fetch+decode during history fill. Two 3800×4400 frames is
/// already hundreds of MB; more would pile decoded rasters on the
/// current-thread runtime's blocking pool.
const BACKFILL_IN_FLIGHT: usize = 2;
const FILL: f32 = -9_999_000.0;

#[cfg(test)]
use crate::source::GridEvent as Event;

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

pub struct Opera {
    pub id: &'static str,
    coverage: Coverage,
    palette: Vec<String>,
    bounds: Vec<f64>,
}

impl Opera {
    pub fn new() -> Self {
        let template: crate::protocol::Frame =
            serde_json::from_str(include_str!("../data/product.json")).unwrap();
        Self {
            id: ID,
            coverage: Coverage::Box {
                north: COVERAGE_NORTH,
                south: COVERAGE_SOUTH,
                east: COVERAGE_EAST,
                west: COVERAGE_WEST,
            },
            palette: template.palette,
            bounds: template.bounds.iter().map(|&b| b as f64).collect(),
        }
    }

    pub fn metadata(&self) -> SourceMetadataBorrowed<'_> {
        SourceMetadataBorrowed {
            id: self.id,
            family: Family::Grid,
            kind: Kind::Mosaic,
            default_product_class: ProductClass::Reflectivity,
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
        Some((self.loading_frame(), Self::loading_texture().ok()?))
    }

    pub fn crs() -> Crs {
        Crs::LambertAzimuthalEqualArea {
            ellipsoid: Ellipsoid::WGS84,
            lat0_deg: LAT0_DEG,
            lon0_deg: LON0_DEG,
            false_easting_m: FALSE_EASTING_M,
            false_northing_m: FALSE_NORTHING_M,
            datum_transform: None,
        }
    }

    /// Shown the instant OPERA is selected, same job as a polar
    /// `<SITE>-loading` frame: chrome (legend, tick strip) stays up while
    /// the first COMP is still in flight. Empty `scan_time` draws no radar.
    pub fn loading_frame(&self) -> MosaicFrame {
        MosaicFrame {
            id: format!("{ID}-loading"),
            product: "REF".into(),
            product_name: "Reflectivity".into(),
            units: "dBZ".into(),
            scan_time: String::new(),
            sweep_end: None,
            status: FrameStatus::Partial,
            texture: String::new(),
            width: 1,
            height: 1,
            crs: Self::crs(),
            geotransform: [0.0, 1.0, 0.0, 0.0, 0.0, -1.0],
            palette: self.palette.clone(),
            bounds: self.bounds.clone(),
            measured_code: None,
        }
    }

    pub fn loading_texture() -> Result<Vec<u8>, String> {
        sweep::png(1, 1, &[0, 0, 0, 0]).map_err(|e| format!("encoding OPERA placeholder: {e}"))
    }

    pub fn poll(&self, events: GridSender, known_keys: HashSet<String>) -> JoinHandle<()> {
        let palette = self.palette.clone();
        let bounds = self.bounds.clone();
        tokio::spawn(async move {
            poll_loop(events, known_keys, palette, bounds).await;
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompObject {
    pub host: &'static str,
    pub key: String,
    pub stamp: NaiveDateTime,
}

#[derive(Debug)]
pub struct DbzhPage {
    pub objects: Vec<CompObject>,
    pub next: Option<String>,
}

/// Parse one ListObjects v2 page for `OPERA@…@DBZH.tiff` keys.
pub fn parse_dbzh_listing(host: &'static str, body: &str) -> Result<DbzhPage, String> {
    let mut objects = Vec::new();
    let mut next = None;
    let mut truncated = false;
    let mut text = String::new();
    for event in EventReader::new(body.as_bytes()) {
        match event.map_err(|e| format!("reading OPERA listing: {e}"))? {
            XmlEvent::StartElement { .. } => text.clear(),
            XmlEvent::Characters(chunk) => text.push_str(&chunk),
            XmlEvent::EndElement { name } => match name.local_name.as_str() {
                "Key" => {
                    if let Some(stamp) = parse_dbzh_key(&text)
                        && text == comp_key(stamp)
                    {
                        objects.push(CompObject {
                            host,
                            key: text.clone(),
                            stamp,
                        });
                    }
                }
                "NextContinuationToken" => next = Some(text.clone()),
                "IsTruncated" => truncated = text == "true",
                _ => {}
            },
            _ => {}
        }
    }
    if truncated && next.is_none() {
        return Err("OPERA listing is truncated without a continuation token".into());
    }
    objects.sort_by_key(|o| o.stamp);
    Ok(DbzhPage {
        objects,
        next: next.filter(|_| truncated),
    })
}

/// Follow continuation tokens: the ORD store caps pages below a day of keys.
async fn list_pages<P, PF>(host: &'static str, page: P) -> Result<Vec<CompObject>, String>
where
    P: Fn(Option<String>) -> PF,
    PF: Future<Output = Result<String, String>>,
{
    let mut objects = Vec::new();
    let mut seen = HashSet::new();
    let mut token = None;
    for _ in 0..LIST_PAGES_MAX {
        let listing = parse_dbzh_listing(host, &page(token.take()).await?)?;
        objects.extend(listing.objects);
        match listing.next {
            None => return Ok(objects),
            Some(next) if next.is_empty() || !seen.insert(next.clone()) => {
                return Err(format!("{host} repeated an OPERA listing token"));
            }
            Some(next) => token = Some(next),
        }
    }
    Err(format!(
        "{host} OPERA listing exceeds {LIST_PAGES_MAX} pages"
    ))
}

pub fn listing_url(
    host: &str,
    day: NaiveDate,
    start_after: Option<&str>,
    token: Option<&str>,
) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(host).map_err(|e| format!("OPERA host: {e}"))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("list-type", "2")
            .append_pair("prefix", &prefix_for(day))
            .append_pair("max-keys", "1000");
        if let Some(start_after) = start_after {
            query.append_pair("start-after", start_after);
        }
        if let Some(token) = token {
            query.append_pair("continuation-token", token);
        }
    }
    Ok(url)
}

/// COMP objects on `host` stamped from `LOOKBACK_MINUTES` before `now`.
/// Today's listing must succeed; yesterday's only adds history.
async fn list_window<L, LF>(
    host: &'static str,
    now: NaiveDateTime,
    list: &L,
) -> Result<Vec<CompObject>, String>
where
    L: Fn(reqwest::Url) -> LF,
    LF: Future<Output = Result<String, String>>,
{
    let since = now - TimeDelta::minutes(LOOKBACK_MINUTES);
    let start_after = format!(
        "{}OPERA@{}",
        prefix_for(since.date()),
        since.format("%Y%m%dT%H%M")
    );
    let today = now.date();
    let mut objects = if since.date() == today {
        list_day(host, today, Some(&start_after), list).await?
    } else {
        let today = list_day(host, today, None, list).await?;
        let mut objects = list_day(host, since.date(), Some(&start_after), list)
            .await
            .unwrap_or_default();
        objects.extend(today);
        objects
    };
    objects.sort_by_key(|o| o.stamp);
    Ok(objects)
}

async fn list_day<L, LF>(
    host: &'static str,
    day: NaiveDate,
    start_after: Option<&str>,
    list: &L,
) -> Result<Vec<CompObject>, String>
where
    L: Fn(reqwest::Url) -> LF,
    LF: Future<Output = Result<String, String>>,
{
    let pages = list_pages(host, |token: Option<String>| {
        let url = listing_url(host, day, start_after, token.as_deref());
        async move { list(url?).await }
    });
    timeout(HOST_LIST_TIMEOUT, pages)
        .await
        .map_err(|_| format!("listing {host} {day} timed out"))?
}

/// `…/OPERA/COMP/OPERA@YYYYMMDDTHHMM@0@DBZH.tiff`
pub fn parse_dbzh_key(key: &str) -> Option<NaiveDateTime> {
    let name = key.rsplit('/').next()?;
    let rest = name.strip_prefix("OPERA@")?;
    let stamp = rest.strip_suffix("@0@DBZH.tiff")?;
    NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M").ok()
}

fn comp_key(stamp: NaiveDateTime) -> String {
    format!(
        "{}OPERA@{}@0@DBZH.tiff",
        prefix_for(stamp.date()),
        stamp.format("%Y%m%dT%H%M")
    )
}

pub fn prefix_for(day: NaiveDate) -> String {
    format!("{}/OPERA/COMP/", day.format("%Y/%m/%d"))
}

// Measured B codes reserve 0/1 for non-measured cells, matching the grid
// shader's raw >= 2 guard. Half-dBZ bins cover -32 through 94.5 dBZ.
const CODE: MeasuredCode = MeasuredCode {
    scale: 2.0,
    offset: 66.0,
};

/// Classify OPERA DBZH floats into the grid texture encoding.
pub fn classify(values: &[f32], nodata: Option<f32>, bounds: &[f64], classes: usize) -> Vec<u8> {
    let classes = classes.max(1);
    let fill = nodata.unwrap_or(FILL);
    let mut pixels = Vec::with_capacity(values.len() * 4);
    for &v in values {
        let (r, g, b) = if v.is_nan() {
            (0, 2, 0) // undetect
        } else if (v - fill).abs() < 1.0 || v <= fill / 2.0 {
            // Missing / nodata (G bit 0). Grid shader draws transparent —
            // nodata draws nothing; oceans must not use the polar folded hatch.
            (0, 1, 0)
        } else {
            let class = class_of(v as f64, bounds, classes);
            // Floor rather than round: a value just below a half-dBZ
            // threshold must remain below its code cutoff. Only B is quantized;
            // the palette class still comes from the original measured value.
            let raw = (f64::from(v) * f64::from(CODE.scale) + f64::from(CODE.offset))
                .floor()
                .clamp(2.0, 255.0) as u8;
            (class + 1, 0, raw)
        };
        pixels.extend_from_slice(&[r, g, b, 255]);
    }
    pixels
}

fn class_of(value: f64, bounds: &[f64], classes: usize) -> u8 {
    if bounds.len() < 2 {
        return 0;
    }
    // Sorted `[lo, hi)` bins: insertion point minus one, clamped so values
    // at/above the last edge stay in the last interval (not a new class).
    bounds
        .partition_point(|&edge| edge <= value)
        .saturating_sub(1)
        .min(bounds.len() - 2)
        .min(classes - 1) as u8
}

/// Decode a COMP DBZH COG into a mosaic frame + PNG texture.
#[cfg(test)]
pub fn decode_frame(
    bytes: &[u8],
    stamp: NaiveDateTime,
    palette: &[String],
    bounds: &[f64],
) -> Result<(MosaicFrame, Vec<u8>, i64), String> {
    let raster = cog::decode_float_cog(bytes)?;
    validate_opera_georef(&raster)?;
    let (frame, png, start_ms, _) = finish_frame(raster, stamp, palette, bounds, Duration::ZERO)?;
    Ok((frame, png, start_ms))
}

struct CpuStages {
    decode: Duration,
    classify: Duration,
    png: Duration,
}

struct Timings {
    download: Duration,
    cpu: CpuStages,
}

struct Loaded {
    object: CompObject,
    frame: MosaicFrame,
    texture: Vec<u8>,
    start_ms: i64,
    timings: Timings,
}

struct Fetched {
    object: CompObject,
    bytes: Vec<u8>,
    download: Duration,
}

/// Drop the COG bytes as soon as the raster exists so a 12 MiB body is
/// not held through classify + PNG of a 3800×4400 frame.
fn decode_owned(
    bytes: Vec<u8>,
    stamp: NaiveDateTime,
    palette: &[String],
    bounds: &[f64],
) -> Result<(MosaicFrame, Vec<u8>, i64, CpuStages), String> {
    let started = Instant::now();
    let raster = cog::decode_float_cog(&bytes)?;
    validate_opera_georef(&raster)?;
    drop(bytes);
    finish_frame(raster, stamp, palette, bounds, started.elapsed())
}

fn finish_frame(
    raster: DecodedRaster,
    stamp: NaiveDateTime,
    palette: &[String],
    bounds: &[f64],
    decode: Duration,
) -> Result<(MosaicFrame, Vec<u8>, i64, CpuStages), String> {
    let started = Instant::now();
    let pixels = classify(&raster.values, raster.nodata, bounds, palette.len());
    let width = raster.width;
    let height = raster.height;
    let geotransform = raster.geotransform;
    drop(raster);
    let classify_time = started.elapsed();
    let started = Instant::now();
    let png =
        sweep::png(width, height, &pixels).map_err(|e| format!("encoding OPERA texture: {e}"))?;
    drop(pixels);
    let png_time = started.elapsed();
    let scan_time = stamp.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let start_ms = stamp.and_utc().timestamp_millis();
    let frame = MosaicFrame {
        id: format!("{ID}-{}", stamp.format("%Y%m%dT%H%M%SZ")),
        product: "REF".into(),
        product_name: "Reflectivity".into(),
        units: "dBZ".into(),
        scan_time,
        sweep_end: None,
        status: FrameStatus::Complete,
        texture: String::new(),
        width,
        height,
        crs: Opera::crs(),
        geotransform,
        palette: palette.to_vec(),
        bounds: bounds.to_vec(),
        measured_code: Some(CODE),
    };
    Ok((
        frame,
        png,
        start_ms,
        CpuStages {
            decode,
            classify: classify_time,
            png: png_time,
        },
    ))
}

fn opera_log(message: impl std::fmt::Display) {
    eprintln!("{} OPERA {message}", Utc::now().to_rfc3339());
}

fn log_load(kind: &str, loaded: &Loaded) {
    let stamp = loaded.object.stamp.format("%Y%m%dT%H%M");
    let t = &loaded.timings;
    if kind.is_empty() {
        opera_log(format_args!(
            "{stamp}: download {:.0?} decode {:.0?} classify {:.0?} png {:.0?}",
            t.download, t.cpu.decode, t.cpu.classify, t.cpu.png
        ));
    } else {
        opera_log(format_args!(
            "{kind} {stamp}: download {:.0?} decode {:.0?} classify {:.0?} png {:.0?}",
            t.download, t.cpu.decode, t.cpu.classify, t.cpu.png
        ));
    }
}

async fn decode_blocking(
    bytes: Vec<u8>,
    stamp: NaiveDateTime,
    palette: Arc<Vec<String>>,
    bounds: Arc<Vec<f64>>,
) -> Result<(MosaicFrame, Vec<u8>, i64, CpuStages), String> {
    spawn_blocking(move || decode_owned(bytes, stamp, &palette, &bounds))
        .await
        .unwrap_or_else(|e| Err(format!("decode {}: {e}", stamp.format("%Y%m%dT%H%M"))))
}

async fn fetch_one<G, GF>(obj: CompObject, get: Arc<G>) -> Result<Fetched, String>
where
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
{
    let started = Instant::now();
    let bytes = (*get)(format!("{}/{}", obj.host, obj.key)).await?;
    Ok(Fetched {
        object: obj,
        bytes,
        download: started.elapsed(),
    })
}

async fn finish_load(
    fetched: Fetched,
    palette: Arc<Vec<String>>,
    bounds: Arc<Vec<f64>>,
    decode_slots: Option<&Semaphore>,
) -> Result<Loaded, String> {
    let _permit = match decode_slots {
        Some(slots) => Some(slots.acquire().await.map_err(|e| format!("decode: {e}"))?),
        None => None,
    };
    let Fetched {
        object,
        bytes,
        download,
    } = fetched;
    let (frame, texture, start_ms, cpu) =
        decode_blocking(bytes, object.stamp, palette, bounds).await?;
    Ok(Loaded {
        object,
        frame,
        texture,
        start_ms,
        timings: Timings { download, cpu },
    })
}

async fn load_one<G, GF>(
    obj: CompObject,
    get: Arc<G>,
    palette: Arc<Vec<String>>,
    bounds: Arc<Vec<f64>>,
    decode_slots: Option<Arc<Semaphore>>,
) -> Result<Loaded, String>
where
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
{
    let fetched = fetch_one(obj, get).await?;
    finish_load(fetched, palette, bounds, decode_slots.as_deref()).await
}

enum Newest {
    Fresh(Box<Loaded>),
    Known,
    Empty,
}

/// Load the newest unseen COMP from the freshest store that serves a
/// readable copy. A failure is reported unless another store delivers.
async fn load_newest<L, G, LF, GF>(
    known: &HashSet<String>,
    list: L,
    get: G,
    palette: Arc<Vec<String>>,
    bounds: Arc<Vec<f64>>,
    decode_slots: Option<Arc<Semaphore>>,
) -> Result<Newest, String>
where
    L: Fn(reqwest::Url) -> LF,
    LF: Future<Output = Result<String, String>>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
{
    let get = Arc::new(get);
    let mut failure = None;
    let mut outcome = Newest::Empty;
    for objects in list_hosts(Utc::now().naive_utc(), list).await? {
        let newest = objects[objects.len() - 1].clone();
        if known.contains(&newest.key) {
            outcome = Newest::Known;
            break;
        }
        let loaded = load_one(
            newest,
            Arc::clone(&get),
            Arc::clone(&palette),
            Arc::clone(&bounds),
            decode_slots.clone(),
        )
        .await;
        match loaded {
            Ok(loaded) => return Ok(Newest::Fresh(Box::new(loaded))),
            Err(reason) => failure = Some(reason),
        }
    }
    failure.map_or(Ok(outcome), Err)
}

fn validate_opera_georef(raster: &DecodedRaster) -> Result<(), String> {
    if raster.geo_double_params.len() >= 4 {
        let (lat0, lon0, fe, fnorth) = (
            raster.geo_double_params[0],
            raster.geo_double_params[1],
            raster.geo_double_params[2],
            raster.geo_double_params[3],
        );
        if (lat0 - LAT0_DEG).abs() > 1e-6
            || (lon0 - LON0_DEG).abs() > 1e-6
            || (fe - FALSE_EASTING_M).abs() > 1e-3
            || (fnorth - FALSE_NORTHING_M).abs() > 1e-3
        {
            return Err(format!(
                "unexpected OPERA LAEA params lat0={lat0} lon0={lon0} fe={fe} fn={fnorth}"
            ));
        }
    }
    if (raster.geotransform[1] - 1000.0).abs() > 1e-6
        || (raster.geotransform[5] + 1000.0).abs() > 1e-6
    {
        return Err(format!(
            "unexpected OPERA pixel size {:?}",
            raster.geotransform
        ));
    }
    Ok(())
}

/// Each host's recent COMP objects, oldest first, freshest host first.
/// Hosts with none are left out; an error surfaces only when no host has any.
pub async fn list_hosts<L, LF>(now: NaiveDateTime, list: L) -> Result<Vec<Vec<CompObject>>, String>
where
    L: Fn(reqwest::Url) -> LF,
    LF: Future<Output = Result<String, String>>,
{
    let mut found = Vec::new();
    let mut failures = Vec::new();
    for host in HOSTS {
        match list_window(host, now, &list).await {
            Ok(mut objects) => {
                objects.retain(|o| o.stamp <= now + FUTURE_SLACK);
                if !objects.is_empty() {
                    found.push(objects);
                }
            }
            Err(reason) => failures.push(reason),
        }
    }
    if found.is_empty() && !failures.is_empty() {
        return Err(failures.join("; "));
    }
    found.sort_by_key(|objects| std::cmp::Reverse(objects[objects.len() - 1].stamp));
    Ok(found)
}

/// History is the served store's scans older than the first live frame.
pub fn backfill_store(
    hosts: Vec<Vec<CompObject>>,
    served: &str,
    before: NaiveDateTime,
) -> Vec<CompObject> {
    let mut objects = hosts
        .into_iter()
        .find(|objects| objects[0].host == served)
        .unwrap_or_default();
    objects.retain(|o| o.stamp < before);
    objects
}

/// Newest `limit` COMP keys that are not already `known`, oldest first so
/// the timeline fills in order like NEXRAD backfill.
pub fn backfill_targets(
    objects: &[CompObject],
    known: &HashSet<String>,
    limit: usize,
) -> Vec<CompObject> {
    let start = objects.len().saturating_sub(limit);
    objects[start..]
        .iter()
        .filter(|o| !known.contains(&o.key))
        .cloned()
        .collect()
}

/// One list+fetch cycle with injectable HTTP (tests mock the closures).
#[cfg(test)]
pub async fn refresh<L, G, LF, GF>(
    known: &HashSet<String>,
    list: L,
    get: G,
    palette: &[String],
    bounds: &[f64],
) -> Result<Option<(CompObject, MosaicFrame, Vec<u8>, i64)>, String>
where
    L: Fn(reqwest::Url) -> LF,
    LF: Future<Output = Result<String, String>>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
{
    Ok(
        match load_newest(
            known,
            list,
            get,
            Arc::new(palette.to_vec()),
            Arc::new(bounds.to_vec()),
            None,
        )
        .await?
        {
            Newest::Fresh(loaded) => {
                Some((loaded.object, loaded.frame, loaded.texture, loaded.start_ms))
            }
            Newest::Known | Newest::Empty => None,
        },
    )
}

/// Fetch up to `HISTORY_MAX` recent COMP frames the live poll has not
/// already delivered. Failures on one object skip it; a listing failure
/// ends the backfill.
async fn backfill_loop(
    events: GridSender,
    known: HashSet<String>,
    served: &'static str,
    before: NaiveDateTime,
    palette: Arc<Vec<String>>,
    bounds: Arc<Vec<f64>>,
    decode_slots: Arc<Semaphore>,
) {
    sleep(BACKFILL_DELAY).await;
    let objects = match list_hosts(Utc::now().naive_utc(), list_http).await {
        Ok(hosts) => backfill_store(hosts, served, before),
        Err(reason) => {
            opera_log(format_args!("backfill listing: {reason}"));
            return;
        }
    };
    let targets = backfill_targets(&objects, &known, HISTORY_MAX - 1);
    fill_history(events, targets, get_http, palette, bounds, decode_slots).await;
}

/// Fetch+decode up to `BACKFILL_IN_FLIGHT` objects at a time, but emit
/// `Event::Backfill` oldest-first. Aborting this task (or a closed
/// receiver) aborts every child, including the one currently joined.
async fn fill_history<G, GF>(
    events: GridSender,
    targets: Vec<CompObject>,
    get: G,
    palette: Arc<Vec<String>>,
    bounds: Arc<Vec<f64>>,
    decode_slots: Arc<Semaphore>,
) where
    G: Fn(String) -> GF + Send + Sync + 'static,
    GF: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
{
    let get = Arc::new(get);
    let mut pending = targets.into_iter();
    let mut inflight: VecDeque<AbortOnDrop<Result<Loaded, String>>> = VecDeque::new();
    loop {
        if events.is_closed() {
            return;
        }
        while inflight.len() < BACKFILL_IN_FLIGHT {
            let Some(obj) = pending.next() else {
                break;
            };
            let get = Arc::clone(&get);
            let palette = Arc::clone(&palette);
            let bounds = Arc::clone(&bounds);
            let decode_slots = Arc::clone(&decode_slots);
            inflight.push_back(AbortOnDrop(tokio::spawn(async move {
                let key = obj.key.clone();
                load_one(obj, get, palette, bounds, Some(decode_slots))
                    .await
                    .map_err(|reason| format!("{key}: {reason}"))
            })));
        }
        let Some(handle) = inflight.pop_front() else {
            break;
        };
        match handle.join().await {
            Ok(Ok(loaded)) => {
                log_load("backfill", &loaded);
                if events
                    .send(ev_backfill(loaded.frame, loaded.texture, loaded.start_ms))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(Err(reason)) => {
                opera_log(format_args!("backfill {reason}"));
            }
            Err(e) if e.is_cancelled() => {}
            Err(e) => opera_log(format_args!("backfill task: {e}")),
        }
    }
}

/// Aborts its task when dropped, including while `.join()` is pending, so
/// replacing the poller mid-backfill takes child fetch/decode tasks with it.
struct AbortOnDrop<T>(JoinHandle<T>);
impl<T> AbortOnDrop<T> {
    async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        (&mut self.0).await
    }
}
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| build_client(REQUEST_TIMEOUT));
    &CLIENT
}

fn build_client(limit: Duration) -> reqwest::Client {
    let mut builder =
        reqwest::Client::builder()
            .timeout(limit)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let home = attempt.previous().first().map(|url| url.origin());
                if attempt.previous().len() > 10 {
                    attempt.error("too many redirects")
                } else if home == Some(attempt.url().origin()) {
                    attempt.follow()
                } else {
                    attempt.error("redirect leaves the OPERA store")
                }
            }));
    if cfg!(test) {
        builder = builder.no_proxy();
    }
    builder.build().expect("OPERA HTTP client")
}

async fn get_body(client: &reqwest::Client, url: &str, max: usize) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("fetching OPERA {url}: {e}"))?;
    live_index::take_body(response, max)
        .await
        .map_err(|e| format!("reading OPERA {url}: {e}"))
}

async fn list_http(url: reqwest::Url) -> Result<String, String> {
    let bytes = get_body(http_client(), url.as_str(), live_index::LISTING_MAX).await?;
    String::from_utf8(bytes).map_err(|e| format!("reading OPERA listing: {e}"))
}

async fn get_http(url: String) -> Result<Vec<u8>, String> {
    get_body(http_client(), &url, BODY_MAX).await
}

async fn poll_loop(
    events: GridSender,
    mut known: HashSet<String>,
    palette: Vec<String>,
    bounds: Vec<f64>,
) {
    let palette = Arc::new(palette);
    let bounds = Arc::new(bounds);
    let decode_slots = Arc::new(Semaphore::new(BACKFILL_IN_FLIGHT));
    let mut first = true;
    let mut backfill = None;
    loop {
        if !first {
            sleep(IDLE).await;
        }
        let result = load_newest(
            &known,
            list_http,
            get_http,
            Arc::clone(&palette),
            Arc::clone(&bounds),
            Some(Arc::clone(&decode_slots)),
        )
        .await;
        let sent = match result {
            Ok(Newest::Fresh(loaded)) => {
                log_load("", &loaded);
                let (host, stamp) = (loaded.object.host, loaded.object.stamp);
                let saved = publish_live(&events, &mut known, *loaded).await;
                if saved == Ok(true) && backfill.is_none() {
                    // Held so aborting this poller cancels in-flight COMP downloads.
                    backfill = Some(AbortOnDrop(tokio::spawn(backfill_loop(
                        events.clone(),
                        known.clone(),
                        host,
                        stamp,
                        Arc::clone(&palette),
                        Arc::clone(&bounds),
                        Arc::clone(&decode_slots),
                    ))));
                }
                saved.map(|_| ())
            }
            Ok(Newest::Known) => events
                .send(GridEvent::Online {
                    source_id: ID.to_owned(),
                })
                .await
                .map_err(|_| ()),
            Ok(Newest::Empty) => events
                .send(ev_silent(format!(
                    "ORD cache has no OPERA DBZH.tiff from the last {LOOKBACK_MINUTES} minutes"
                )))
                .await
                .map_err(|_| ()),
            Err(reason) => events.send(ev_offline(reason)).await.map_err(|_| ()),
        };
        first = false;
        if sent.is_err() {
            return;
        }
    }
}

/// Send a live frame. Its key counts as shown only once the engine saved it,
/// so a frame the engine could not store is fetched again next poll.
async fn publish_live(
    events: &GridSender,
    known: &mut HashSet<String>,
    loaded: Loaded,
) -> Result<bool, ()> {
    let key = loaded.object.key.clone();
    let slot = events.reserve().await.map_err(|_| ())?;
    let saved = slot
        .send(ev_frame(loaded.frame, loaded.texture, loaded.start_ms))
        .await
        .unwrap_or(false);
    if saved {
        known.insert(key.clone());
        if known.len() > HISTORY_MAX * 4 {
            *known = HashSet::from([key]);
        }
    }
    Ok(saved)
}

/// Tiny synthetic COMP-shaped COG for decoder / fetch tests (not live data).
#[cfg(test)]
pub fn synthetic_fixture_cog() -> Vec<u8> {
    let width = 32u32;
    let height = 32u32;
    let mut band0 = vec![f32::NAN; (width * height) as usize];
    let band1 = vec![FILL; band0.len()];
    // A few measured cells and one nodata.
    band0[0] = 5.0;
    band0[1] = FILL;
    band0[2] = 35.0;
    band0[16 * 32 + 16] = 45.0;
    let gt = [-500.0, 1000.0, 0.0, 500.0, 0.0, -1000.0];
    let doubles = [
        LAT0_DEG,
        LON0_DEG,
        FALSE_EASTING_M,
        FALSE_NORTHING_M,
        298.257223563,
        6_378_137.0,
    ];
    cog::write_float_cog(width, height, 16, 16, gt, &doubles, FILL, &band0, &band1)
        .expect("synthetic OPERA fixture")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AdapterTarget, FrameWire, GeoPoint, Selection};
    use crate::source::{Candidate, SourceRegistry, covering_selection};

    #[test]
    fn loading_frame_is_blank_mosaic_chrome() {
        let opera = Opera::new();
        let frame = opera.loading_frame();
        assert_eq!(frame.id, "opera-loading");
        assert!(frame.scan_time.is_empty());
        assert_eq!(frame.status, FrameStatus::Partial);
        assert_eq!(frame.measured_code, None);
        let wire = serde_json::to_value(FrameWire::Mosaic(frame.clone())).unwrap();
        for key in ["scale", "offset", "measuredCode"] {
            assert!(wire.get(key).is_none(), "loading frame carries {key}");
        }
        assert_eq!(serde_json::from_value::<MosaicFrame>(wire).unwrap(), frame);
        assert_eq!(frame.width, 1);
        assert_eq!(frame.height, 1);
        assert!(!frame.palette.is_empty());
        assert_eq!(frame.bounds.len(), frame.palette.len() + 1);
        let png = Opera::loading_texture().unwrap();
        assert!(!png.is_empty());
    }

    #[test]
    fn listing_keeps_only_dbzh_tiffs() {
        let body = r#"<ListBucketResult>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1200@0@DBZH.h5</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1200@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1205@0@RATE.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1210@0@DBZH.tiff</Key></Contents>
            </ListBucketResult>"#;
        let objects = parse_dbzh_listing(HOSTS[0], body).unwrap().objects;
        assert_eq!(objects.len(), 2);
        assert_eq!(
            objects[0].key,
            "2026/09/17/OPERA/COMP/OPERA@20260917T1200@0@DBZH.tiff"
        );
        assert_eq!(
            objects[1].stamp,
            NaiveDate::from_ymd_opt(2026, 9, 17)
                .unwrap()
                .and_hms_opt(12, 10, 0)
                .unwrap()
        );
    }

    #[test]
    fn decoder_classifies_synthetic_fixture() {
        let bytes = synthetic_fixture_cog();
        let stamp = NaiveDate::from_ymd_opt(2026, 9, 17)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let opera = Opera::new();
        let (frame, png, start_ms) =
            decode_frame(&bytes, stamp, &opera.palette, &opera.bounds).unwrap();
        assert_eq!(frame.width, 32);
        assert_eq!(frame.height, 32);
        assert!(matches!(
            frame.crs,
            Crs::LambertAzimuthalEqualArea { lat0_deg: 55.0, .. }
        ));
        assert_eq!(frame.product_name, "Reflectivity");
        assert_eq!(frame.units, "dBZ");
        let wire = serde_json::to_value(FrameWire::Mosaic(frame)).unwrap();
        assert_eq!(wire["kind"], "mosaic");
        // Current clients read top-level scale > 0 as the floor being in force.
        assert!(wire.get("scale").is_none());
        assert!(wire.get("offset").is_none());
        assert_eq!(
            wire["measuredCode"],
            serde_json::json!({"scale": 2.0, "offset": 66.0})
        );
        assert_eq!(start_ms, stamp.and_utc().timestamp_millis());
        let mut reader = png::Decoder::new(std::io::Cursor::new(&png))
            .read_info()
            .unwrap();
        let info = reader.info();
        assert_eq!((info.width, info.height), (32, 32));
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut decoded).unwrap();
        assert_eq!(decoded[2], 76); // the emitted PNG retains 5 dBZ in B
        let (scale, offset) = (
            wire["measuredCode"]["scale"].as_f64().unwrap(),
            wire["measuredCode"]["offset"].as_f64().unwrap(),
        );
        assert_eq!((f64::from(decoded[2]) - offset) / scale, 5.0);
        assert_eq!(&decoded[4..8], &[0, 1, 0, 255]);
        let pixels = classify(
            &cog::decode_float_cog(&bytes).unwrap().values,
            Some(FILL),
            &opera.bounds,
            opera.palette.len(),
        );
        assert_eq!(pixels[2], 76); // 5 dBZ measured code
        assert_eq!(pixels[1], 0); // measured G
        assert!(pixels[0] >= 1);
        assert_eq!(pixels[4], 0);
        assert_eq!(pixels[5], 1); // nodata/missing
        assert_eq!(pixels[12], 0);
        assert_eq!(pixels[13], 2); // undetect nan
    }

    #[test]
    fn measured_codes_keep_floor_boundary_and_special_cells_distinct() {
        let opera = Opera::new();
        let values = [
            -40.0,
            -32.0,
            0.0,
            4.5,
            f32::from_bits(5.0f32.to_bits() - 1),
            5.0,
            5.5,
            100.0,
            FILL,
            f32::NAN,
        ];
        let pixels = classify(&values, Some(FILL), &opera.bounds, opera.palette.len());
        let (cells, remainder) = pixels.as_chunks::<4>();
        assert!(remainder.is_empty());
        let cutoff = (5.0 * CODE.scale + CODE.offset).ceil() as u8;
        assert_eq!(cutoff, 76);
        for (i, cell) in cells[..8].iter().enumerate() {
            assert_eq!(
                cell[0],
                class_of(values[i] as f64, &opera.bounds, opera.palette.len()) + 1
            );
            assert_eq!(cell[1], 0);
            assert!(cell[2] >= 2);
            assert_eq!(cell[2] < cutoff, values[i] < 5.0);
            assert_eq!(cell[3], 255);
        }
        assert_eq!(cells[0][2], 2);
        assert_eq!(cells[7][2], 255);
        assert_eq!(cells[8], [0, 1, 0, 255]);
        assert_eq!(cells[9], [0, 2, 0, 255]);
    }

    /// The previous linear scan, kept so the `partition_point` mapping cannot
    /// drift from below-min / `[lo, hi)` / at-max / clamp behavior.
    fn class_of_linear(value: f64, bounds: &[f64], classes: usize) -> u8 {
        if bounds.len() < 2 {
            return 0;
        }
        let mut above = 0usize;
        for (i, edge) in bounds.iter().enumerate().skip(1) {
            if value >= bounds[i - 1] && value < *edge {
                return (i - 1).min(classes - 1) as u8;
            }
            if value >= *edge {
                above = i;
            }
        }
        above.saturating_sub(1).min(classes - 1) as u8
    }

    #[test]
    fn class_of_matches_linear_scan_on_edges() {
        let bounds = [-32.0, 0.0, 10.0, 20.0, 30.0, 40.0, 45.0];
        let values = [
            f64::NEG_INFINITY,
            -32.0 - f64::EPSILON,
            -32.0,
            -16.0,
            -0.0,
            0.0,
            9.999,
            10.0,
            20.0,
            45.0,
            45.0 + f64::EPSILON,
            100.0,
            f64::INFINITY,
        ];
        for classes in [1usize, 3, 6, 12] {
            assert_eq!(class_of(0.0, &[], classes), 0);
            assert_eq!(class_of(0.0, &[0.0], classes), 0);
            assert_eq!(class_of_linear(0.0, &[], classes), 0);
            assert_eq!(class_of_linear(0.0, &[0.0], classes), 0);
            for &value in &values {
                assert_eq!(
                    class_of(value, &bounds, classes),
                    class_of_linear(value, &bounds, classes),
                    "value={value} classes={classes}"
                );
            }
        }
        // Last interval is class 5 for 7 edges; extra palette classes do not
        // invent a bin past the maximum, and too few classes clamp down.
        assert_eq!(class_of(-40.0, &bounds, 12), 0);
        assert_eq!(class_of(-32.0, &bounds, 12), 0);
        assert_eq!(class_of(0.0, &bounds, 12), 1);
        assert_eq!(class_of(10.0, &bounds, 12), 2);
        assert_eq!(class_of(45.0, &bounds, 12), 5);
        assert_eq!(class_of(99.0, &bounds, 12), 5);
        assert_eq!(class_of(10.0, &bounds, 2), 1);
        assert_eq!(class_of(99.0, &bounds, 1), 0);
    }

    #[test]
    fn refresh_fetches_newest_unseen_with_mocked_http() {
        let fixture = synthetic_fixture_cog();
        let opera = Opera::new();
        let listing = r#"<ListBucketResult>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1200@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1205@0@DBZH.tiff</Key></Contents>
            </ListBucketResult>"#;
        let known = HashSet::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let got = runtime
            .block_on(refresh(
                &known,
                |_url| async { Ok(listing.to_owned()) },
                |key| {
                    let fixture = fixture.clone();
                    async move {
                        assert!(key.ends_with("OPERA@20260917T1205@0@DBZH.tiff"));
                        Ok(fixture)
                    }
                },
                &opera.palette,
                &opera.bounds,
            ))
            .unwrap()
            .unwrap();
        assert!(got.0.key.ends_with("1205@0@DBZH.tiff"));
        assert_eq!(got.1.width, 32);
        let mut known = HashSet::new();
        known.insert(got.0.key.clone());
        let again = runtime
            .block_on(refresh(
                &known,
                |_url| async { Ok(listing.to_owned()) },
                |_key| async { panic!("should not refetch") },
                &opera.palette,
                &opera.bounds,
            ))
            .unwrap();
        assert!(again.is_none());
    }

    #[test]
    fn backfill_skips_known_and_keeps_a_dozen() {
        let listing = r#"<ListBucketResult>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1100@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1105@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1110@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1115@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1120@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1125@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1130@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1135@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1140@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1145@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1150@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1155@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1200@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1205@0@DBZH.tiff</Key></Contents>
            </ListBucketResult>"#;
        let objects = parse_dbzh_listing(HOSTS[0], listing).unwrap().objects;
        assert_eq!(objects.len(), 14);
        let mut known = HashSet::new();
        known.insert("2026/09/17/OPERA/COMP/OPERA@20260917T1205@0@DBZH.tiff".into());
        let targets = backfill_targets(&objects, &known, HISTORY_MAX);
        assert_eq!(targets.len(), 11);
        assert!(targets[0].key.ends_with("1110@0@DBZH.tiff"));
        assert!(targets[10].key.ends_with("1200@0@DBZH.tiff"));
        assert!(
            !targets
                .iter()
                .any(|o| o.key.contains("1100") || o.key.contains("1105"))
        );
    }

    #[test]
    fn opera_covers_london_and_loses_to_polar() {
        let registry = SourceRegistry::compiled();
        let london = GeoPoint {
            lat: 51.5,
            lon: -0.1,
        };
        let picked = registry.covering_selection(london, None).unwrap();
        assert_eq!(picked.source_id, ID);
        assert_eq!(picked.target, AdapterTarget::Mosaic);

        let ktlx = GeoPoint {
            lat: 35.333,
            lon: -97.278,
        };
        let picked = registry.covering_selection(ktlx, None).unwrap();
        assert_eq!(picked.source_id, "nexrad");
    }

    #[test]
    fn national_priority_preempts_opera_when_both_cover() {
        let opera = Candidate {
            selection: Selection {
                source_id: ID.into(),
                target: AdapterTarget::Mosaic,
            },
            family: Family::Grid,
            product: ProductClass::Reflectivity,
            priority: SELECTION_PRIORITY,
            coverage: Coverage::Box {
                north: COVERAGE_NORTH,
                south: COVERAGE_SOUTH,
                east: COVERAGE_EAST,
                west: COVERAGE_WEST,
            },
            dish: None,
        };
        let national = Candidate {
            selection: Selection {
                source_id: "italy-dpc".into(),
                target: AdapterTarget::Mosaic,
            },
            family: Family::Grid,
            product: ProductClass::Reflectivity,
            priority: 50,
            coverage: Coverage::Box {
                north: 47.0,
                south: 36.0,
                east: 19.0,
                west: 6.0,
            },
            dish: None,
        };
        let rome = GeoPoint {
            lat: 41.9,
            lon: 12.5,
        };
        assert_eq!(
            covering_selection(rome, None, &[opera.clone(), national.clone()]).map(|s| s.source_id),
            Some("italy-dpc".into())
        );
        let held_opera = Selection {
            source_id: ID.into(),
            target: AdapterTarget::Mosaic,
        };
        assert_eq!(
            covering_selection(rome, Some(&held_opera), &[opera, national]).map(|s| s.source_id),
            Some("italy-dpc".into()),
            "higher priority national preempts held OPERA"
        );
    }

    #[test]
    fn attribution_and_hello_metadata() {
        let opera = Opera::new();
        let meta = opera.metadata();
        assert_eq!(meta.attribution, ATTRIBUTION);
        assert_eq!(meta.name, NAME);
        assert_eq!(meta.default_product_class, ProductClass::Reflectivity);
    }

    fn current_thread() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn listing(keys: &[&str], next: Option<&str>) -> String {
        let mut body = String::from("<ListBucketResult>");
        for stamp in keys {
            let day = format!("{}/{}/{}", &stamp[..4], &stamp[4..6], &stamp[6..8]);
            body += &format!(
                "<Contents><Key>{day}/OPERA/COMP/OPERA@{stamp}@0@DBZH.tiff</Key></Contents>"
            );
        }
        if let Some(next) = next {
            body += &format!(
                "<IsTruncated>true</IsTruncated><NextContinuationToken>{next}</NextContinuationToken>"
            );
        }
        body + "</ListBucketResult>"
    }

    fn by_host(
        original: &'static [&'static str],
        backup: &'static [&'static str],
    ) -> impl Fn(reqwest::Url) -> std::future::Ready<Result<String, String>> {
        move |url| {
            let keys = if url.as_str().starts_with(HOSTS[0]) {
                original
            } else {
                backup
            };
            std::future::ready(Ok(listing(keys, None)))
        }
    }

    fn loaded_url(
        known: &[&str],
        list: impl Fn(reqwest::Url) -> std::future::Ready<Result<String, String>>,
        body: impl Fn(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Option<String>, String> {
        let known = known.iter().map(|k| k.to_string()).collect();
        let opera = Opera::new();
        let loaded = current_thread().block_on(refresh(
            &known,
            list,
            |url| std::future::ready(body(&url)),
            &opera.palette,
            &opera.bounds,
        ))?;
        Ok(loaded.map(|(object, ..)| format!("{}/{}", object.host, object.key)))
    }

    fn served(_url: &str) -> Result<Vec<u8>, String> {
        Ok(synthetic_fixture_cog())
    }

    fn now() -> NaiveDateTime {
        parse_dbzh_key("OPERA@20261120T2010@0@DBZH.tiff").unwrap()
    }

    #[test]
    fn empty_original_host_uses_the_backup() {
        let url = loaded_url(&[], by_host(&[], &["20261120T2005"]), served)
            .unwrap()
            .unwrap();
        assert!(url.starts_with(HOSTS[1]), "{url}");
        assert!(url.ends_with("OPERA@20261120T2005@0@DBZH.tiff"), "{url}");
    }

    #[test]
    fn original_host_with_files_is_used() {
        let url = loaded_url(
            &[],
            by_host(&["20261120T2005"], &["20261120T2005"]),
            |url| {
                assert!(url.starts_with(HOSTS[0]), "{url}");
                served(url)
            },
        )
        .unwrap()
        .unwrap();
        assert!(url.starts_with(HOSTS[0]), "{url}");
    }

    #[test]
    fn fresher_host_wins_over_a_stalled_one() {
        let url = loaded_url(&[], by_host(&["20261120T1900"], &["20261120T2005"]), served)
            .unwrap()
            .unwrap();
        assert!(url.starts_with(HOSTS[1]), "{url}");
        assert!(url.ends_with("OPERA@20261120T2005@0@DBZH.tiff"), "{url}");
    }

    #[test]
    fn failed_download_falls_back_to_the_other_host() {
        let url = loaded_url(
            &[],
            by_host(&["20261120T2005"], &["20261120T2005"]),
            |url| {
                if url.starts_with(HOSTS[0]) {
                    Err("404".to_owned())
                } else {
                    served(url)
                }
            },
        )
        .unwrap()
        .unwrap();
        assert!(url.starts_with(HOSTS[1]), "{url}");
    }

    #[test]
    fn unreadable_frame_falls_back_to_the_other_host() {
        let url = loaded_url(
            &[],
            by_host(&["20261120T2005"], &["20261120T2005"]),
            |url| {
                if url.starts_with(HOSTS[0]) {
                    Ok(b"not a tiff".to_vec())
                } else {
                    served(url)
                }
            },
        )
        .unwrap()
        .unwrap();
        assert!(url.starts_with(HOSTS[1]), "{url}");
    }

    #[test]
    fn failed_newer_frame_is_reported_when_the_other_store_has_only_known_ones() {
        let result = loaded_url(
            &["2026/11/20/OPERA/COMP/OPERA@20261120T2000@0@DBZH.tiff"],
            by_host(&["20261120T2005"], &["20261120T2000"]),
            |url| {
                if url.starts_with(HOSTS[0]) {
                    Err("503".to_owned())
                } else {
                    served(url)
                }
            },
        );
        assert!(result.unwrap_err().contains("503"));
        let unchanged = loaded_url(
            &["2026/11/20/OPERA/COMP/OPERA@20261120T2005@0@DBZH.tiff"],
            by_host(&["20261120T2005"], &["20261120T2005"]),
            |_url| panic!("known frame must not be downloaded"),
        );
        assert_eq!(unchanged, Ok(None));
    }

    #[test]
    fn history_comes_from_the_store_that_served_live() {
        let original = vec![CompObject {
            host: HOSTS[0],
            ..comp_on("2026/11/20", "20261120T2005")
        }];
        let backup = vec![
            comp_on("2026/11/20", "20261120T1955"),
            comp_on("2026/11/20", "20261120T2000"),
            comp_on("2026/11/20", "20261120T2005"),
        ];
        let first_live = backup[1].stamp;
        let hosts = vec![original, backup.clone()];
        assert_eq!(
            backfill_store(hosts.clone(), HOSTS[1], first_live),
            backup[..1]
        );
        assert!(backfill_store(hosts[..1].to_vec(), HOSTS[1], first_live).is_empty());
    }

    #[test]
    fn live_frame_counts_as_shown_only_once_the_engine_saved_it() {
        let opera = Opera::new();
        current_thread().block_on(async {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let events = GridSender::new(tx, 1);
            let engine = tokio::spawn(async move {
                for saved in [false, true] {
                    let message = rx.recv().await.unwrap();
                    message.published.unwrap().send(saved).unwrap();
                }
            });
            let mut known = HashSet::new();
            for expected in [false, true] {
                let Newest::Fresh(loaded) = load_newest(
                    &known,
                    by_host(&[], &["20261120T2005"]),
                    |url| std::future::ready(served(&url)),
                    Arc::new(opera.palette.clone()),
                    Arc::new(opera.bounds.clone()),
                    None,
                )
                .await
                .unwrap() else {
                    panic!("an unsaved frame is offered again");
                };
                assert_eq!(
                    publish_live(&events, &mut known, *loaded).await,
                    Ok(expected)
                );
            }
            engine.await.unwrap();
            assert_eq!(
                known,
                HashSet::from(["2026/11/20/OPERA/COMP/OPERA@20261120T2005@0@DBZH.tiff".to_owned()])
            );
        });
    }

    #[test]
    fn known_newest_and_empty_window_are_reported_apart() {
        let opera = Opera::new();
        let newest = |known: HashSet<String>, original: &'static [&'static str]| {
            current_thread().block_on(load_newest(
                &known,
                by_host(original, &[]),
                |url| std::future::ready(served(&url)),
                Arc::new(opera.palette.clone()),
                Arc::new(opera.bounds.clone()),
                None,
            ))
        };
        let shown =
            HashSet::from(["2026/11/20/OPERA/COMP/OPERA@20261120T2005@0@DBZH.tiff".to_owned()]);
        assert!(matches!(
            newest(shown, &["20261120T2005"]),
            Ok(Newest::Known)
        ));
        assert!(matches!(newest(HashSet::new(), &[]), Ok(Newest::Empty)));
    }

    #[test]
    fn future_stamped_scan_does_not_outrank_a_live_store() {
        let hosts = current_thread()
            .block_on(list_hosts(
                now(),
                by_host(&["20261120T2005"], &["20261120T2300"]),
            ))
            .unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0][0].host, HOSTS[0]);
        assert_eq!(hosts[0][0].stamp, comp("20261120T2005").stamp);
    }

    #[test]
    fn listing_skips_keys_outside_the_comp_path() {
        let body = r#"<ListBucketResult>
            <Contents><Key>2026/11/20/OPERA/COMP/../../../etc/OPERA@20261120T2000@0@DBZH.tiff</Key></Contents>
            <Contents><Key>/2026/11/20/OPERA/COMP/OPERA@20261120T2000@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/11/19/OPERA/COMP/OPERA@20261120T2000@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/11/20/OPERA/COMP/OPERA@20261120T2005@0@DBZH.tiff</Key></Contents>
            </ListBucketResult>"#;
        let objects = parse_dbzh_listing(HOSTS[0], body).unwrap().objects;
        assert_eq!(objects.len(), 1);
        assert_eq!(
            objects[0].key,
            "2026/11/20/OPERA/COMP/OPERA@20261120T2005@0@DBZH.tiff"
        );
    }

    #[test]
    fn listed_objects_download_from_the_host_that_listed_them() {
        let hosts = current_thread()
            .block_on(list_hosts(
                now(),
                by_host(&[], &["20261120T2000", "20261120T2005"]),
            ))
            .unwrap();
        assert_eq!(hosts.len(), 1);
        assert!(hosts[0].iter().all(|o| o.host == HOSTS[1]));
    }

    #[test]
    fn hanging_original_host_falls_back_to_backup() {
        let hosts = current_thread()
            .block_on(async {
                tokio::time::pause();
                let listed = list_hosts(now(), |url: reqwest::Url| async move {
                    if url.as_str().starts_with(HOSTS[0]) {
                        std::future::pending::<()>().await;
                    }
                    Ok(listing(&["20261120T2005"], None))
                });
                timeout(HOST_LIST_TIMEOUT * 8, listed)
                    .await
                    .expect("a stalled listing must time out")
            })
            .unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0][0].host, HOSTS[1]);
    }

    #[test]
    fn listing_failure_shows_when_no_host_has_files() {
        let result = current_thread().block_on(list_hosts(now(), |url: reqwest::Url| async move {
            if url.as_str().starts_with(HOSTS[0]) {
                Err("original down".to_owned())
            } else {
                Ok(listing(&[], None))
            }
        }));
        assert!(result.unwrap_err().contains("original down"));
        let empty = current_thread().block_on(list_hosts(now(), by_host(&[], &[])));
        assert!(empty.unwrap().is_empty());
    }

    #[test]
    fn listing_starts_ninety_minutes_back_across_midnight() {
        let urls = std::sync::Mutex::new(Vec::new());
        let after_midnight = parse_dbzh_key("OPERA@20261120T0030@0@DBZH.tiff").unwrap();
        current_thread()
            .block_on(list_hosts(after_midnight, |url: reqwest::Url| {
                if url.as_str().starts_with(HOSTS[0]) {
                    urls.lock()
                        .unwrap()
                        .push(url.query_pairs().into_owned().collect::<Vec<_>>());
                }
                std::future::ready(Ok(listing(&[], None)))
            }))
            .unwrap();
        let urls = urls.into_inner().unwrap();
        assert_eq!(urls.len(), 2);
        assert_eq!(param(&urls[0], "prefix").unwrap(), "2026/11/20/OPERA/COMP/");
        assert_eq!(param(&urls[0], "start-after"), None);
        assert_eq!(param(&urls[1], "prefix").unwrap(), "2026/11/19/OPERA/COMP/");
        assert_eq!(
            param(&urls[1], "start-after").unwrap(),
            "2026/11/19/OPERA/COMP/OPERA@20261119T2300"
        );
    }

    #[test]
    fn failing_yesterday_listing_keeps_todays_frames() {
        let after_midnight = parse_dbzh_key("OPERA@20261120T0010@0@DBZH.tiff").unwrap();
        let hosts = current_thread()
            .block_on(list_hosts(after_midnight, |url: reqwest::Url| {
                let query: Vec<_> = url.query_pairs().into_owned().collect();
                std::future::ready(if param(&query, "start-after").is_some() {
                    Err("yesterday timed out".to_owned())
                } else {
                    Ok(listing(&["20261120T0005"], None))
                })
            }))
            .unwrap();
        assert_eq!(
            hosts[0],
            vec![CompObject {
                host: HOSTS[0],
                ..comp_on("2026/11/20", "20261120T0005")
            }]
        );
    }

    #[test]
    fn stalled_yesterday_listing_keeps_todays_frames() {
        let after_midnight = parse_dbzh_key("OPERA@20261120T0010@0@DBZH.tiff").unwrap();
        let hosts = current_thread()
            .block_on(async {
                tokio::time::pause();
                let listed = list_hosts(after_midnight, |url: reqwest::Url| async move {
                    let query: Vec<_> = url.query_pairs().into_owned().collect();
                    if param(&query, "start-after").is_some() {
                        std::future::pending::<()>().await;
                    }
                    Ok(listing(&["20261120T0005"], None))
                });
                timeout(HOST_LIST_TIMEOUT * 8, listed)
                    .await
                    .expect("a stalled listing must time out")
            })
            .unwrap();
        assert_eq!(hosts.len(), 2);
        assert!(hosts.iter().all(|objects| objects.len() == 1));
    }

    fn param(query: &[(String, String)], name: &str) -> Option<String> {
        query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn listing_url_carries_an_opaque_token_intact() {
        let token = "2026/11/20/OPERA/COMP/OPERA@20261120T1900@0@RATE.tiff+a=b&c d%";
        let day = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let url = listing_url(HOSTS[1], day, None, Some(token)).unwrap();
        assert!(url.as_str().starts_with(HOSTS[1]));
        let sent: Vec<_> = url.query_pairs().into_owned().collect();
        assert!(sent.contains(&("continuation-token".to_owned(), token.to_owned())));
    }

    #[test]
    fn listing_follows_ord_continuation_pages() {
        let first = include_str!("../tests/fixtures/opera-listing-page1.xml");
        let rest = include_str!("../tests/fixtures/opera-listing-page2.xml");
        let now = parse_dbzh_key("OPERA@20261120T2030@0@DBZH.tiff").unwrap();
        let objects = current_thread()
            .block_on(list_window(HOSTS[1], now, &|url: reqwest::Url| {
                let query: Vec<_> = url.query_pairs().into_owned().collect();
                let sent = (
                    param(&query, "prefix"),
                    param(&query, "start-after"),
                    param(&query, "continuation-token"),
                );
                std::future::ready(match sent {
                    (Some(prefix), Some(after), token)
                        if prefix == "2026/11/20/OPERA/COMP/"
                            && after == "2026/11/20/OPERA/COMP/OPERA@20261120T1900" =>
                    {
                        match token.as_deref() {
                            None => Ok(first.to_owned()),
                            Some("2026/11/20/OPERA/COMP/OPERA@20261120T1900@0@RATE.tiff") => {
                                Ok(rest.to_owned())
                            }
                            Some(other) => Err(format!("unexpected token {other}")),
                        }
                    }
                    other => Err(format!("unexpected request {other:?}")),
                })
            }))
            .unwrap();
        assert_eq!(objects.len(), 12);
        assert!(objects.iter().all(|o| o.key.ends_with("@0@DBZH.tiff")));
        assert_eq!(objects[0], comp_on("2026/11/20", "20261120T1900"));
        assert_eq!(objects[11], comp_on("2026/11/20", "20261120T1955"));
    }

    fn paged(last_page_truncated: bool) -> (Result<Vec<CompObject>, String>, usize) {
        let requests = std::sync::atomic::AtomicUsize::new(0);
        let result = current_thread().block_on(list_pages(HOSTS[0], |_token| {
            let n = requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let next = (n < 8 || last_page_truncated).then(|| format!("t{n}"));
            std::future::ready(if n > 20 {
                Err("runaway listing".to_owned())
            } else {
                Ok(listing(&["20261120T2005"], next.as_deref()))
            })
        }));
        (result, requests.into_inner())
    }

    #[test]
    fn eighth_page_ends_a_listing_and_a_ninth_is_never_requested() {
        let (complete, requests) = paged(false);
        assert_eq!(complete.unwrap().len(), 8);
        assert_eq!(requests, 8);
        let (endless, requests) = paged(true);
        assert!(endless.unwrap_err().contains("exceeds"));
        assert_eq!(requests, 8);
    }

    #[test]
    fn repeated_token_stops_the_listing() {
        let requests = std::sync::atomic::AtomicUsize::new(0);
        let result = current_thread().block_on(list_pages(HOSTS[0], |_token| {
            let n = requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            std::future::ready(if n > 20 {
                Err("runaway listing".to_owned())
            } else {
                Ok(listing(&[], Some("same")))
            })
        }));
        assert!(result.unwrap_err().contains("repeated"));
        assert_eq!(requests.into_inner(), 2);
    }

    #[test]
    fn truncated_listing_without_token_is_an_error() {
        let body = "<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>";
        assert!(parse_dbzh_listing(HOSTS[0], body).is_err());
    }

    #[test]
    fn redirects_stay_on_the_store_origin() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        current_thread().block_on(async {
            let home = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let away = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", home.local_addr().unwrap());
            let elsewhere = format!("http://{}/x", away.local_addr().unwrap());
            let server = tokio::spawn(async move {
                for location in ["/moved".to_owned(), elsewhere] {
                    let (mut socket, _) = home.accept().await.unwrap();
                    let _ = socket.read(&mut [0u8; 2048]).await.unwrap();
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                }
            });
            let error = get_http(format!("{base}/start")).await.unwrap_err();
            assert!(error.contains("error following redirect"), "{error}");
            timeout(Duration::from_secs(5), server)
                .await
                .expect("the same-origin hop was not followed")
                .unwrap();
            assert!(timeout(Duration::from_millis(100), away.accept()).await.is_err());
        });
    }

    #[test]
    fn stalled_download_times_out() {
        current_thread().block_on(async {
            let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/x", server.local_addr().unwrap());
            let _held = tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut socket, _) = server.accept().await.unwrap();
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npartial")
                    .await
                    .unwrap();
                let _ = socket.read(&mut [0u8; 2048]).await.unwrap();
                std::future::pending::<()>().await;
                drop(socket);
            });
            let client = build_client(Duration::from_secs(2));
            let result = timeout(Duration::from_secs(10), get_body(&client, &url, BODY_MAX)).await;
            let error = result
                .expect("request timeout must fire first")
                .unwrap_err();
            assert!(error.starts_with("reading OPERA"), "{error}");
        });
    }

    fn comp_on(day: &str, stamp: &str) -> CompObject {
        let key = format!("{day}/OPERA/COMP/OPERA@{stamp}@0@DBZH.tiff");
        CompObject {
            host: HOSTS[1],
            stamp: parse_dbzh_key(&key).unwrap(),
            key,
        }
    }

    fn comp(stamp: &str) -> CompObject {
        comp_on("2026/09/17", stamp)
    }

    #[test]
    fn decode_blocking_matches_decode_frame_on_current_thread() {
        let bytes = synthetic_fixture_cog();
        let stamp = NaiveDate::from_ymd_opt(2026, 9, 17)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let opera = Opera::new();
        let direct = decode_frame(&bytes, stamp, &opera.palette, &opera.bounds).unwrap();
        let blocked = current_thread()
            .block_on(decode_blocking(
                bytes,
                stamp,
                Arc::new(opera.palette.clone()),
                Arc::new(opera.bounds.clone()),
            ))
            .unwrap();
        assert_eq!(direct.0.id, blocked.0.id);
        assert_eq!(direct.1, blocked.1);
        assert_eq!(direct.2, blocked.2);
    }

    #[test]
    fn history_fill_delivers_oldest_first_with_bounded_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::mpsc;

        let fixture = synthetic_fixture_cog();
        let opera = Opera::new();
        let palette = Arc::new(opera.palette.clone());
        let bounds = Arc::new(opera.bounds.clone());
        let targets = vec![
            comp("20260917T1110"),
            comp("20260917T1115"),
            comp("20260917T1120"),
        ];
        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::channel(8);
        let tx = GridSender::new(tx, 1);
        let got = current_thread().block_on(async {
            let get_inflight = Arc::clone(&inflight);
            let get_peak = Arc::clone(&peak);
            let get_fixture = fixture.clone();
            let fill = fill_history(
                tx,
                targets,
                move |key: String| {
                    let inflight = Arc::clone(&get_inflight);
                    let peak = Arc::clone(&get_peak);
                    let fixture = get_fixture.clone();
                    async move {
                        let n = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(n, Ordering::SeqCst);
                        let delay = if key.contains("1110") {
                            Duration::from_millis(40)
                        } else {
                            Duration::from_millis(5)
                        };
                        sleep(delay).await;
                        inflight.fetch_sub(1, Ordering::SeqCst);
                        Ok(fixture)
                    }
                },
                palette,
                bounds,
                Arc::new(Semaphore::new(BACKFILL_IN_FLIGHT)),
            );
            fill.await;
            let mut stamps = Vec::new();
            while let Ok(event) = rx.try_recv() {
                let Event::Backfill { frame, .. } = event.event else {
                    panic!("expected backfill");
                };
                stamps.push(frame.scan_time.clone());
            }
            stamps
        });
        assert_eq!(
            got,
            vec![
                "2026-09-17T11:10:00Z".to_string(),
                "2026-09-17T11:15:00Z".to_string(),
                "2026-09-17T11:20:00Z".to_string(),
            ]
        );
        assert!(
            peak.load(Ordering::SeqCst) <= BACKFILL_IN_FLIGHT,
            "peak fetch concurrency {}",
            peak.load(Ordering::SeqCst)
        );
        assert_eq!(peak.load(Ordering::SeqCst), BACKFILL_IN_FLIGHT);
    }

    #[test]
    fn history_fill_stops_when_receiver_closes() {
        use tokio::sync::mpsc;

        let fixture = synthetic_fixture_cog();
        let opera = Opera::new();
        let targets = vec![
            comp("20260917T1110"),
            comp("20260917T1115"),
            comp("20260917T1120"),
        ];
        current_thread().block_on(async {
            let (tx, rx) = mpsc::channel(1);
            let tx = GridSender::new(tx, 1);
            let fill = tokio::spawn(fill_history(
                tx,
                targets,
                move |_key: String| {
                    let fixture = fixture.clone();
                    async move {
                        sleep(Duration::from_millis(50)).await;
                        Ok(fixture)
                    }
                },
                Arc::new(opera.palette.clone()),
                Arc::new(opera.bounds.clone()),
                Arc::new(Semaphore::new(BACKFILL_IN_FLIGHT)),
            ));
            sleep(Duration::from_millis(10)).await;
            drop(rx);
            tokio::time::timeout(Duration::from_secs(2), fill)
                .await
                .expect("fill_history should stop after the receiver closes")
                .unwrap();
        });
    }

    #[test]
    fn history_fill_parent_abort_cancels_inflight_fetches() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::mpsc;

        struct FetchDrop(Arc<AtomicUsize>);
        impl Drop for FetchDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let fixture = synthetic_fixture_cog();
        let opera = Opera::new();
        let started = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let targets = vec![
            comp("20260917T1110"),
            comp("20260917T1115"),
            comp("20260917T1120"),
        ];
        current_thread().block_on(async {
            let (tx, mut rx) = mpsc::channel(8);
            let tx = GridSender::new(tx, 1);
            let get_started = Arc::clone(&started);
            let get_dropped = Arc::clone(&dropped);
            let get_finished = Arc::clone(&finished);
            let fill = tokio::spawn(fill_history(
                tx,
                targets,
                move |_key: String| {
                    let started = Arc::clone(&get_started);
                    let dropped = Arc::clone(&get_dropped);
                    let finished = Arc::clone(&get_finished);
                    let fixture = fixture.clone();
                    async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        let _guard = FetchDrop(dropped);
                        sleep(Duration::from_secs(30)).await;
                        finished.fetch_add(1, Ordering::SeqCst);
                        Ok(fixture)
                    }
                },
                Arc::new(opera.palette.clone()),
                Arc::new(opera.bounds.clone()),
                Arc::new(Semaphore::new(BACKFILL_IN_FLIGHT)),
            ));
            timeout(Duration::from_secs(1), async {
                while started.load(Ordering::SeqCst) < 1 {
                    sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("a fetch should start before abort");
            fill.abort();
            let _ = fill.await;
            let expect = started.load(Ordering::SeqCst);
            timeout(Duration::from_secs(1), async {
                while dropped.load(Ordering::SeqCst) < expect {
                    sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("parent abort should drop in-flight fetch tasks");
            assert_eq!(finished.load(Ordering::SeqCst), 0);
            assert!(
                expect <= BACKFILL_IN_FLIGHT,
                "queued fetches must not start after abort, started={expect}"
            );
            assert!(
                rx.try_recv().is_err(),
                "aborted fill must not deliver events"
            );
        });
    }
}
