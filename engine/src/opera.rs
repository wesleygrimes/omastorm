//! EUMETNET OPERA COMP DBZH GridFamily adapter: anonymous ORD 24h COG
//! (`docs/grid-adapters.md`). Live fetch only; no vendored GeoTIFF.

use crate::{
    cog::{self, DecodedRaster},
    live_index,
    protocol::{
        AdapterTarget, Coverage, Crs, Ellipsoid, Family, FrameStatus, Kind, MosaicFrame,
        ProductClass,
    },
    source::{GridEvent, MosaicMeta, SourceMetadataBorrowed},
    sweep,
};
use chrono::{NaiveDate, NaiveDateTime, Utc};
use std::{
    collections::{HashSet, VecDeque},
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Semaphore, mpsc::Sender},
    task::{JoinHandle, spawn_blocking},
    time::{sleep, timeout},
};
use xml::reader::{EventReader, XmlEvent};

pub const ID: &str = "opera";
pub const NAME: &str = "EUMETNET OPERA";
pub const ATTRIBUTION: &str = "EUMETNET OPERA";
/// Continental fallback: a covering national mosaic ranks above this.
pub const SELECTION_PRIORITY: i32 = 10;

pub const HOST: &str = "https://s3.waw3-1.cloudferro.com/openradar-24h";
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
const START_TIMEOUT: Duration = Duration::from_secs(45);
const IDLE: Duration = Duration::from_secs(30);
/// After the newest COMP is on screen, pull earlier stamps (NEXRAD waits
/// the same beat so a hand-off mid-pan does not spend bandwidth).
const BACKFILL_DELAY: Duration = Duration::from_secs(3);
/// Concurrent fetch+decode during history fill. Two 3800×4400 frames is
/// already hundreds of MB; more would pile decoded rasters on the
/// current-thread runtime's blocking pool.
const BACKFILL_IN_FLIGHT: usize = 2;
const FILL: f32 = -9_999_000.0;

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
        }
    }

    pub fn loading_texture() -> Result<Vec<u8>, String> {
        sweep::png(1, 1, &[0, 0, 0, 0]).map_err(|e| format!("encoding OPERA placeholder: {e}"))
    }

    pub fn poll(
        &self,
        target: &AdapterTarget,
        events: Sender<Event>,
        known_keys: HashSet<String>,
    ) -> Option<JoinHandle<()>> {
        match target {
            AdapterTarget::Mosaic => {
                let palette = self.palette.clone();
                let bounds = self.bounds.clone();
                Some(tokio::spawn(async move {
                    poll_loop(events, known_keys, palette, bounds).await;
                }))
            }
            AdapterTarget::Site { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompObject {
    pub key: String,
    pub stamp: NaiveDateTime,
}

/// Parse ListObjects v2 XML for `OPERA@…@DBZH.tiff` keys.
pub fn parse_dbzh_listing(body: &str) -> Result<Vec<CompObject>, String> {
    let mut objects = Vec::new();
    let mut in_key = false;
    let mut key = None::<String>;
    for event in EventReader::new(body.as_bytes()) {
        match event.map_err(|e| format!("reading OPERA listing: {e}"))? {
            XmlEvent::StartElement { name, .. } if name.local_name == "Key" => {
                in_key = true;
                key = Some(String::new());
            }
            XmlEvent::Characters(text) if in_key => {
                if let Some(k) = key.as_mut() {
                    k.push_str(&text);
                }
            }
            XmlEvent::EndElement { name } if name.local_name == "Key" => {
                in_key = false;
                if let Some(k) = key.take()
                    && let Some(stamp) = parse_dbzh_key(&k)
                {
                    objects.push(CompObject { key: k, stamp });
                }
            }
            _ => {}
        }
    }
    objects.sort_by_key(|o| o.stamp);
    Ok(objects)
}

/// `…/OPERA/COMP/OPERA@YYYYMMDDTHHMM@0@DBZH.tiff`
pub fn parse_dbzh_key(key: &str) -> Option<NaiveDateTime> {
    let name = key.rsplit('/').next()?;
    let rest = name.strip_prefix("OPERA@")?;
    let stamp = rest.strip_suffix("@0@DBZH.tiff")?;
    NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M").ok()
}

pub fn prefix_for(day: NaiveDate) -> String {
    format!("{}/OPERA/COMP/", day.format("%Y/%m/%d"))
}

/// Classify OPERA DBZH floats into the grid texture encoding.
pub fn classify(values: &[f32], nodata: Option<f32>, bounds: &[f64], classes: usize) -> Vec<u8> {
    let classes = classes.max(1);
    let fill = nodata.unwrap_or(FILL);
    let mut pixels = Vec::with_capacity(values.len() * 4);
    for &v in values {
        let (r, g) = if v.is_nan() {
            (0, 2) // undetect
        } else if (v - fill).abs() < 1.0 || v <= fill / 2.0 {
            // Missing / nodata (G bit 0). Grid shader draws transparent —
            // nodata draws nothing; oceans must not use the polar folded hatch.
            (0, 1)
        } else {
            let class = class_of(v as f64, bounds, classes);
            (class + 1, 0)
        };
        pixels.extend_from_slice(&[r, g, 0, 255]);
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
    let bytes = (*get)(obj.key.clone()).await?;
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

/// List + GET the newest unseen COMP. Decode is the caller's job so the
/// start timeout cannot turn a slow PNG encode into `Offline`.
async fn fetch_newest<L, G, LF, GF>(
    known: &HashSet<String>,
    list_day: L,
    get: G,
) -> Result<Option<Fetched>, String>
where
    L: Fn(NaiveDate) -> LF,
    LF: Future<Output = Result<Vec<CompObject>, String>>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
{
    let objects = list_recent(list_day).await?;
    let Some(newest) = objects.last().cloned() else {
        return Ok(None);
    };
    if known.contains(&newest.key) {
        return Ok(None);
    }
    Ok(Some(fetch_one(newest, Arc::new(get)).await?))
}

async fn load_newest<L, G, LF, GF>(
    known: &HashSet<String>,
    list_day: L,
    get: G,
    palette: Arc<Vec<String>>,
    bounds: Arc<Vec<f64>>,
    decode_slots: Option<Arc<Semaphore>>,
) -> Result<Option<Loaded>, String>
where
    L: Fn(NaiveDate) -> LF,
    LF: Future<Output = Result<Vec<CompObject>, String>>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
{
    let Some(fetched) = fetch_newest(known, list_day, get).await? else {
        return Ok(None);
    };
    Ok(Some(
        finish_load(fetched, palette, bounds, decode_slots.as_deref()).await?,
    ))
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

/// List today's COMP objects, falling back to yesterday when today is empty.
pub async fn list_recent<L, LF>(list_day: L) -> Result<Vec<CompObject>, String>
where
    L: Fn(NaiveDate) -> LF,
    LF: Future<Output = Result<Vec<CompObject>, String>>,
{
    let today = Utc::now().date_naive();
    let mut objects = list_day(today).await?;
    if objects.is_empty() {
        objects = list_day(today.pred_opt().unwrap_or(today)).await?;
    }
    Ok(objects)
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
    list_day: L,
    get: G,
    palette: &[String],
    bounds: &[f64],
) -> Result<Option<(CompObject, MosaicFrame, Vec<u8>, i64)>, String>
where
    L: Fn(NaiveDate) -> LF,
    LF: Future<Output = Result<Vec<CompObject>, String>>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
{
    Ok(load_newest(
        known,
        list_day,
        get,
        Arc::new(palette.to_vec()),
        Arc::new(bounds.to_vec()),
        None,
    )
    .await?
    .map(|loaded| (loaded.object, loaded.frame, loaded.texture, loaded.start_ms)))
}

/// Fetch up to `HISTORY_MAX` recent COMP frames the live poll has not
/// already delivered. Failures on one object skip it; a listing failure
/// ends the backfill.
async fn backfill_loop(
    events: Sender<Event>,
    known: HashSet<String>,
    palette: Arc<Vec<String>>,
    bounds: Arc<Vec<f64>>,
    decode_slots: Arc<Semaphore>,
) {
    sleep(BACKFILL_DELAY).await;
    let objects = match list_recent(list_http).await {
        Ok(objects) => objects,
        Err(reason) => {
            opera_log(format_args!("backfill listing: {reason}"));
            return;
        }
    };
    let targets = backfill_targets(&objects, &known, HISTORY_MAX);
    fill_history(events, targets, get_http, palette, bounds, decode_slots).await;
}

/// Fetch+decode up to `BACKFILL_IN_FLIGHT` objects at a time, but emit
/// `Event::Backfill` oldest-first. Aborting this task (or a closed
/// receiver) aborts every child, including the one currently joined.
async fn fill_history<G, GF>(
    events: Sender<Event>,
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

async fn list_http(day: NaiveDate) -> Result<Vec<CompObject>, String> {
    let prefix = prefix_for(day);
    let url = format!("{HOST}?list-type=2&prefix={prefix}&max-keys=1000");
    let response = live_index::http_client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("listing OPERA: {e}"))?
        .error_for_status()
        .map_err(|e| format!("listing OPERA: {e}"))?;
    let bytes = live_index::take_body(response, live_index::LISTING_MAX)
        .await
        .map_err(|e| format!("reading OPERA listing: {e}"))?;
    let body = String::from_utf8(bytes).map_err(|e| format!("reading OPERA listing: {e}"))?;
    parse_dbzh_listing(&body)
}

async fn get_http(key: String) -> Result<Vec<u8>, String> {
    let url = format!("{HOST}/{key}");
    let response = live_index::http_client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fetching OPERA {key}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("fetching OPERA {key}: {e}"))?;
    live_index::take_body(response, BODY_MAX)
        .await
        .map_err(|e| format!("reading OPERA {key}: {e}"))
}

async fn poll_loop(
    events: Sender<Event>,
    mut known: HashSet<String>,
    palette: Vec<String>,
    bounds: Vec<f64>,
) {
    let palette = Arc::new(palette);
    let bounds = Arc::new(bounds);
    let decode_slots = Arc::new(Semaphore::new(BACKFILL_IN_FLIGHT));
    let first = timeout(START_TIMEOUT, fetch_newest(&known, list_http, get_http)).await;
    match first {
        Ok(Ok(Some(fetched))) => {
            match finish_load(fetched, Arc::clone(&palette), Arc::clone(&bounds), None).await {
                Ok(loaded) => {
                    log_load("", &loaded);
                    known.insert(loaded.object.key);
                    let _ = events
                        .send(ev_frame(loaded.frame, loaded.texture, loaded.start_ms))
                        .await;
                }
                Err(reason) => {
                    let _ = events.send(ev_offline(reason)).await;
                }
            }
        }
        Ok(Ok(None)) => {
            let _ = events
                .send(ev_silent("ORD cache has no OPERA DBZH.tiff yet".into()))
                .await;
        }
        Ok(Err(reason)) => {
            let _ = events.send(ev_offline(reason)).await;
        }
        Err(_) => {
            let _ = events
                .send(ev_offline("OPERA fetch timed out".into()))
                .await;
        }
    }
    // Same shape as NEXRAD: newest is already on screen; pull the rest of
    // the dozen in the background so play/[ ] have a loop. Held so aborting
    // this poller cancels in-flight COMP downloads.
    let _backfill = AbortOnDrop(tokio::spawn({
        let events = events.clone();
        let known = known.clone();
        let palette = Arc::clone(&palette);
        let bounds = Arc::clone(&bounds);
        let decode_slots = Arc::clone(&decode_slots);
        async move {
            backfill_loop(events, known, palette, bounds, decode_slots).await;
        }
    }));
    loop {
        sleep(IDLE).await;
        match load_newest(
            &known,
            list_http,
            get_http,
            Arc::clone(&palette),
            Arc::clone(&bounds),
            Some(Arc::clone(&decode_slots)),
        )
        .await
        {
            Ok(Some(loaded)) => {
                log_load("", &loaded);
                known.insert(loaded.object.key.clone());
                if known.len() > HISTORY_MAX * 4 {
                    known = HashSet::from([loaded.object.key]);
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
    use crate::protocol::{AdapterTarget, GeoPoint, Selection};
    use crate::source::{Candidate, SourceRegistry, covering_selection};

    #[test]
    fn loading_frame_is_blank_mosaic_chrome() {
        let opera = Opera::new();
        let frame = opera.loading_frame();
        assert_eq!(frame.id, "opera-loading");
        assert!(frame.scan_time.is_empty());
        assert_eq!(frame.status, FrameStatus::Partial);
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
        let objects = parse_dbzh_listing(body).unwrap();
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
        assert_eq!(start_ms, stamp.and_utc().timestamp_millis());
        let info = png::Decoder::new(std::io::Cursor::new(&png))
            .read_info()
            .unwrap()
            .info()
            .clone();
        assert_eq!((info.width, info.height), (32, 32));
        let pixels = classify(
            &cog::decode_float_cog(&bytes).unwrap().values,
            Some(FILL),
            &opera.bounds,
            opera.palette.len(),
        );
        assert_eq!(pixels[1], 0); // measured G
        assert!(pixels[0] >= 1);
        assert_eq!(pixels[4], 0);
        assert_eq!(pixels[5], 1); // nodata/missing
        assert_eq!(pixels[12], 0);
        assert_eq!(pixels[13], 2); // undetect nan
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
                |_day| async { parse_dbzh_listing(listing) },
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
                |_day| async { parse_dbzh_listing(listing) },
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
        let objects = parse_dbzh_listing(listing).unwrap();
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

    fn comp(stamp: &str) -> CompObject {
        let key = format!("2026/09/17/OPERA/COMP/OPERA@{stamp}@0@DBZH.tiff");
        CompObject {
            stamp: parse_dbzh_key(&key).unwrap(),
            key,
        }
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
                let Event::Backfill { frame, .. } = event else {
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

    #[test]
    fn start_timeout_covers_fetch_not_decode() {
        let fixture = synthetic_fixture_cog();
        let opera = Opera::new();
        let listing = r#"<ListBucketResult>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1200@0@DBZH.tiff</Key></Contents>
            <Contents><Key>2026/09/17/OPERA/COMP/OPERA@20260917T1205@0@DBZH.tiff</Key></Contents>
            </ListBucketResult>"#;
        let known = HashSet::new();
        current_thread().block_on(async {
            let slow = fixture.clone();
            let missed = timeout(
                Duration::from_millis(20),
                fetch_newest(
                    &known,
                    |_day| async { parse_dbzh_listing(listing) },
                    move |_key| {
                        let slow = slow.clone();
                        async move {
                            sleep(Duration::from_millis(80)).await;
                            Ok(slow)
                        }
                    },
                ),
            )
            .await;
            assert!(missed.is_err(), "slow GET must trip the fetch timeout");

            let fetched = timeout(
                Duration::from_millis(20),
                fetch_newest(&known, |_day| async { parse_dbzh_listing(listing) }, {
                    let fixture = fixture.clone();
                    move |_key| {
                        let fixture = fixture.clone();
                        async move { Ok(fixture) }
                    }
                }),
            )
            .await
            .expect("fast GET should beat the fetch timeout")
            .unwrap()
            .unwrap();
            assert!(fetched.object.key.ends_with("1205@0@DBZH.tiff"));

            // A stall longer than the fetch budget must not prevent decode:
            // poll_loop times out only listing+GET, then shows Frame.
            sleep(Duration::from_millis(40)).await;
            let loaded = finish_load(
                fetched,
                Arc::new(opera.palette.clone()),
                Arc::new(opera.bounds.clone()),
                None,
            )
            .await
            .unwrap();
            assert!(loaded.object.key.ends_with("1205@0@DBZH.tiff"));
        });
    }
}
