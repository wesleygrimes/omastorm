//! Airport METARs for the selected radar (DESIGN.md, observations).
//!
//! `metar_query` is a reply to its sender, like `search_places`. The engine
//! fetches NOAA/NWS Aviation Weather Center JSON, keeps the raw observation
//! string, and names FAA flight category only so the UI can color ICAO chips.
//! It does not decode English. Nothing is fetched until a client asks.
//! Coverage is US, Canada, Hawaii, Guam, and Puerto Rico / USVI — not the
//! coarse NEXRAD clip (that includes RKJK and LPLA). A radar outside that
//! area, including OPERA Europe, is a no-op: empty `metars`, no fetch.
//! Returned stations are ICAO `K`, `C`, `P`, `TI`, `TJ`, and `M`. Default
//! pick is the nearest stations inside 250 km of the radar. `pick=priority` ranks AWC stationinfo
//! `priority` (lower is a hub) inside the view bbox. A repeat of the same
//! radar within ten minutes is answered from the feed cache, including a
//! slightly moved view box and while backing off after a fetch failure.
//! `OMASTORM_METAR_URL` / `OMASTORM_METAR_FIXTURE` and
//! `OMASTORM_STATIONS_URL` / `OMASTORM_STATIONS_FIXTURE` override the live
//! feeds for checks (no network).

use crate::protocol::MetarReport;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    env, fs, io,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, oneshot};

const DEFAULT_URL: &str = "https://aviationweather.gov/api/data/metar";
const DEFAULT_STATIONS_URL: &str = "https://aviationweather.gov/api/data/stationinfo";
const USER_AGENT: &str = concat!(
    "omastorm/",
    env!("CARGO_PKG_VERSION"),
    " (https://omastorm.com)"
);
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY: usize = 1 << 20;
/// How far around the radar a station may sit when no view bbox is sent.
const RADIUS_KM: f64 = 250.0;
pub const LIMIT: usize = 16;
const MISSING_PRIORITY: u8 = 99;
const TTL: Duration = Duration::from_secs(600);
const STATIONS_TTL: Duration = Duration::from_secs(24 * 3600);
const EARTH_KM: f64 = 6371.0;
/// Same cap OSM uses for tile fetches.
const IN_FLIGHT: usize = 4;
/// After a 429, a 5xx, or a transport failure. AWC allows 100 requests a minute.
const BACK_OFF: Duration = Duration::from_secs(30);
const VIEW_PAD: f64 = 0.25;
const MAX_PAD_DEG: f64 = 1.0;
/// Expired feed and station entries are dropped, then the maps are capped.
const CACHE_CAP: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Nearest,
    Priority,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BBox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub lat: f64,
    pub lon: f64,
    pub bbox: Option<BBox>,
    pub pick: Pick,
    pub limit: usize,
    pub always_on: Vec<String>,
}

pub struct Service {
    client: reqwest::Client,
    url: String,
    stations_url: String,
    fixture: Option<PathBuf>,
    stations_fixture: Option<PathBuf>,
    permits: Semaphore,
    feeds: Mutex<HashMap<BoxKey, CachedFeed>>,
    stations: Mutex<HashMap<BoxKey, CachedStations>>,
    metar_flights: Flights<Vec<MetarReport>>,
    station_flights: Flights<HashMap<String, u8>>,
    back_off_until: Mutex<Option<Instant>>,
}

type Waiters<T> = Vec<oneshot::Sender<Result<T, String>>>;
type Flights<T> = Mutex<HashMap<BoxKey, Waiters<T>>>;

/// Rounded fetch box plus the UTC hour (METARs) or UTC day (station info).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BoxKey {
    south: i32,
    west: i32,
    north: i32,
    east: i32,
    bucket: i64,
}

struct CachedFeed {
    at: Instant,
    radar: (i32, i32),
    box_: BBox,
    reports: Vec<MetarReport>,
}

struct CachedStations {
    at: Instant,
    radar: (i32, i32),
    box_: BBox,
    by_id: HashMap<String, u8>,
}

/// Tells every waiter for one in-flight fetch, including when the leader is dropped.
struct Done<'a, T: Clone> {
    flights: &'a Flights<T>,
    key: BoxKey,
    result: Option<Result<T, String>>,
}

impl<T: Clone> Drop for Done<'_, T> {
    fn drop(&mut self) {
        let waiters = self
            .flights
            .lock()
            .unwrap()
            .remove(&self.key)
            .unwrap_or_default();
        let result = self
            .result
            .take()
            .unwrap_or_else(|| Err("METAR fetch failed".into()));
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
    }
}

#[derive(Deserialize)]
struct AwcMetar {
    #[serde(rename = "icaoId", default)]
    icao_id: Option<String>,
    #[serde(rename = "rawOb", default)]
    raw_ob: String,
    #[serde(rename = "fltCat", default)]
    flt_cat: String,
    lat: f64,
    lon: f64,
    #[serde(rename = "obsTime", default)]
    obs_time: Option<f64>,
    #[serde(default)]
    visib: serde_json::Value,
    /// `None` when AWC omitted sky cover. An empty array means no ceiling.
    #[serde(default)]
    clouds: Option<Vec<AwcCloud>>,
    #[serde(rename = "vertVis", default)]
    vert_vis: Option<f64>,
}

#[derive(Deserialize, Default)]
struct AwcCloud {
    #[serde(default)]
    cover: String,
    #[serde(default)]
    base: Option<f64>,
}

#[derive(Deserialize)]
struct AwcStation {
    #[serde(rename = "icaoId", default)]
    icao_id: Option<String>,
    #[serde(default)]
    priority: serde_json::Value,
}

impl Query {
    #[allow(clippy::too_many_arguments)]
    pub fn parse(
        lat: f64,
        lon: f64,
        south: Option<f64>,
        west: Option<f64>,
        north: Option<f64>,
        east: Option<f64>,
        pick: Option<String>,
        limit: Option<u32>,
        always_on: Vec<String>,
    ) -> Result<Query, String> {
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return Err("metar_query needs lat in [-90, 90] and lon in [-180, 180].".into());
        }
        let pick = match pick.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("nearest") => Pick::Nearest,
            Some("priority") => Pick::Priority,
            Some(_) => return Err("metar_query pick must be nearest or priority.".into()),
        };
        let limit = match limit {
            None => LIMIT,
            Some(n) if (1..=LIMIT as u32).contains(&n) => n as usize,
            Some(_) => return Err("metar_query limit must be 1 through 16.".into()),
        };
        let bbox = match (south, west, north, east) {
            (None, None, None, None) => None,
            (Some(south), Some(west), Some(north), Some(east)) => {
                if !(-90.0..=90.0).contains(&south)
                    || !(-90.0..=90.0).contains(&north)
                    || !(-180.0..=180.0).contains(&west)
                    || !(-180.0..=180.0).contains(&east)
                    || south > north
                    || west > east
                {
                    return Err("metar_query needs south, west, north, east as a view box.".into());
                }
                Some(BBox {
                    south,
                    west,
                    north,
                    east,
                })
            }
            _ => {
                return Err("metar_query needs south, west, north, east together.".into());
            }
        };
        if pick == Pick::Priority && bbox.is_none() {
            return Err("metar_query pick=priority needs the view box.".into());
        }
        let mut seen = HashSet::new();
        let mut pinned = Vec::new();
        for raw in always_on {
            let id = raw.trim().to_uppercase();
            if id.is_empty() {
                continue;
            }
            if !id.chars().all(|c| c.is_ascii_alphanumeric()) || !(3..=4).contains(&id.len()) {
                return Err("metar_query always_on ids must be ICAO station ids.".into());
            }
            if pinned.len() >= limit {
                break;
            }
            if seen.insert(id.clone()) {
                pinned.push(id);
            }
        }
        Ok(Query {
            lat,
            lon,
            bbox,
            pick,
            limit,
            always_on: pinned,
        })
    }
}

impl Service {
    pub fn open() -> io::Result<Service> {
        Service::assemble(
            env::var("OMASTORM_METAR_URL").unwrap_or_else(|_| DEFAULT_URL.into()),
            env::var("OMASTORM_STATIONS_URL").unwrap_or_else(|_| DEFAULT_STATIONS_URL.into()),
            env::var_os("OMASTORM_METAR_FIXTURE").map(PathBuf::from),
            env::var_os("OMASTORM_STATIONS_FIXTURE").map(PathBuf::from),
        )
    }

    fn assemble(
        url: String,
        stations_url: String,
        fixture: Option<PathBuf>,
        stations_fixture: Option<PathBuf>,
    ) -> io::Result<Service> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(io::Error::other)?;
        Ok(Service {
            client,
            url,
            stations_url,
            fixture,
            stations_fixture,
            permits: Semaphore::new(IN_FLIGHT),
            feeds: Mutex::new(HashMap::new()),
            stations: Mutex::new(HashMap::new()),
            metar_flights: Mutex::new(HashMap::new()),
            station_flights: Mutex::new(HashMap::new()),
            back_off_until: Mutex::new(None),
        })
    }

    #[cfg(test)]
    fn for_tests(url: &str, fixture: Option<PathBuf>, stations: Option<PathBuf>) -> Service {
        Service::assemble(url.into(), url.into(), fixture, stations).unwrap()
    }

    pub async fn query(&self, q: Query) -> Result<Vec<MetarReport>, String> {
        if !covered(q.lon, q.lat) {
            return Ok(Vec::new());
        }
        let reports = self.metar_feed(&q).await?;
        let priorities = if q.pick == Pick::Priority {
            match self.station_priorities(&q).await {
                Ok(priorities) => priorities,
                // A cached observation still paints chips; missing ranks
                // fall back to distance.
                Err(_) => self.cached_stations_for(&q, true).unwrap_or_default(),
            }
        } else {
            HashMap::new()
        };
        Ok(select(reports, &q, &priorities))
    }

    /// Parsed observations for this selection. A cached feed covering this box
    /// within ten minutes does not start another request.
    async fn metar_feed(&self, q: &Query) -> Result<Vec<MetarReport>, String> {
        if let Some(hit) = self.cached_feed_for(q, false) {
            return Ok(hit);
        }
        let box_ = fetch_box(q);
        let key = BoxKey::hour(box_);
        if self.fixture.is_none() && self.backing_off() {
            return self
                .cached_feed_for(q, true)
                .ok_or_else(|| "backing off after a METAR fetch failure".into());
        }
        let follower = {
            let mut flights = self.metar_flights.lock().unwrap();
            if let Some(hit) = self.cached_feed_for(q, false) {
                return Ok(hit);
            }
            match flights.entry(key) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let (tx, rx) = oneshot::channel();
                    entry.get_mut().push(tx);
                    Some(rx)
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Vec::new());
                    None
                }
            }
        };
        if let Some(rx) = follower {
            return rx
                .await
                .unwrap_or_else(|_| Err("METAR fetch failed".into()));
        }
        let mut done = Done {
            flights: &self.metar_flights,
            key,
            result: None,
        };
        let result = self.fetch_metars(box_).await;
        done.result = Some(result.clone());
        if let Ok(ref reports) = result {
            self.store_feed(key, q, reports.clone());
        }
        drop(done);
        result
    }

    async fn station_priorities(&self, q: &Query) -> Result<HashMap<String, u8>, String> {
        if let Some(hit) = self.cached_stations_for(q, false) {
            return Ok(hit);
        }
        let box_ = fetch_box(q);
        let key = BoxKey::day(box_);
        if self.stations_fixture.is_none() && self.backing_off() {
            return Err("backing off after a METAR fetch failure".into());
        }
        let follower = {
            let mut flights = self.station_flights.lock().unwrap();
            if let Some(hit) = self.cached_stations_for(q, false) {
                return Ok(hit);
            }
            match flights.entry(key) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let (tx, rx) = oneshot::channel();
                    entry.get_mut().push(tx);
                    Some(rx)
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Vec::new());
                    None
                }
            }
        };
        if let Some(rx) = follower {
            return rx
                .await
                .unwrap_or_else(|_| Err("METAR fetch failed".into()));
        }
        let mut done = Done {
            flights: &self.station_flights,
            key,
            result: None,
        };
        let result = self.fetch_stations(box_).await;
        done.result = Some(result.clone());
        if let Ok(ref by_id) = result {
            self.store_stations(key, q, by_id.clone());
        }
        drop(done);
        result
    }

    fn cached_feed_for(&self, q: &Query, same_radar: bool) -> Option<Vec<MetarReport>> {
        let radar = (tenths(q.lat), tenths(q.lon));
        let want = feed_box(q);
        let cache = self.feeds.lock().unwrap();
        cache
            .values()
            .filter(|entry| entry.at.elapsed() < TTL)
            .filter(|entry| box_contains(entry.box_, want) || (same_radar && entry.radar == radar))
            .max_by_key(|entry| entry.at)
            .map(|entry| entry.reports.clone())
    }

    fn store_feed(&self, key: BoxKey, q: &Query, reports: Vec<MetarReport>) {
        let mut cache = self.feeds.lock().unwrap();
        cache.insert(
            key,
            CachedFeed {
                at: Instant::now(),
                radar: (tenths(q.lat), tenths(q.lon)),
                box_: fetch_box(q),
                reports,
            },
        );
        prune_map(
            &mut cache,
            |entry| entry.at.elapsed() < TTL,
            |entry| entry.at,
        );
    }

    fn cached_stations_for(&self, q: &Query, same_radar: bool) -> Option<HashMap<String, u8>> {
        let radar = (tenths(q.lat), tenths(q.lon));
        let want = feed_box(q);
        let cache = self.stations.lock().unwrap();
        cache
            .values()
            .filter(|entry| entry.at.elapsed() < STATIONS_TTL)
            .filter(|entry| box_contains(entry.box_, want) || (same_radar && entry.radar == radar))
            .max_by_key(|entry| entry.at)
            .map(|entry| entry.by_id.clone())
    }

    fn store_stations(&self, key: BoxKey, q: &Query, by_id: HashMap<String, u8>) {
        let mut cache = self.stations.lock().unwrap();
        cache.insert(
            key,
            CachedStations {
                at: Instant::now(),
                radar: (tenths(q.lat), tenths(q.lon)),
                box_: fetch_box(q),
                by_id,
            },
        );
        prune_map(
            &mut cache,
            |entry| entry.at.elapsed() < STATIONS_TTL,
            |entry| entry.at,
        );
    }

    fn backing_off(&self) -> bool {
        self.back_off_until
            .lock()
            .unwrap()
            .is_some_and(|until| Instant::now() < until)
    }

    fn note_failure(&self) {
        *self.back_off_until.lock().unwrap() = Some(Instant::now() + BACK_OFF);
    }

    async fn fetch_metars(&self, box_: BBox) -> Result<Vec<MetarReport>, String> {
        let bytes = if let Some(path) = &self.fixture {
            fs::read(path).map_err(|e| format!("metar fixture: {e}"))?
        } else {
            self.http_get(&self.url, box_, true).await?
        };
        parse_body(&bytes)
    }

    async fn fetch_stations(&self, box_: BBox) -> Result<HashMap<String, u8>, String> {
        let bytes = if let Some(path) = &self.stations_fixture {
            fs::read(path).map_err(|e| format!("stations fixture: {e}"))?
        } else {
            self.http_get(&self.stations_url, box_, false).await?
        };
        parse_stations(&bytes)
    }

    async fn http_get(&self, base: &str, box_: BBox, hours: bool) -> Result<Vec<u8>, String> {
        let _permit = self.permits.acquire().await.map_err(|e| e.to_string())?;
        let response = match self.client.get(bbox_url(base, box_, hours)).send().await {
            Ok(response) => response,
            Err(e) => {
                self.note_failure();
                return Err(e.to_string());
            }
        };
        match disposition(response.status()) {
            Ok(BodyPlan::Empty) => Ok(Vec::new()),
            Ok(BodyPlan::Read) => take_body(response).await,
            Err(Failure::Backoff(message)) => {
                self.note_failure();
                Err(message)
            }
            Err(Failure::Failed(message)) => Err(message),
        }
    }
}

enum BodyPlan {
    Empty,
    Read,
}

enum Failure {
    Backoff(String),
    Failed(String),
}

/// 204 is a valid empty feed. 429 and 5xx back off. Other failures do not.
fn disposition(status: reqwest::StatusCode) -> Result<BodyPlan, Failure> {
    if status == reqwest::StatusCode::NO_CONTENT {
        return Ok(BodyPlan::Empty);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return Err(Failure::Backoff(format!("HTTP {status}")));
    }
    if !status.is_success() {
        return Err(Failure::Failed(format!("HTTP {status}")));
    }
    Ok(BodyPlan::Read)
}

async fn take_body(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err("body over the size limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if chunk.len() > MAX_BODY - bytes.len() {
            return Err("body over the size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn prune_map<V>(
    map: &mut HashMap<BoxKey, V>,
    fresh: impl Fn(&V) -> bool,
    at: impl Fn(&V) -> Instant,
) {
    map.retain(|_, entry| fresh(entry));
    while map.len() > CACHE_CAP {
        let Some(oldest) = map
            .iter()
            .min_by_key(|(_, entry)| at(entry))
            .map(|(key, _)| *key)
        else {
            break;
        };
        map.remove(&oldest);
    }
}

fn parse_body(bytes: &[u8]) -> Result<Vec<MetarReport>, String> {
    if blank(bytes) {
        return Ok(Vec::new());
    }
    let rows: Vec<AwcMetar> = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let mut by_id: HashMap<String, MetarReport> = HashMap::new();
    for row in rows {
        let Some(id) = row.icao_id.as_deref() else {
            continue;
        };
        let id = id.trim().to_uppercase();
        if id.is_empty() || row.raw_ob.trim().is_empty() {
            continue;
        }
        if !(-90.0..=90.0).contains(&row.lat) || !(-180.0..=180.0).contains(&row.lon) {
            continue;
        }
        let category = category(&row);
        // No flight category: the airport is a non-response. No chip.
        if category.is_empty() {
            continue;
        }
        let report = MetarReport {
            id: id.clone(),
            lat: row.lat,
            lon: row.lon,
            category,
            raw: row.raw_ob.trim().to_string(),
            obs_time: row
                .obs_time
                .and_then(|secs| format_obs(secs as i64))
                .unwrap_or_default(),
        };
        match by_id.get(&id) {
            Some(prev) if prev.obs_time >= report.obs_time => {}
            _ => {
                by_id.insert(id, report);
            }
        }
    }
    Ok(by_id.into_values().collect())
}

/// US / Canada / Hawaii / Guam / Puerto Rico. Not [`crate::envelope::nexrad_network`]:
/// that clip includes RKJK (Korea) and LPLA (Azores).
fn covered(lon: f64, lat: f64) -> bool {
    // Hawaii
    if (18.5..=22.5).contains(&lat) && (-160.5..=-154.5).contains(&lon) {
        return true;
    }
    // Guam
    if (13.2..=13.7).contains(&lat) && (144.6..=145.0).contains(&lon) {
        return true;
    }
    // Puerto Rico / US Virgin Islands
    if (17.6..=18.6).contains(&lat) && (-67.5..=-64.4).contains(&lon) {
        return true;
    }
    // CONUS, Canada, Alaska east of 170W. West of 52W keeps Azores/Europe out.
    let north_america = (24.0..=72.0).contains(&lat) && (-170.0..=-52.0).contains(&lon);
    // Western Aleutians wrap past 180.
    let west_alaska = (51.0..=72.0).contains(&lat) && lon >= 172.0;
    north_america || west_alaska
}

/// AWC METAR is worldwide. This overlay keeps ICAO `K`, `C`, `P`, `M`,
/// and `TI` / `TJ`.
fn awc_na_station(id: &str) -> bool {
    let b = id.as_bytes();
    match b.first() {
        Some(b'K' | b'C' | b'P' | b'M') => true,
        Some(b'T') => b.len() >= 2 && (b[1] == b'J' || b[1] == b'I'),
        _ => false,
    }
}

fn select(
    mut reports: Vec<MetarReport>,
    q: &Query,
    priorities: &HashMap<String, u8>,
) -> Vec<MetarReport> {
    reports.retain(|r| awc_na_station(&r.id));
    if let Some(box_) = q.bbox {
        reports.retain(|r| in_bbox(r.lat, r.lon, box_));
    } else {
        reports.retain(|r| great_circle_km(q.lat, q.lon, r.lat, r.lon) <= RADIUS_KM);
    }
    let mut by_id: HashMap<String, MetarReport> =
        reports.into_iter().map(|r| (r.id.clone(), r)).collect();
    let mut chosen = Vec::new();
    for id in &q.always_on {
        if let Some(report) = by_id.remove(id) {
            chosen.push(report);
            if chosen.len() >= q.limit {
                return chosen;
            }
        }
    }
    let mut rest: Vec<MetarReport> = by_id.into_values().collect();
    rest.sort_by(|a, b| {
        let pri = if q.pick == Pick::Priority {
            let pa = *priorities.get(&a.id).unwrap_or(&MISSING_PRIORITY);
            let pb = *priorities.get(&b.id).unwrap_or(&MISSING_PRIORITY);
            pa.cmp(&pb)
        } else {
            std::cmp::Ordering::Equal
        };
        pri.then(
            great_circle_km(q.lat, q.lon, a.lat, a.lon)
                .total_cmp(&great_circle_km(q.lat, q.lon, b.lat, b.lon)),
        )
        .then(a.id.cmp(&b.id))
    });
    for report in rest {
        chosen.push(report);
        if chosen.len() >= q.limit {
            break;
        }
    }
    chosen
}

fn in_bbox(lat: f64, lon: f64, box_: BBox) -> bool {
    lat >= box_.south && lat <= box_.north && lon >= box_.west && lon <= box_.east
}

fn box_contains(outer: BBox, inner: BBox) -> bool {
    outer.south <= inner.south
        && outer.north >= inner.north
        && outer.west <= inner.west
        && outer.east >= inner.east
}

impl BoxKey {
    fn hour(box_: BBox) -> Self {
        Self::new(box_, Utc::now().timestamp().div_euclid(3600))
    }

    fn day(box_: BBox) -> Self {
        Self::new(box_, Utc::now().timestamp().div_euclid(86400))
    }

    fn new(box_: BBox, bucket: i64) -> Self {
        Self {
            south: tenths(box_.south),
            west: tenths(box_.west),
            north: tenths(box_.north),
            east: tenths(box_.east),
            bucket,
        }
    }
}

fn tenths(value: f64) -> i32 {
    (value * 10.0).round() as i32
}

/// The box actually fetched, expanded to the 0.1° cache grid so a later
/// query for the same selection reuses the feed instead of fetching again.
fn feed_box(q: &Query) -> BBox {
    let exact = q
        .bbox
        .unwrap_or_else(|| radius_bbox(q.lat, q.lon, RADIUS_KM));
    BBox {
        south: tenth_floor(exact.south).clamp(-90.0, 90.0),
        west: tenth_floor(exact.west).clamp(-180.0, 180.0),
        north: tenth_ceil(exact.north).clamp(-90.0, 90.0),
        east: tenth_ceil(exact.east).clamp(-180.0, 180.0),
    }
}

fn fetch_box(q: &Query) -> BBox {
    let want = feed_box(q);
    if q.bbox.is_none() {
        return want;
    }
    let lat_pad = ((want.north - want.south) * VIEW_PAD).min(MAX_PAD_DEG);
    let lon_pad = ((want.east - want.west) * VIEW_PAD).min(MAX_PAD_DEG);
    BBox {
        south: tenth_floor(want.south - lat_pad).clamp(-90.0, 90.0),
        west: tenth_floor(want.west - lon_pad).clamp(-180.0, 180.0),
        north: tenth_ceil(want.north + lat_pad).clamp(-90.0, 90.0),
        east: tenth_ceil(want.east + lon_pad).clamp(-180.0, 180.0),
    }
}

fn tenth_floor(value: f64) -> f64 {
    (value * 10.0).floor() / 10.0
}

fn tenth_ceil(value: f64) -> f64 {
    (value * 10.0).ceil() / 10.0
}

fn bbox_url(base: &str, box_: BBox, hours: bool) -> String {
    let join = if base.contains('?') { '&' } else { '?' };
    let hours = if hours { "&hours=1" } else { "" };
    format!(
        "{base}{join}bbox={:.4},{:.4},{:.4},{:.4}&format=json{hours}",
        box_.south, box_.west, box_.north, box_.east
    )
}

fn blank(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| b.is_ascii_whitespace())
}

fn parse_stations(bytes: &[u8]) -> Result<HashMap<String, u8>, String> {
    if blank(bytes) {
        return Ok(HashMap::new());
    }
    let rows: Vec<AwcStation> = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let mut by_id = HashMap::new();
    for row in rows {
        let Some(id) = row.icao_id.as_deref() else {
            continue;
        };
        let id = id.trim().to_uppercase();
        if id.is_empty() {
            continue;
        }
        if let Some(priority) = parse_priority(&row.priority) {
            by_id.insert(id, priority);
        }
    }
    Ok(by_id)
}

fn parse_priority(value: &serde_json::Value) -> Option<u8> {
    match value {
        serde_json::Value::Number(n) => n.as_u64().and_then(|n| u8::try_from(n).ok()),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn radius_bbox(lat: f64, lon: f64, km: f64) -> BBox {
    let (south, west, north, east) = bbox(lat, lon, km);
    BBox {
        south,
        west,
        north,
        east,
    }
}

/// FAA category, or empty when the observation cannot be colored safely.
/// An empty category is omitted from the reply.
fn category(row: &AwcMetar) -> String {
    let named = row.flt_cat.trim().to_lowercase();
    if matches!(named.as_str(), "vfr" | "mvfr" | "ifr" | "lifr") {
        return named;
    }
    let vis = parse_vis_sm(&row.visib);
    let sky = sky(row);
    // The worse of the two inputs wins, so a known LIFR value is enough.
    if vis.is_some_and(|v| v < 1.0) || matches!(sky, Sky::Base(feet) if feet < 500.0) {
        return "lifr".into();
    }
    let ceiling = match sky {
        Sky::NoCeiling => f64::INFINITY,
        Sky::Base(feet) => feet,
        Sky::Unknown => return String::new(),
    };
    let Some(vis) = vis else {
        return String::new();
    };
    from_vis_ceiling(vis, ceiling)
}

enum Sky {
    Unknown,
    NoCeiling,
    Base(f64),
}

/// FAA flight category from visibility (statute miles) and ceiling (feet).
/// No ceiling is an unlimited ceiling.
fn from_vis_ceiling(vis: f64, ceiling: f64) -> String {
    if ceiling < 500.0 || vis < 1.0 {
        "lifr".into()
    } else if ceiling < 1000.0 || vis < 3.0 {
        "ifr".into()
    } else if ceiling <= 3000.0 || vis <= 5.0 {
        "mvfr".into()
    } else {
        "vfr".into()
    }
}

fn parse_vis_sm(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => {
            let t = s.trim().trim_end_matches('+');
            if let Ok(n) = t.parse::<f64>() {
                return Some(n);
            }
            if let Some((a, b)) = t.split_once('/') {
                let n = a.parse::<f64>().ok()?;
                let d = b.parse::<f64>().ok()?;
                if d != 0.0 {
                    return Some(n / d);
                }
            }
            None
        }
        _ => None,
    }
}

fn sky(row: &AwcMetar) -> Sky {
    if let Some(vv) = row.vert_vis.filter(|v| *v > 0.0) {
        return Sky::Base(vv);
    }
    let Some(clouds) = row.clouds.as_ref() else {
        return Sky::Unknown;
    };
    let mut ceiling: Option<f64> = None;
    for cloud in clouds {
        if !matches!(
            cloud.cover.to_uppercase().as_str(),
            "BKN" | "OVC" | "VV" | "OVX"
        ) {
            continue;
        }
        let Some(base) = cloud.base else {
            return Sky::Unknown;
        };
        ceiling = Some(ceiling.map_or(base, |lowest| lowest.min(base)));
    }
    match ceiling {
        Some(feet) => Sky::Base(feet),
        None => Sky::NoCeiling,
    }
}

fn format_obs(secs: i64) -> Option<String> {
    DateTime::from_timestamp(secs, 0).map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn bbox(lat: f64, lon: f64, km: f64) -> (f64, f64, f64, f64) {
    let dlat = km / 111.0;
    let dlon = km / (111.0 * lat.to_radians().cos().abs().max(0.2));
    (
        (lat - dlat).clamp(-90.0, 90.0),
        (lon - dlon).clamp(-180.0, 180.0),
        (lat + dlat).clamp(-90.0, 90.0),
        (lon + dlon).clamp(-180.0, 180.0),
    )
}

fn great_circle_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_KM * h.clamp(0.0, 1.0).sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MetarReport;
    use std::collections::HashMap;

    const KTLX: (f64, f64) = (35.33306, -97.27748);
    const FIXTURE: &str = include_str!("../tests/fixtures/metar-ktlx.json");
    const STATIONS: &str = include_str!("../tests/fixtures/stations-ktlx.json");

    fn nearest_query(limit: usize) -> Query {
        Query {
            lat: KTLX.0,
            lon: KTLX.1,
            bbox: None,
            pick: Pick::Nearest,
            limit,
            always_on: vec![],
        }
    }

    fn priorities() -> HashMap<String, u8> {
        parse_stations(STATIONS.as_bytes()).unwrap()
    }

    #[test]
    fn fixture_ranks_sixteen_nearest_to_ktlx() {
        let reports = select(
            parse_body(FIXTURE.as_bytes()).unwrap(),
            &nearest_query(LIMIT),
            &HashMap::new(),
        );
        assert_eq!(reports.len(), LIMIT);
        assert_eq!(reports[0].id, "KTIK");
        assert!(reports.iter().any(|r| r.id == "KOKC"));
        assert!(reports.iter().all(|r| !r.raw.is_empty()));
        let far = great_circle_km(
            KTLX.0,
            KTLX.1,
            reports.last().unwrap().lat,
            reports.last().unwrap().lon,
        );
        let near = great_circle_km(KTLX.0, KTLX.1, reports[0].lat, reports[0].lon);
        assert!(near <= far);
    }

    #[test]
    fn count_shrinks_the_pool() {
        let reports = select(
            parse_body(FIXTURE.as_bytes()).unwrap(),
            &nearest_query(4),
            &HashMap::new(),
        );
        assert_eq!(reports.len(), 4);
        assert_eq!(reports[0].id, "KTIK");
    }

    #[test]
    fn priority_prefers_hubs_over_near_fields() {
        let q = Query {
            lat: KTLX.0,
            lon: KTLX.1,
            bbox: Some(BBox {
                south: 34.0,
                west: -99.0,
                north: 37.0,
                east: -95.0,
            }),
            pick: Pick::Priority,
            limit: 16,
            always_on: vec![],
        };
        let reports = select(parse_body(FIXTURE.as_bytes()).unwrap(), &q, &priorities());
        assert_eq!(reports[0].id, "KOKC");
        let tik = reports.iter().position(|r| r.id == "KTIK").unwrap();
        let okc = reports.iter().position(|r| r.id == "KOKC").unwrap();
        assert!(okc < tik);
    }

    #[test]
    fn always_on_pins_a_home_field_in_view() {
        let q = Query {
            lat: KTLX.0,
            lon: KTLX.1,
            bbox: Some(BBox {
                south: 34.0,
                west: -99.0,
                north: 37.0,
                east: -95.0,
            }),
            pick: Pick::Priority,
            limit: 4,
            always_on: vec!["KOUN".into()],
        };
        let reports = select(parse_body(FIXTURE.as_bytes()).unwrap(), &q, &priorities());
        assert_eq!(reports.len(), 4);
        assert_eq!(reports[0].id, "KOUN");
        assert_eq!(reports[1].id, "KOKC");
    }

    fn report(id: &str, lat: f64, lon: f64) -> MetarReport {
        MetarReport {
            id: id.into(),
            lat,
            lon,
            category: "vfr".into(),
            raw: format!("{id} TEST"),
            obs_time: String::new(),
        }
    }

    #[test]
    fn kbna_in_view_drops_km19_when_priority_fills() {
        // KNQA view that reaches Nashville (KOHX is the radar landmark).
        // KBNA is priority 1; KM19 is 5. Sixteen better-ranked stations
        // in that box crowd KM19 out.
        let knqa = (35.3566, -89.8704);
        let mut reports = vec![
            report("KBNA", 36.1245, -86.6782),
            report("KM19", 35.6377, -91.1764),
            report("KMEM", 35.0424, -89.9767),
        ];
        let mut pri = HashMap::from([("KBNA".into(), 1u8), ("KMEM".into(), 2), ("KM19".into(), 5)]);
        for i in 0..14 {
            let id = format!("K{i:02}X");
            reports.push(report(&id, 35.4, -89.9 - i as f64 * 0.05));
            pri.insert(id, 4);
        }
        let q = Query {
            lat: knqa.0,
            lon: knqa.1,
            bbox: Some(BBox {
                south: 34.5,
                west: -92.5,
                north: 36.5,
                east: -86.4,
            }),
            pick: Pick::Priority,
            limit: 16,
            always_on: vec![],
        };
        let chosen = select(reports, &q, &pri);
        assert_eq!(chosen.len(), 16);
        assert!(chosen.iter().any(|r| r.id == "KBNA"));
        assert!(chosen.iter().any(|r| r.id == "KMEM"));
        assert!(chosen.iter().all(|r| r.id != "KM19"));
    }

    #[test]
    fn bbox_drops_stations_off_screen() {
        let q = Query {
            lat: KTLX.0,
            lon: KTLX.1,
            bbox: Some(BBox {
                south: 35.3,
                west: -97.5,
                north: 35.5,
                east: -97.2,
            }),
            pick: Pick::Nearest,
            limit: 16,
            always_on: vec![],
        };
        let reports = select(parse_body(FIXTURE.as_bytes()).unwrap(), &q, &HashMap::new());
        assert!(reports.iter().any(|r| r.id == "KTIK"));
        assert!(reports.iter().all(|r| r.id != "KTUL"));
    }

    #[test]
    fn parse_query_rejects_bad_pick_and_limit() {
        assert!(
            Query::parse(
                KTLX.0,
                KTLX.1,
                None,
                None,
                None,
                None,
                Some("hubs".into()),
                None,
                vec![]
            )
            .is_err()
        );
        assert!(
            Query::parse(
                KTLX.0,
                KTLX.1,
                None,
                None,
                None,
                None,
                None,
                Some(32),
                vec![]
            )
            .is_err()
        );
        assert!(
            Query::parse(
                KTLX.0,
                KTLX.1,
                None,
                None,
                None,
                None,
                Some("priority".into()),
                None,
                vec![]
            )
            .is_err()
        );
    }

    #[test]
    fn categories_follow_awc_then_faa_vis_ceiling() {
        let reports = parse_body(FIXTURE.as_bytes()).unwrap();
        let cat = |id: &str| {
            reports
                .iter()
                .find(|r| r.id == id)
                .unwrap()
                .category
                .as_str()
        };
        assert_eq!(cat("KOKC"), "vfr");
        assert_eq!(cat("KPWA"), "mvfr");
        assert_eq!(cat("KTIK"), "ifr");
        assert_eq!(cat("KADM"), "lifr");
        assert_eq!(cat("KSRE"), "vfr"); // no fltCat; SCT 3500 / 10 SM
    }

    #[test]
    fn vis_fractions_and_plus_parse() {
        assert_eq!(parse_vis_sm(&serde_json::json!("10+")), Some(10.0));
        assert_eq!(parse_vis_sm(&serde_json::json!("1/2")), Some(0.5));
        assert_eq!(parse_vis_sm(&serde_json::json!(4)), Some(4.0));
    }

    #[test]
    fn faa_breakpoints() {
        assert_eq!(from_vis_ceiling(0.5, 8000.0), "lifr");
        assert_eq!(from_vis_ceiling(10.0, 400.0), "lifr");
        assert_eq!(from_vis_ceiling(2.0, 8000.0), "ifr");
        assert_eq!(from_vis_ceiling(10.0, 800.0), "ifr");
        assert_eq!(from_vis_ceiling(4.0, 8000.0), "mvfr");
        assert_eq!(from_vis_ceiling(10.0, 2000.0), "mvfr");
        assert_eq!(from_vis_ceiling(10.0, 3500.0), "vfr");
        assert_eq!(from_vis_ceiling(10.0, f64::INFINITY), "vfr");
    }

    #[test]
    fn coverage_is_us_canada_not_korea_or_azores() {
        assert!(covered(KTLX.1, KTLX.0));
        assert!(covered(-79.629, 43.679)); // CYYZ
        assert!(covered(-150.0, 61.2)); // PANC
        assert!(covered(-157.9, 21.3)); // PHNL
        assert!(covered(-66.0, 18.4)); // TJSJ
        assert!(!covered(-0.1, 51.5)); // London OPERA
        assert!(!covered(126.616, 35.903)); // RKJK
        assert!(!covered(-27.091, 38.762)); // LPLA
        assert!(crate::envelope::nexrad_network(126.616, 35.903));
        assert!(crate::envelope::nexrad_network(-27.091, 38.762));
    }

    #[test]
    fn select_keeps_prefixes_inside_250_km() {
        let q = nearest_query(16);
        let reports = vec![
            report("KTIK", 35.4147, -97.3867),
            report("CYYZ", 35.50, -97.40),
            report("MMMX", 35.20, -97.50),
            report("EGLL", 35.30, -97.30),
            report("LFPG", 35.36, -97.20),
            report("KFAR", 38.50, -97.27748),
        ];
        let chosen = select(reports, &q, &HashMap::new());
        let ids: Vec<_> = chosen.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"KTIK"));
        assert!(ids.contains(&"CYYZ"));
        assert!(ids.contains(&"MMMX"));
        assert!(!ids.contains(&"EGLL"));
        assert!(!ids.contains(&"LFPG"));
        assert!(!ids.contains(&"KFAR"));
    }

    #[test]
    fn unknown_category_is_omitted() {
        let json = br#"[
            {"icaoId":"KAAA","rawOb":"KAAA NODATA","lat":35.4,"lon":-97.6},
            {"icaoId":"KBBB","rawOb":"KBBB 10SM","lat":35.4,"lon":-97.6,"visib":"10","clouds":[]},
            {"icaoId":"KCCC","rawOb":"KCCC 1/2SM","lat":35.4,"lon":-97.6,"visib":"1/2"},
            {"icaoId":null,"rawOb":"NONE","lat":35.4,"lon":-97.6,"fltCat":"VFR"}
        ]"#;
        let reports = parse_body(json).unwrap();
        let ids: Vec<_> = reports.iter().map(|r| r.id.as_str()).collect();
        assert!(!ids.contains(&"KAAA"));
        assert!(
            reports
                .iter()
                .any(|r| r.id == "KBBB" && r.category == "vfr")
        );
        assert!(
            reports
                .iter()
                .any(|r| r.id == "KCCC" && r.category == "lifr")
        );
    }

    #[test]
    fn missing_obs_time_stays_empty() {
        let json = br#"[{"icaoId":"KOKC","rawOb":"KOKC","lat":35.4,"lon":-97.6,"fltCat":"VFR"}]"#;
        let reports = parse_body(json).unwrap();
        assert_eq!(reports[0].obs_time, "");
    }

    #[test]
    fn empty_body_is_no_reports() {
        assert!(parse_body(b"").unwrap().is_empty());
        assert!(parse_body(b"  ").unwrap().is_empty());
        assert!(parse_stations(b"").unwrap().is_empty());
    }

    #[test]
    fn null_station_rows_are_skipped() {
        let json = br#"[
            {"icaoId":null,"priority":1},
            {"icaoId":"KOKC","priority":2},
            {"priority":3},
            {"icaoId":"KTUL","priority":"nope"}
        ]"#;
        let map = parse_stations(json).unwrap();
        assert_eq!(map.get("KOKC"), Some(&2));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn http_204_is_empty_and_429_backs_off() {
        assert!(matches!(
            disposition(reqwest::StatusCode::NO_CONTENT),
            Ok(BodyPlan::Empty)
        ));
        assert!(matches!(
            disposition(reqwest::StatusCode::OK),
            Ok(BodyPlan::Read)
        ));
        assert!(matches!(
            disposition(reqwest::StatusCode::TOO_MANY_REQUESTS),
            Err(Failure::Backoff(_))
        ));
        assert!(matches!(
            disposition(reqwest::StatusCode::BAD_GATEWAY),
            Err(Failure::Backoff(_))
        ));
        assert!(matches!(
            disposition(reqwest::StatusCode::BAD_REQUEST),
            Err(Failure::Failed(_))
        ));
    }

    #[test]
    fn repeat_selection_does_not_read_the_fixture_again() {
        let path =
            std::env::temp_dir().join(format!("omastorm-metar-cache-{}.json", std::process::id()));
        fs::write(&path, FIXTURE).unwrap();
        let service = Service::for_tests("http://127.0.0.1:9", Some(path.clone()), None);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let query = nearest_query(4);
        let first = runtime.block_on(service.query(query.clone())).unwrap();
        fs::remove_file(&path).unwrap();
        let second = runtime.block_on(service.query(query)).unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].id, "KTIK");
    }

    #[test]
    fn same_radar_reuses_the_feed_when_the_box_jitters() {
        let path =
            std::env::temp_dir().join(format!("omastorm-metar-jitter-{}.json", std::process::id()));
        fs::write(&path, FIXTURE).unwrap();
        let service = Service::for_tests("http://127.0.0.1:9", Some(path.clone()), None);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut first_q = nearest_query(4);
        first_q.bbox = Some(BBox {
            south: 34.04,
            west: -98.0,
            north: 36.0,
            east: -96.0,
        });
        let first = runtime.block_on(service.query(first_q)).unwrap();
        fs::remove_file(&path).unwrap();
        let mut second_q = nearest_query(4);
        second_q.bbox = Some(BBox {
            south: 34.16,
            west: -98.1,
            north: 36.1,
            east: -95.9,
        });
        let second = runtime.block_on(service.query(second_q)).unwrap();
        assert_eq!(first[0].id, second[0].id);
    }

    #[test]
    fn same_radar_refetches_a_wider_box() {
        let path =
            std::env::temp_dir().join(format!("omastorm-metar-wider-{}.json", std::process::id()));
        fs::write(&path, "[]").unwrap();
        let service = Service::for_tests("http://127.0.0.1:9", Some(path.clone()), None);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let first = runtime.block_on(service.query(nearest_query(4))).unwrap();
        assert!(first.is_empty());
        fs::write(&path, FIXTURE).unwrap();
        let mut wide = nearest_query(4);
        wide.bbox = Some(BBox {
            south: 30.0,
            west: -103.0,
            north: 40.0,
            east: -92.0,
        });
        let second = runtime.block_on(service.query(wide)).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(!second.is_empty());
    }

    #[test]
    fn backoff_still_serves_a_fresh_cache() {
        let path = std::env::temp_dir().join(format!(
            "omastorm-metar-backoff-{}.json",
            std::process::id()
        ));
        fs::write(&path, FIXTURE).unwrap();
        let service = Service::for_tests("http://127.0.0.1:9", Some(path.clone()), None);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let query = nearest_query(4);
        let first = runtime.block_on(service.query(query.clone())).unwrap();
        fs::remove_file(&path).unwrap();
        service.note_failure();
        let second = runtime.block_on(service.query(query)).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn duplicate_ids_keep_the_newer_observation() {
        let json = br#"[
            {"icaoId":"KOKC","rawOb":"KOKC OLD","lat":35.4,"lon":-97.6,"obsTime":100,"fltCat":"VFR"},
            {"icaoId":"KOKC","rawOb":"KOKC NEW","lat":35.4,"lon":-97.6,"obsTime":200,"fltCat":"IFR"}
        ]"#;
        let reports = parse_body(json).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].raw, "KOKC NEW");
        assert_eq!(reports[0].category, "ifr");
    }
}
