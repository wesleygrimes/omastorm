//! Global air quality for the `aqi_*` commands (`docs/protocol.md`). Two
//! sources: Open-Meteo's Air Quality API (model analysis, no key) and, with
//! the user's token, WAQI's ground stations (their China-scale index plus the
//! pollutant breakdown). Raw pollutant concentrations (µg/m³) are the one
//! comparable datapoint; the indices are labeled regional wrappers, and this
//! module computes only the two the sources do not serve: China's HJ 633-2012
//! index and India's CPCB NAQI. US and European indices pass through from
//! Open-Meteo, whose published breakpoint tables (EEA-2024, EPA-2024) are the
//! normative reference for those scales.
//!
//! The `Service` holds the caches and the bounds politeness state behind a
//! short lock; the network fetches run without it (the `fetch_*` functions),
//! so a slow source never holds up the shared state or another client.

use crate::protocol::{Aqi, StationAqi, VERSION};
use serde_json::Value;
use std::{
    collections::HashMap,
    env,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const OPEN_METEO: &str = "https://air-quality-api.open-meteo.com/v1/air-quality";
const WAQI: &str = "https://api.waqi.info";
const USER_AGENT: &str = "omastorm (https://omastorm.com)";
/// One network answer, whichever source.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// A point's cache key: readings within this many degrees share a slot, so a
/// small pan does not re-fetch.
const GRID_DEG: f64 = 0.05;
/// A bounds request rounds its rectangle to this, so panning fetches one
/// rectangle at a time rather than one per settle.
const BOUNDS_ROUND_DEG: f64 = 0.5;
/// Point answers: refetched after the TTL, served stale up to two hours
/// while the fetch path is down, dropped after a day.
const POINT_TTL: Duration = Duration::from_secs(30 * 60);
const POINT_STALE: Duration = Duration::from_secs(2 * 60 * 60);
const POINT_DROP: Duration = Duration::from_secs(24 * 60 * 60);
/// Station lists: WAQI's stations update on minutes, so two minutes.
const BOUNDS_TTL: Duration = Duration::from_secs(2 * 60);
/// One station's breakdown: five minutes.
const DETAIL_TTL: Duration = Duration::from_secs(5 * 60);
/// Two bounds fetches stay this far apart, doubling to the cap after a
/// failure and resetting after a success. WAQI's quota is generous
/// (1,000 requests a second); this paces a free community API, not a wall.
const BOUNDS_GAP: Duration = Duration::from_secs(10);
const BOUNDS_GAP_MAX: Duration = Duration::from_secs(60);

type GridKey = (i64, i64);
type BoundsKey = (i64, i64, i64, i64);

struct Timed<T> {
    value: T,
    fetched: Instant,
}

impl<T> Timed<T> {
    fn within(&self, ttl: Duration) -> bool {
        self.fetched.elapsed() < ttl
    }
}

/// Unix seconds, floored.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The UTC day a politeness counter belongs to; it resets on the roll.
fn day_number() -> u64 {
    now_secs() / (24 * 60 * 60)
}

/// The bounds endpoint's soft daily budget: a panning session stays a guest.
/// `OMASTORM_AQI_BUDGET` names another number for the checks.
fn budget_from_env() -> u32 {
    env::var("OMASTORM_AQI_BUDGET")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(500)
}

/// The client, the three caches, and the bounds pacing. Created once in
/// `serve`; the token never lives here, only in the commands that ask.
pub struct Service {
    client: reqwest::Client,
    points: HashMap<GridKey, Timed<Aqi>>,
    bounds: HashMap<BoundsKey, Timed<Vec<StationAqi>>>,
    details: HashMap<u32, Timed<Aqi>>,
    last_bounds: Instant,
    bounds_gap: Duration,
    budget: (u64, u32),
    budget_daily: u32,
}

impl Service {
    /// Build the HTTP client. Fetches nothing.
    pub fn new() -> Result<Service, String> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|e| format!("Air quality client: {e}"))?;
        Ok(Service {
            client,
            points: HashMap::new(),
            bounds: HashMap::new(),
            details: HashMap::new(),
            last_bounds: Instant::now() - BOUNDS_GAP,
            bounds_gap: BOUNDS_GAP,
            budget: (day_number(), 0),
            budget_daily: budget_from_env(),
        })
    }

    /// The cached point answer for `key`, when it is within its window.
    pub fn point_cached(&self, lat: f64, lon: f64) -> Option<Aqi> {
        let key = grid_key(lat, lon);
        let cached = self.points.get(&key)?;
        (!cached.within(POINT_DROP) && cached.within(POINT_TTL)).then(|| cached.value.clone())
    }

    /// The stale point answer, served while a fetch keeps failing.
    pub fn point_stale(&self, lat: f64, lon: f64) -> Option<Aqi> {
        let key = grid_key(lat, lon);
        let cached = self.points.get(&key)?;
        (!cached.within(POINT_DROP) && cached.within(POINT_STALE)).then(|| cached.value.clone())
    }

    /// Record a fetched point answer under its grid slot.
    pub fn point_store(&mut self, lat: f64, lon: f64, mut aqi: Aqi) {
        let key = grid_key(lat, lon);
        aqi.lat = (key.0 as f64 + 0.5) * GRID_DEG;
        aqi.lon = (key.1 as f64 + 0.5) * GRID_DEG;
        complete(&mut aqi);
        self.points.insert(
            key,
            Timed {
                value: aqi,
                fetched: Instant::now(),
            },
        );
    }

    /// The cached station list for a bounds rectangle, when fresh.
    pub fn stations_cached(
        &self,
        lat0: f64,
        lon0: f64,
        lat1: f64,
        lon1: f64,
    ) -> Option<Vec<StationAqi>> {
        let cached = self.bounds.get(&bounds_key(lat0, lon0, lat1, lon1))?;
        cached.within(BOUNDS_TTL).then(|| cached.value.clone())
    }

    /// Any station list cached for the rectangle, however stale, when the
    /// pacing or the politeness budget says not to fetch now.
    pub fn bounds_deferred(
        &self,
        lat0: f64,
        lon0: f64,
        lat1: f64,
        lon1: f64,
    ) -> Option<Vec<StationAqi>> {
        let key = bounds_key(lat0, lon0, lat1, lon1);
        self.bounds.get(&key).map(|cached| cached.value.clone())
    }

    /// Whether a bounds fetch may go out now: past the gap, and inside the
    /// day's politeness budget, which it then spends.
    pub fn bounds_may_fetch(&mut self) -> bool {
        if self.last_bounds.elapsed() < self.bounds_gap {
            return false;
        }
        let (day, count) = self.budget;
        if day != day_number() {
            self.budget = (day_number(), 1);
            return true;
        }
        if count >= self.budget_daily {
            return false;
        }
        self.budget = (day, count + 1);
        true
    }

    pub fn bounds_stored(
        &mut self,
        lat0: f64,
        lon0: f64,
        lat1: f64,
        lon1: f64,
        stations: Vec<StationAqi>,
    ) {
        self.last_bounds = Instant::now();
        self.bounds_gap = BOUNDS_GAP;
        self.bounds.insert(
            bounds_key(lat0, lon0, lat1, lon1),
            Timed {
                value: stations,
                fetched: Instant::now(),
            },
        );
    }

    pub fn bounds_failed(&mut self) {
        self.last_bounds = Instant::now();
        self.bounds_gap = (self.bounds_gap * 2).min(BOUNDS_GAP_MAX);
    }

    /// The cached breakdown for one station, when fresh.
    pub fn detail_cached(&self, uid: u32) -> Option<Aqi> {
        let cached = self.details.get(&uid)?;
        cached.within(DETAIL_TTL).then(|| cached.value.clone())
    }

    pub fn detail_store(&mut self, uid: u32, aqi: Aqi) {
        self.details.insert(
            uid,
            Timed {
                value: aqi,
                fetched: Instant::now(),
            },
        );
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

/// The point grid slot a place falls in.
fn grid_key(lat: f64, lon: f64) -> GridKey {
    (
        (lat / GRID_DEG).floor() as i64,
        (lon / GRID_DEG).floor() as i64,
    )
}

/// The rounded bounds rectangle a request names, north/west/south/east.
fn bounds_key(lat0: f64, lon0: f64, lat1: f64, lon1: f64) -> BoundsKey {
    (
        ((lat0.max(lat1) / BOUNDS_ROUND_DEG).floor() as i64),
        ((lon0.min(lon1) / BOUNDS_ROUND_DEG).floor() as i64),
        ((lat0.min(lat1) / BOUNDS_ROUND_DEG).floor() as i64),
        ((lon0.max(lon1) / BOUNDS_ROUND_DEG).floor() as i64),
    )
}

/// One station per network may list the same physical site twice; the
/// reading is the same, so the first wins.
fn dedupe(stations: Vec<StationAqi>) -> Vec<StationAqi> {
    let mut seen = HashMap::new();
    let mut kept = Vec::new();
    for station in stations {
        if seen.insert(station.uid, ()).is_none() {
            kept.push(station);
        }
    }
    kept
}

/// Thin a station list to at most `max`: stations cluster into a grid whose
/// cell grows from a small fraction of the span until the clusters fit, and
/// each cluster keeps its worst reading (ties to the freshest, then the
/// smaller id, so the choice never depends on arrival order).
pub fn decimate(stations: Vec<StationAqi>, max: usize) -> Vec<StationAqi> {
    if max == 0 || stations.len() <= max {
        return stations;
    }
    let (mut min_lat, mut max_lat, mut min_lon, mut max_lon) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for s in &stations {
        min_lat = min_lat.min(s.lat);
        max_lat = max_lat.max(s.lat);
        min_lon = min_lon.min(s.lon);
        max_lon = max_lon.max(s.lon);
    }
    let span = (max_lat - min_lat).max(max_lon - min_lon).max(1e-6);
    let mut cell = span / 32.0;
    loop {
        let mut clusters: HashMap<(i64, i64), StationAqi> = HashMap::new();
        for s in stations.iter().cloned() {
            let key = ((s.lat / cell).floor() as i64, (s.lon / cell).floor() as i64);
            let keep = match clusters.get(&key) {
                None => true,
                Some(held) => match (held.aqi, s.aqi) {
                    (_, Some(b)) => match held.aqi {
                        Some(a) => {
                            b > a
                                || (b == a && (s.observed_at, s.uid) > (held.observed_at, held.uid))
                        }
                        None => true,
                    },
                    _ => false,
                },
            };
            if keep {
                clusters.insert(key, s);
            }
        }
        let kept: Vec<StationAqi> = clusters.into_values().collect();
        if kept.len() <= max {
            return kept;
        }
        cell *= 2.0;
    }
}

/// One pollutant's sub-index: linear between band bounds, clamped at the
/// top of the table, where both standards cap the index.
fn sub_index(value: f64, bands: &[(f64, u32)]) -> Option<u32> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let (mut lo_c, mut lo_i) = (0.0, 0.0);
    for (hi_c, hi_i) in bands {
        if value <= *hi_c {
            let within = lo_i + (f64::from(*hi_i) - lo_i) * (value - lo_c) / (hi_c - lo_c);
            return Some(within.round() as u32);
        }
        lo_c = *hi_c;
        lo_i = f64::from(*hi_i);
    }
    Some(bands.last()?.1)
}

/// China's HJ 633-2012 IAQI tables: 24 h averaging for PM, SO2, and NO2
/// (µg/m³), CO in mg/m³, 1 h for O3.
const CHINA_PM2_5: [(f64, u32); 7] = [
    (35.0, 50),
    (75.0, 100),
    (115.0, 150),
    (150.0, 200),
    (250.0, 300),
    (350.0, 400),
    (500.0, 500),
];
const CHINA_PM10: [(f64, u32); 7] = [
    (50.0, 50),
    (150.0, 100),
    (250.0, 150),
    (350.0, 200),
    (420.0, 300),
    (500.0, 400),
    (600.0, 500),
];
const CHINA_SO2: [(f64, u32); 7] = [
    (150.0, 50),
    (475.0, 100),
    (800.0, 150),
    (1600.0, 200),
    (2100.0, 300),
    (2620.0, 400),
    (2620.0, 500),
];
const CHINA_NO2: [(f64, u32); 7] = [
    (40.0, 50),
    (80.0, 100),
    (180.0, 150),
    (280.0, 200),
    (565.0, 300),
    (750.0, 400),
    (1100.0, 500),
];
const CHINA_O3: [(f64, u32); 7] = [
    (160.0, 50),
    (200.0, 100),
    (300.0, 150),
    (400.0, 200),
    (800.0, 300),
    (1000.0, 400),
    (1200.0, 500),
];
const CHINA_CO: [(f64, u32); 7] = [
    (2.0, 50),
    (4.0, 100),
    (14.0, 150),
    (24.0, 200),
    (36.0, 300),
    (48.0, 400),
    (60.0, 500),
];

/// India's CPCB National Air Quality Index: 24 h for PM, SO2, and NO2,
/// 8 h for CO (mg/m³) and O3.
const INDIA_PM2_5: [(f64, u32); 6] = [
    (30.0, 50),
    (60.0, 100),
    (90.0, 200),
    (120.0, 300),
    (250.0, 400),
    (380.0, 500),
];
const INDIA_PM10: [(f64, u32); 6] = [
    (50.0, 50),
    (100.0, 100),
    (250.0, 200),
    (350.0, 300),
    (430.0, 400),
    (510.0, 500),
];
const INDIA_SO2: [(f64, u32); 6] = [
    (40.0, 50),
    (80.0, 100),
    (380.0, 200),
    (800.0, 300),
    (1600.0, 400),
    (2400.0, 500),
];
const INDIA_NO2: [(f64, u32); 6] = [
    (40.0, 50),
    (80.0, 100),
    (180.0, 200),
    (280.0, 300),
    (400.0, 400),
    (1000.0, 500),
];
const INDIA_CO: [(f64, u32); 6] = [
    (1.0, 50),
    (2.0, 100),
    (10.0, 200),
    (17.0, 300),
    (34.0, 400),
    (46.0, 500),
];
const INDIA_O3: [(f64, u32); 6] = [
    (50.0, 50),
    (100.0, 100),
    (168.0, 200),
    (208.0, 300),
    (748.0, 400),
    (1000.0, 500),
];

/// The concentrations of one reading by the indices' names; CO converts
/// from the wire's µg/m³ to the tables' mg/m³. `so2` has no India band
/// beyond very poor in the table above, which the clamp covers.
fn pollutant(reading: &Aqi, name: &str) -> Option<f64> {
    match name {
        "pm2_5" => reading.pm2_5,
        "pm10" => reading.pm10,
        "o3" => reading.o3,
        "no2" => reading.no2,
        "so2" => reading.so2,
        "co" => reading.co.map(|co| co / 1000.0),
        _ => None,
    }
}

/// The worst pollutant sub-index, or `None` with no concentration at all.
fn max_index(reading: &Aqi, china: bool) -> Option<u32> {
    let mut best = None;
    for name in ["pm2_5", "pm10", "o3", "no2", "so2", "co"] {
        let Some(value) = pollutant(reading, name) else {
            continue;
        };
        let bands: &[(f64, u32)] = match (name, china) {
            ("pm2_5", true) => &CHINA_PM2_5,
            ("pm2_5", false) => &INDIA_PM2_5,
            ("pm10", true) => &CHINA_PM10,
            ("pm10", false) => &INDIA_PM10,
            ("o3", true) => &CHINA_O3,
            ("o3", false) => &INDIA_O3,
            ("no2", true) => &CHINA_NO2,
            ("no2", false) => &INDIA_NO2,
            ("so2", true) => &CHINA_SO2,
            ("so2", false) => &INDIA_SO2,
            (_, true) => &CHINA_CO,
            _ => &INDIA_CO,
        };
        if let Some(index) = sub_index(value, bands) {
            best = Some(best.map_or(index, |held: u32| held.max(index)));
        }
    }
    best
}

/// Fill the indices a reading lacks: China (WAQI's index when the source is
/// WAQI, computed here otherwise) and India, always computed here.
fn complete(reading: &mut Aqi) {
    if reading.china_aqi.is_none() {
        reading.china_aqi = max_index(reading, true);
    }
    if reading.india_aqi.is_none() {
        reading.india_aqi = max_index(reading, false);
    }
}

/// A WAQI payload's `status: "ok"` guard, naming its `data`.
fn waqi_data(value: &Value) -> Option<&Value> {
    let data = value.get("data")?;
    (value.get("status").and_then(Value::as_str) == Some("ok")).then_some(data)
}

/// WAQI's `aqi` field: a number, or a string (`"41"`, or `"-"` when a
/// station has no current reading).
fn parse_waqi_aqi(value: Option<&Value>) -> Option<u32> {
    match value? {
        Value::Number(n) => n.as_f64().filter(|n| *n >= 0.0).map(|n| n.round() as u32),
        Value::String(text) => text.trim().parse::<u32>().ok(),
        _ => None,
    }
}

/// A `/feed/geo` or `/feed/@uid` payload: the station and its pollutant
/// breakdown (IAQI keys normalized: WAQI writes `pm25`).
fn parse_feed(value: &Value) -> Option<(StationAqi, Vec<(String, f64)>)> {
    let data = waqi_data(value)?;
    let city = data.get("city")?;
    let (lat, lon) = match city.get("geo").and_then(Value::as_array)?.as_slice() {
        [Value::Number(lat), Value::Number(lon)] => (lat.as_f64()?, lon.as_f64()?),
        _ => return None,
    };
    let mut pollutants: Vec<(String, f64)> = Vec::new();
    if let Some(iaqi) = data.get("iaqi").and_then(Value::as_object) {
        for (name, value) in iaqi {
            if let Some(number) = value.get("v").and_then(Value::as_f64) {
                pollutants.push((normalize(name), number));
            }
        }
    }
    pollutants.sort_by(|a, b| a.0.cmp(&b.0));
    Some((
        StationAqi {
            uid: 0,
            lat,
            lon,
            aqi: parse_waqi_aqi(data.get("aqi")),
            name: city
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            observed_at: data
                .get("time")
                .and_then(|time| time.get("v"))
                .and_then(Value::as_f64)
                .map(|t| t as u64)
                .unwrap_or(0),
        },
        pollutants,
    ))
}

/// IAQI's `pm25` reads as `pm2_5` everywhere else in this module.
fn normalize(name: &str) -> String {
    if name == "pm25" {
        "pm2_5".to_owned()
    } else {
        name.to_owned()
    }
}

/// The `/map/bounds` payload: one station object each.
fn parse_bounds(value: &Value) -> Vec<StationAqi> {
    let Some(data) = waqi_data(value).and_then(Value::as_array) else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|station| {
            Some(StationAqi {
                uid: station.get("uid")?.as_u64()? as u32,
                lat: station.get("lat")?.as_f64()?,
                lon: station.get("lon")?.as_f64()?,
                aqi: parse_waqi_aqi(station.get("aqi")),
                name: station
                    .get("station")
                    .and_then(|s| s.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                observed_at: station
                    .get("station")
                    .and_then(|s| s.get("time"))
                    .and_then(Value::as_str)
                    .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
                    .map(|t| t.timestamp() as u64)
                    .unwrap_or(0),
            })
        })
        .collect()
}

/// One Open-Meteo `current` block into an `Aqi`: values may be `null` and
/// the two served indices may be absent. `time` is UTC without an offset.
fn parse_open_meteo(value: &Value) -> Option<Aqi> {
    let current = value.get("current")?;
    let number = |name: &str| current.get(name).and_then(Value::as_f64);
    let index = |name: &str| number(name).filter(|n| *n >= 0.0).map(|n| n.round() as u32);
    let observed_at = current
        .get("time")
        .and_then(Value::as_str)
        .and_then(|time| {
            chrono::NaiveDateTime::parse_from_str(time, "%Y-%m-%dT%H:%M")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(time, "%Y-%m-%dT%H:%M:%S"))
                .ok()
        })
        .map(|t| t.and_utc().timestamp() as u64)
        .unwrap_or(0);
    Some(Aqi {
        v: VERSION,
        lat: 0.0,
        lon: 0.0,
        source: "open-meteo",
        observed_at,
        pm2_5: number("pm2_5"),
        pm10: number("pm10"),
        o3: number("ozone"),
        no2: number("nitrogen_dioxide"),
        so2: number("sulphur_dioxide"),
        co: number("carbon_monoxide"),
        us_aqi: index("us_aqi"),
        european_aqi: index("european_aqi"),
        china_aqi: None,
        india_aqi: None,
    })
}

/// One WAQI GET into parsed JSON; anything else is a reason.
async fn fetch_waqi(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("WAQI: {e}"))?;
    let body = response.bytes().await.map_err(|e| format!("WAQI: {e}"))?;
    let value: Value = serde_json::from_slice(&body).map_err(|e| format!("WAQI: {e}"))?;
    if waqi_data(&value).is_none() {
        let reason = value
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or("the feed answered with an error");
        return Err(format!("WAQI: {reason}."));
    }
    Ok(value)
}

/// One Open-Meteo GET into parsed JSON.
async fn fetch_open_meteo(client: &reqwest::Client, lat: f64, lon: f64) -> Result<Aqi, String> {
    let url = format!(
        "{OPEN_METEO}?latitude={lat:.4}&longitude={lon:.4}&current=pm2_5,pm10,nitrogen_dioxide,ozone,sulphur_dioxide,carbon_monoxide,us_aqi,european_aqi"
    );
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Open-Meteo: {e}"))?;
    let body = response
        .bytes()
        .await
        .map_err(|e| format!("Open-Meteo: {e}"))?;
    let value: Value = serde_json::from_slice(&body).map_err(|e| format!("Open-Meteo: {e}"))?;
    if value.get("error").and_then(Value::as_bool) == Some(true) {
        let reason = value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("the API answered with an error");
        return Err(format!("Open-Meteo: {reason}."));
    }
    parse_open_meteo(&value).ok_or_else(|| "Open-Meteo answered without a current block.".into())
}

/// The reading at one place: with a token, WAQI's nearest ground station
/// (falling back on any failure), else Open-Meteo. Network only; the
/// caller caches.
pub async fn fetch_point(
    client: &reqwest::Client,
    lat: f64,
    lon: f64,
    token: &str,
) -> Result<Aqi, String> {
    if token.trim().is_empty() {
        return fetch_open_meteo(client, lat, lon).await;
    }
    let waqi = fetch_waqi(
        client,
        &format!("{WAQI}/feed/geo:{lat:.4};{lon:.4}/?token={token}"),
    )
    .await;
    let parsed = waqi.ok().as_ref().and_then(parse_feed);
    if let Some((station, pollutants)) = parsed {
        let mut reading = Aqi {
            v: VERSION,
            lat: station.lat,
            lon: station.lon,
            source: "waqi",
            observed_at: station.observed_at,
            ..Aqi::default()
        };
        for (name, value) in pollutants {
            match name.as_str() {
                "pm2_5" => reading.pm2_5 = Some(value),
                "pm10" => reading.pm10 = Some(value),
                "o3" => reading.o3 = Some(value),
                "no2" => reading.no2 = Some(value),
                "so2" => reading.so2 = Some(value),
                "co" => reading.co = Some(value),
                _ => {}
            }
        }
        // WAQI's index is the China scale by its own definition.
        reading.china_aqi = station.aqi;
        complete(&mut reading);
        return Ok(reading);
    }
    fetch_open_meteo(client, lat, lon).await
}

/// The stations inside a bounds rectangle (WAQI's `latlng` order:
/// north, west, south, east), deduplicated.
pub async fn fetch_stations(
    client: &reqwest::Client,
    lat0: f64,
    lon0: f64,
    lat1: f64,
    lon1: f64,
    token: &str,
) -> Result<Vec<StationAqi>, String> {
    let north = lat0.max(lat1);
    let south = lat0.min(lat1);
    let west = lon0.min(lon1);
    let east = lon0.max(lon1);
    let value = fetch_waqi(
        client,
        &format!("{WAQI}/map/bounds/?latlng={north:.4},{west:.4},{south:.4},{east:.4}&networks=all&token={token}"),
    )
    .await?;
    Ok(dedupe(parse_bounds(&value)))
}

/// One station's breakdown, as a full reading plus its pollutant map
/// (normalized IAQI keys, µg/m³, CO in µg/m³) for the card.
pub async fn fetch_detail(
    client: &reqwest::Client,
    uid: u32,
    token: &str,
) -> Result<(StationAqi, Aqi, Vec<(String, f64)>), String> {
    let value = fetch_waqi(client, &format!("{WAQI}/feed/@{uid}/?token={token}")).await?;
    let (mut station, pollutants) = parse_feed(&value)
        .ok_or_else(|| format!("WAQI answered station {uid} without a reading."))?;
    station.uid = uid;
    let mut reading = Aqi {
        v: VERSION,
        lat: station.lat,
        lon: station.lon,
        source: "waqi",
        observed_at: station.observed_at,
        ..Aqi::default()
    };
    for (name, value) in &pollutants {
        match name.as_str() {
            "pm2_5" => reading.pm2_5 = Some(*value),
            "pm10" => reading.pm10 = Some(*value),
            "o3" => reading.o3 = Some(*value),
            "no2" => reading.no2 = Some(*value),
            "so2" => reading.so2 = Some(*value),
            "co" => reading.co = Some(*value),
            _ => {}
        }
    }
    // WAQI's index is the China scale by its own definition; the reply
    // carries the full reading so the UI can label whichever scale it shows.
    reading.china_aqi = station.aqi;
    complete(&mut reading);
    Ok((station, reading, pollutants))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reading(values: &[(&str, f64)]) -> Aqi {
        let mut value = Aqi::default();
        for (name, number) in values {
            match *name {
                "pm2_5" => value.pm2_5 = Some(*number),
                "pm10" => value.pm10 = Some(*number),
                "o3" => value.o3 = Some(*number),
                "no2" => value.no2 = Some(*number),
                "so2" => value.so2 = Some(*number),
                _ => value.co = Some(*number),
            }
        }
        value
    }

    #[test]
    fn sub_index_interpolates_and_caps() {
        // China PM2.5: 35 → 50, 55 → 75, 115 → 150; past the top it clamps.
        let bands = &CHINA_PM2_5;
        assert_eq!(sub_index(0.0, bands), Some(0));
        assert_eq!(sub_index(35.0, bands), Some(50));
        assert_eq!(sub_index(55.0, bands), Some(75));
        assert_eq!(sub_index(115.0, bands), Some(150));
        assert_eq!(sub_index(600.0, bands), Some(500));
        assert_eq!(sub_index(-1.0, bands), None);
    }

    #[test]
    fn the_china_index_is_the_worst_pollutant() {
        // PM10 160 → 105 (band 150–250 → 100–150) wins over PM2.5 30 → 43.
        let value = reading(&[("pm2_5", 30.0), ("pm10", 160.0)]);
        assert_eq!(max_index(&value, true), Some(105));
        // PM2.5 alone, the clean side of good.
        let value = reading(&[("pm2_5", 10.0)]);
        assert_eq!(max_index(&value, true), Some(14));
        // Nothing measured: no index.
        assert_eq!(max_index(&Aqi::default(), true), None);
    }

    #[test]
    fn the_india_index_matches_its_table() {
        // PM2.5 45: between 30 and 60 the index runs 50 to 100.
        let value = reading(&[("pm2_5", 45.0)]);
        assert_eq!(max_index(&value, false), Some(75));
        // Between 90 and 120 the index runs 200 to 300 (poor).
        let value = reading(&[("pm2_5", 105.0)]);
        assert_eq!(max_index(&value, false), Some(250));
        // Between 250 and 380 the index runs 400 to 500 (severe).
        let value = reading(&[("pm2_5", 315.0)]);
        assert_eq!(max_index(&value, false), Some(450));
    }

    #[test]
    fn waqi_index_strings_parse() {
        assert_eq!(parse_waqi_aqi(Some(&json!("41"))), Some(41));
        assert_eq!(parse_waqi_aqi(Some(&json!(41))), Some(41));
        assert_eq!(parse_waqi_aqi(Some(&json!("-"))), None);
        assert_eq!(parse_waqi_aqi(Some(&json!(null))), None);
        assert_eq!(parse_waqi_aqi(None), None);
        assert_eq!(normalize("pm25"), "pm2_5");
    }

    #[test]
    fn open_meteo_current_parses_and_completes() {
        let value: Value = serde_json::from_str(
            r#"{"latitude":35.4,"longitude":-97.5,"current":{
                "time":"2026-09-16T12:00","pm2_5":14.2,"pm10":40.0,"ozone":120.0,
                "nitrogen_dioxide":8.0,"sulphur_dioxide":2.0,"carbon_monoxide":240.0,
                "us_aqi":56,"european_aqi":38}}"#,
        )
        .unwrap();
        let mut parsed = parse_open_meteo(&value).unwrap();
        assert_eq!(parsed.source, "open-meteo");
        assert_eq!(parsed.pm2_5, Some(14.2));
        assert_eq!(parsed.us_aqi, Some(56));
        assert_eq!(parsed.european_aqi, Some(38));
        assert!(parsed.observed_at > 0);
        complete(&mut parsed);
        // China from the concentrations: PM10 40 → 40 wins over PM2.5 14.2 → 20.
        assert_eq!(parsed.china_aqi, Some(40));
        // India: O3 120 → 129 (band 100–168 → 100–200) wins.
        assert_eq!(parsed.india_aqi, Some(129));
    }

    #[test]
    fn waqi_feed_parses_station_pollutants_and_index() {
        let value = json!({"status":"ok","data":{"aqi":"41","time":{"v":1789646400},
            "iaqi":{"pm25":{"v":30.0},"pm10":{"v":22.0},"o3":{"v":15.0}},
            "city":{"name":"Norman","geo":[35.2,-97.4]}}});
        let (station, pollutants) = parse_feed(&value).unwrap();
        assert_eq!(station.aqi, Some(41));
        assert_eq!(station.name, "Norman");
        assert_eq!(station.observed_at, 1_789_646_400);
        assert_eq!(pollutants.len(), 3);
        assert_eq!(pollutants[0], ("o3".to_owned(), 15.0));
        assert_eq!(pollutants[1], ("pm10".to_owned(), 22.0));
        assert_eq!(pollutants[2], ("pm2_5".to_owned(), 30.0));
        // An error payload is not a reading.
        assert_eq!(
            parse_feed(&json!({"status":"error","data":"invalid token"})),
            None
        );
    }

    #[test]
    fn bounds_parse_dedupe_and_decimate() {
        let value = json!({"status":"ok","data":[
            {"uid":7536,"lat":40.69,"lon":-73.92,"aqi":"45",
             "station":{"name":"Brooklyn, New York, USA","time":"2024-01-15T12:00:00-05:00"}},
            {"uid":7536,"lat":40.69,"lon":-73.92,"aqi":"46",
             "station":{"name":"Brooklyn (2)","time":"2024-01-15T12:00:00-05:00"}},
            {"uid":9001,"lat":40.70,"lon":-73.90,"aqi":"-",
             "station":{"name":"Queens","time":"2024-01-15T12:00:00-05:00"}}]});
        let stations = dedupe(parse_bounds(&value));
        assert_eq!(stations.len(), 2, "the same uid lists once");
        assert_eq!(stations[0].aqi, Some(45));
        assert_eq!(stations[1].aqi, None, "the dash is no reading");
        assert_eq!(stations[1].observed_at, 1_705_338_000);
        // Under the cap nothing changes.
        let mut many: Vec<StationAqi> = (0..50)
            .map(|i| StationAqi {
                uid: i,
                lat: 40.0 + (i % 10) as f64 * 0.01,
                lon: -73.0 + (i / 10) as f64 * 0.01,
                aqi: Some(50 + i),
                name: format!("s{i}"),
                observed_at: 0,
            })
            .collect();
        assert_eq!(decimate(many.clone(), 100).len(), 50);
        let thinned = decimate(many.clone(), 10);
        assert!(thinned.len() <= 10);
        // A cluster keeps its worst reading.
        many[7].aqi = Some(400);
        let thinned = decimate(many, 1);
        assert_eq!(thinned.len(), 1);
        assert_eq!(thinned[0].aqi, Some(400));
    }

    #[test]
    fn the_politeness_budget_resets_on_the_day_and_stops_at_the_cap() {
        let mut service = Service::new().unwrap();
        service.budget_daily = 3;
        service.budget = (day_number(), 0);
        for _ in 0..3 {
            assert!(service.bounds_may_fetch());
        }
        assert!(!service.bounds_may_fetch(), "the day's budget holds");
        service.budget = (day_number() - 1, 0);
        assert!(service.bounds_may_fetch(), "a new day spends again");
    }
}
