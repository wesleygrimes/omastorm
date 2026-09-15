//! Surface wind observations (Path C): NDBC buoys and C-MAN (including
//! lighthouses) plus METARs in the view. The engine stamps observation time
//! and names the network; it does not interpolate a field.

use crate::protocol::WindObs;
use chrono::{DateTime, TimeZone, Utc};
use std::{env, io, time::Duration};

const NDBC_LATEST: &str = "https://www.ndbc.noaa.gov/data/latest_obs/latest_obs.txt";
const METAR_URL: &str = "https://aviationweather.gov/api/data/metar";
const USER_AGENT: &str = concat!(
    "omastorm/",
    env!("CARGO_PKG_VERSION"),
    " (https://omastorm.com)"
);
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 2 << 20;
/// Stations farther than this from the view centre are dropped.
pub const RADIUS_KM: f64 = 400.0;
const MAX_OBS: usize = 80;
const EARTH_KM: f64 = 6371.0;

/// Load observations: a fixture file when `OMASTORM_WIND_OBS` is set,
/// otherwise NDBC latest_obs plus METARs around `lat`/`lon`.
pub async fn load(lat: f64, lon: f64) -> io::Result<Vec<WindObs>> {
    if let Some(path) = env::var_os("OMASTORM_WIND_OBS").filter(|p| !p.is_empty()) {
        let text = std::fs::read_to_string(&path)?;
        return Ok(nearest(parse_ndbc(&text), lat, lon, RADIUS_KM, MAX_OBS));
    }
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(io::Error::other)?;
    let mut obs = Vec::new();
    match get(&client, NDBC_LATEST).await {
        Ok(text) => obs.extend(parse_ndbc(&text)),
        Err(e) => eprintln!("wind obs NDBC: {e}"),
    }
    match get(&client, &metar_url(lat, lon)).await {
        Ok(text) => obs.extend(parse_metar_json(&text)),
        Err(e) => eprintln!("wind obs METAR: {e}"),
    }
    Ok(nearest(obs, lat, lon, RADIUS_KM, MAX_OBS))
}

async fn get(client: &reqwest::Client, url: &str) -> io::Result<String> {
    let response = client.get(url).send().await.map_err(io::Error::other)?;
    if !response.status().is_success() {
        return Err(io::Error::other(format!(
            "{url} HTTP {}",
            response.status()
        )));
    }
    let bytes = response.bytes().await.map_err(io::Error::other)?;
    if bytes.len() > MAX_BODY {
        return Err(io::Error::other(format!("{url} body too large")));
    }
    String::from_utf8(bytes.to_vec()).map_err(io::Error::other)
}

fn metar_url(lat: f64, lon: f64) -> String {
    let span = RADIUS_KM / 111.0;
    let min_lat = (lat - span).clamp(-90.0, 90.0);
    let max_lat = (lat + span).clamp(-90.0, 90.0);
    let min_lon = (lon - span).clamp(-180.0, 180.0);
    let max_lon = (lon + span).clamp(-180.0, 180.0);
    format!("{METAR_URL}?format=json&hours=1&bbox={min_lat},{min_lon},{max_lat},{max_lon}")
}

/// NDBC `latest_obs.txt`: `#` comments, then STN LAT LON YYYY MM DD hh mm WDIR WSPD GST ...
pub fn parse_ndbc(text: &str) -> Vec<WindObs> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 11 {
            continue;
        }
        let Some(lat) = parse_f64(cols[1]) else {
            continue;
        };
        let Some(lon) = parse_f64(cols[2]) else {
            continue;
        };
        let Some(year) = cols[3].parse::<i32>().ok() else {
            continue;
        };
        let Some(month) = cols[4].parse::<u32>().ok() else {
            continue;
        };
        let Some(day) = cols[5].parse::<u32>().ok() else {
            continue;
        };
        let Some(hour) = cols[6].parse::<u32>().ok() else {
            continue;
        };
        let Some(minute) = cols[7].parse::<u32>().ok() else {
            continue;
        };
        let Some(dt) = Utc
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
        else {
            continue;
        };
        let dir = parse_f64(cols[8]).map(|d| d.round() as i32);
        let speed = parse_f64(cols[9]);
        let gust = parse_f64(cols[10]);
        if speed.is_none() && gust.is_none() && dir.is_none() {
            continue;
        }
        out.push(WindObs {
            id: cols[0].to_string(),
            name: cols[0].to_string(),
            network: "NDBC".into(),
            lat,
            lon,
            speed_ms: speed,
            gust_ms: gust,
            dir_deg: dir,
            observed_at: dt.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        });
    }
    out
}

/// aviationweather.gov METAR JSON: wspd/wgst in knots.
pub fn parse_metar_json(text: &str) -> Vec<WindObs> {
    #[derive(serde::Deserialize)]
    struct Row {
        #[serde(default)]
        icao_id: String,
        #[serde(default, rename = "icaoId")]
        icao_id_camel: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        lat: f64,
        #[serde(default)]
        lon: f64,
        #[serde(default)]
        wdir: serde_json::Value,
        #[serde(default)]
        wspd: serde_json::Value,
        #[serde(default)]
        wgst: serde_json::Value,
        #[serde(default, rename = "obsTime")]
        obs_time: Option<i64>,
        #[serde(default, rename = "reportTime")]
        report_time: Option<String>,
    }
    let rows: Vec<Row> = serde_json::from_str(text).unwrap_or_default();
    rows.into_iter()
        .filter(|row| row.lat != 0.0 || row.lon != 0.0)
        .map(|row| {
            let id = if row.icao_id.is_empty() {
                row.icao_id_camel
            } else {
                row.icao_id
            };
            let observed_at = row
                .report_time
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    row.obs_time.and_then(|unix| {
                        DateTime::from_timestamp(unix, 0)
                            .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                    })
                })
                .unwrap_or_default();
            let name = if row.name.is_empty() {
                id.clone()
            } else {
                row.name
            };
            WindObs {
                id: id.clone(),
                name,
                network: "METAR".into(),
                lat: row.lat,
                lon: row.lon,
                speed_ms: json_f64(&row.wspd).map(|kt| kt * 0.514444),
                gust_ms: json_f64(&row.wgst).map(|kt| kt * 0.514444),
                dir_deg: json_f64(&row.wdir).map(|d| d.round() as i32),
                observed_at,
            }
        })
        .collect()
}

fn json_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn parse_f64(col: &str) -> Option<f64> {
    if col == "MM" {
        return None;
    }
    col.parse().ok()
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let p = std::f64::consts::PI / 180.0;
    let dlat = (lat2 - lat1) * p;
    let dlon = (lon2 - lon1) * p;
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_KM * a.sqrt().asin()
}

fn nearest(mut obs: Vec<WindObs>, lat: f64, lon: f64, radius_km: f64, cap: usize) -> Vec<WindObs> {
    obs.retain(|o| haversine_km(lat, lon, o.lat, o.lon) <= radius_km);
    obs.sort_by(|a, b| {
        haversine_km(lat, lon, a.lat, a.lon)
            .total_cmp(&haversine_km(lat, lon, b.lat, b.lon))
            .then_with(|| a.id.cmp(&b.id))
    });
    obs.truncate(cap);
    obs
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
#STN       LAT      LON  YYYY MM DD hh mm WDIR WSPD   GST WVHT
44025    40.251  -73.164 2026 09 12 13 50 220   8.2  10.1   MM
BUZM3    41.397  -71.033 2026 09 12 13 48 240   6.7   8.8   MM
KTLX     35.333  -97.278 2026 09 12 13 00  MM    MM    MM   MM
15001   -10.000  -10.000 2026 09 12 13 00 128   7.3   8.5   MM
";

    #[test]
    fn ndbc_parse_skips_missing_wind_and_comments() {
        let all = parse_ndbc(SAMPLE);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, "44025");
        assert_eq!(all[0].network, "NDBC");
        assert!((all[0].speed_ms.unwrap() - 8.2).abs() < 1e-6);
        assert_eq!(all[0].dir_deg, Some(220));
        assert_eq!(all[0].observed_at, "2026-09-12T13:50:00Z");
        let near = nearest(all, 40.7, -73.0, RADIUS_KM, MAX_OBS);
        assert_eq!(near.len(), 2);
        assert_eq!(near[0].id, "44025");
        assert_eq!(near[1].id, "BUZM3");
    }

    #[test]
    fn metar_json_converts_knots_to_metres_per_second() {
        let json = r#"[{"icaoId":"KBDR","name":"Bridgeport","lat":41.16,"lon":-73.13,"wdir":270,"wspd":12,"wgst":18,"reportTime":"2026-09-12T13:52:00Z"}]"#;
        let obs = parse_metar_json(json);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].id, "KBDR");
        assert_eq!(obs[0].network, "METAR");
        assert!((obs[0].speed_ms.unwrap() - 12.0 * 0.514444).abs() < 1e-4);
        assert_eq!(obs[0].dir_deg, Some(270));
    }

    #[test]
    fn metar_variable_wind_does_not_drop_the_list() {
        let json = r#"[{"icaoId":"KABC","lat":40.0,"lon":-74.0,"wdir":"VRB","wspd":5,"reportTime":"2026-09-12T13:52:00Z"},{"icaoId":"KDEF","lat":40.1,"lon":-74.1,"wdir":270,"wspd":8,"reportTime":"2026-09-12T13:52:00Z"}]"#;
        let obs = parse_metar_json(json);
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0].dir_deg, None);
        assert_eq!(obs[1].dir_deg, Some(270));
    }
}
