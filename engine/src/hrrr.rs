//! HRRR 10 m wind overlay (Path B): analysis (`f00`) and forecast hours
//! from NOAA's public bucket. The live path range-gets UGRD/VGRD from the
//! GRIB2 index. Tests and development can point `OMASTORM_HRRR` at a JSON
//! grid (`u`/`v` row-major, west/south/east/north in degrees).

use crate::product::{WIND_BOUNDS, WIND_PALETTE};
use crate::protocol::{WindField, WindFieldStatus};
use crate::sweep;
use chrono::{Datelike, Duration as ChronoDuration, TimeZone, Timelike, Utc};
use serde::Deserialize;
use std::{env, io, time::Duration};

const BUCKET: &str = "https://noaa-hrrr-bdp-pds.s3.amazonaws.com";
const USER_AGENT: &str = concat!(
    "omastorm/",
    env!("CARGO_PKG_VERSION"),
    " (https://omastorm.com)"
);
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_IDX: usize = 1 << 20;
const MAX_MSG: usize = 8 << 20;
const ATTRIBUTION: &str = "NOAA NCEP HRRR";
/// Output raster: 0.1° CONUS, about 10 km cells.
const WEST: f64 = -125.0;
const EAST: f64 = -66.0;
const SOUTH: f64 = 24.0;
const NORTH: f64 = 50.0;
const STEP: f64 = 0.1;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GridFile {
    #[serde(default)]
    valid_time: String,
    #[serde(default)]
    forecast_hour: u32,
    west: f64,
    south: f64,
    east: f64,
    north: f64,
    width: u32,
    height: u32,
    u: Vec<f32>,
    v: Vec<f32>,
}

/// A decoded 10 m wind field, before it is published as a texture.
pub struct Decoded {
    pub valid_time: String,
    pub forecast_hour: u32,
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
    pub width: u32,
    pub height: u32,
    pub speed: Vec<f32>,
}

impl Decoded {
    pub fn encode(&self) -> io::Result<(Vec<u8>, WindField)> {
        let pixels = raster(&self.speed, self.width, self.height);
        let png = sweep::png(self.width, self.height, &pixels)?;
        let field = WindField {
            status: WindFieldStatus::Ok,
            source: "HRRR".into(),
            valid_time: self.valid_time.clone(),
            forecast_hour: self.forecast_hour,
            units: "m/s".into(),
            texture: String::new(),
            west: self.west,
            south: self.south,
            east: self.east,
            north: self.north,
            width: self.width,
            height: self.height,
            palette: WIND_PALETTE.iter().map(|c| (*c).to_string()).collect(),
            bounds: WIND_BOUNDS.to_vec(),
            attribution: ATTRIBUTION.into(),
        };
        Ok((png, field))
    }
}

fn raster(speed: &[f32], width: u32, height: u32) -> Vec<u8> {
    let classes = WIND_PALETTE.len();
    let mut pixels = vec![0u8; speed.len() * 4];
    for (i, &s) in speed.iter().enumerate() {
        if !s.is_finite() || s < 0.0 {
            pixels[i * 4 + 3] = 255;
            continue;
        }
        let above = WIND_BOUNDS.partition_point(|&b| b as f32 <= s);
        let class = above.saturating_sub(1).min(classes - 1) as u8 + 1;
        pixels[i * 4] = class;
        pixels[i * 4 + 3] = 255;
        let _ = (width, height);
    }
    pixels
}

/// Load a field: JSON fixture when `OMASTORM_HRRR` is set, otherwise the
/// latest HRRR cycle's `hour` forecast from the public bucket.
pub async fn load(hour: u32) -> io::Result<Decoded> {
    if hour > 18 {
        return Err(io::Error::other(
            "set_wind_forecast hour must be 0 through 18",
        ));
    }
    if let Some(path) = env::var_os("OMASTORM_HRRR").filter(|p| !p.is_empty()) {
        let mut decoded = load_json(&std::fs::read_to_string(&path)?)?;
        decoded.forecast_hour = hour;
        return Ok(decoded);
    }
    load_live(hour).await
}

fn load_json(text: &str) -> io::Result<Decoded> {
    let grid: GridFile = serde_json::from_str(text).map_err(io::Error::other)?;
    if grid.u.len() != grid.v.len()
        || grid.u.len() != (grid.width as usize) * (grid.height as usize)
    {
        return Err(io::Error::other("HRRR fixture grid size mismatch"));
    }
    let speed: Vec<f32> = grid
        .u
        .iter()
        .zip(grid.v.iter())
        .map(|(u, v)| (u * u + v * v).sqrt())
        .collect();
    Ok(Decoded {
        valid_time: grid.valid_time,
        forecast_hour: grid.forecast_hour,
        west: grid.west,
        south: grid.south,
        east: grid.east,
        north: grid.north,
        width: grid.width,
        height: grid.height,
        speed,
    })
}

/// NOAA HRRR `.idx` line: `msg:byte_offset:date:name:level:fcst:`.
#[derive(Debug, PartialEq)]
pub struct IdxEntry {
    pub start: u64,
    pub name: String,
    pub level: String,
}

pub fn parse_idx(text: &str) -> Vec<IdxEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(6, ':').collect();
        if parts.len() < 5 {
            continue;
        }
        let Some(start) = parts[1].parse().ok() else {
            continue;
        };
        out.push(IdxEntry {
            start,
            name: parts[3].to_string(),
            level: parts[4].to_string(),
        });
    }
    out
}

fn field_range(idx: &[IdxEntry], name: &str, level: &str) -> Option<(u64, u64)> {
    let i = idx
        .iter()
        .position(|e| e.name == name && e.level == level)?;
    let start = idx[i].start;
    let end = idx.get(i + 1).map(|n| n.start.saturating_sub(1))?;
    Some((start, end))
}

async fn load_live(hour: u32) -> io::Result<Decoded> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(io::Error::other)?;
    let (cycle, url) = latest_cycle(&client, hour).await?;
    let idx_text = get_text(&client, &format!("{url}.idx"), MAX_IDX).await?;
    let idx = parse_idx(&idx_text);
    let Some((u0, u1)) = field_range(&idx, "UGRD", "10 m above ground") else {
        return Err(io::Error::other("HRRR index has no 10 m UGRD"));
    };
    let Some((v0, v1)) = field_range(&idx, "VGRD", "10 m above ground") else {
        return Err(io::Error::other("HRRR index has no 10 m VGRD"));
    };
    let u_bytes = get_range(&client, &url, u0, u1).await?;
    let v_bytes = get_range(&client, &url, v0, v1).await?;
    decode_grib_pair(&u_bytes, &v_bytes, hour, &cycle)
}

async fn latest_cycle(
    client: &reqwest::Client,
    hour: u32,
) -> io::Result<(chrono::DateTime<Utc>, String)> {
    let now = Utc::now();
    for back in 0..8 {
        let cycle = now - ChronoDuration::hours(back);
        let date = cycle.format("%Y%m%d");
        let t = cycle.format("%H");
        // HRRR cycles are hourly; try this UTC hour.
        let url = format!("{BUCKET}/hrrr.{date}/conus/hrrr.t{t}z.wrfsfcf{hour:02}.grib2");
        let idx = format!("{url}.idx");
        if head_ok(client, &idx).await {
            let aligned = Utc
                .with_ymd_and_hms(cycle.year(), cycle.month(), cycle.day(), cycle.hour(), 0, 0)
                .single()
                .unwrap_or(cycle);
            return Ok((aligned, url));
        }
    }
    Err(io::Error::other("no recent HRRR cycle found"))
}

async fn head_ok(client: &reqwest::Client, url: &str) -> bool {
    client
        .head(url)
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

async fn get_text(client: &reqwest::Client, url: &str, max: usize) -> io::Result<String> {
    let bytes = get_bytes(client, url, max).await?;
    String::from_utf8(bytes).map_err(io::Error::other)
}

async fn get_bytes(client: &reqwest::Client, url: &str, max: usize) -> io::Result<Vec<u8>> {
    let response = client.get(url).send().await.map_err(io::Error::other)?;
    if !response.status().is_success() {
        return Err(io::Error::other(format!(
            "{url} HTTP {}",
            response.status()
        )));
    }
    let bytes = response.bytes().await.map_err(io::Error::other)?;
    if bytes.len() > max {
        return Err(io::Error::other(format!("{url} body too large")));
    }
    Ok(bytes.to_vec())
}

async fn get_range(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64,
) -> io::Result<Vec<u8>> {
    let response = client
        .get(url)
        .header("Range", format!("bytes={start}-{end}"))
        .send()
        .await
        .map_err(io::Error::other)?;
    if !response.status().is_success() && response.status().as_u16() != 206 {
        return Err(io::Error::other(format!(
            "{url} range HTTP {}",
            response.status()
        )));
    }
    let bytes = response.bytes().await.map_err(io::Error::other)?;
    if bytes.len() > MAX_MSG {
        return Err(io::Error::other("HRRR message too large"));
    }
    Ok(bytes.to_vec())
}

/// Decode one U and one V GRIB2 message into a CONUS lat/lon speed raster.
/// Without a GRIB crate this returns unavailable-shaped error so tests use JSON.
fn decode_grib_pair(
    u_bytes: &[u8],
    v_bytes: &[u8],
    hour: u32,
    cycle: &chrono::DateTime<Utc>,
) -> io::Result<Decoded> {
    let u = grib_simple_grid(u_bytes)?;
    let v = grib_simple_grid(v_bytes)?;
    if u.ni != v.ni || u.nj != v.nj {
        return Err(io::Error::other("HRRR U/V grids differ"));
    }
    let valid = *cycle + ChronoDuration::hours(i64::from(hour));
    let width = ((EAST - WEST) / STEP).round() as u32 + 1;
    let height = ((NORTH - SOUTH) / STEP).round() as u32 + 1;
    let mut speed = vec![f32::NAN; (width * height) as usize];
    for j in 0..height {
        let lat = NORTH - f64::from(j) * STEP;
        for i in 0..width {
            let lon = WEST + f64::from(i) * STEP;
            let Some((ui, uj)) = u.latlon_to_ij(lat, lon) else {
                continue;
            };
            let idx = uj * u.ni as usize + ui;
            let Some(&uu) = u.values.get(idx) else {
                continue;
            };
            let Some(&vv) = v.values.get(idx) else {
                continue;
            };
            if uu.is_finite() && vv.is_finite() {
                speed[(j * width + i) as usize] = (uu * uu + vv * vv).sqrt();
            }
        }
    }
    Ok(Decoded {
        valid_time: valid.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        forecast_hour: hour,
        west: WEST,
        south: SOUTH,
        east: EAST,
        north: NORTH,
        width,
        height,
        speed,
    })
}

struct NativeGrid {
    ni: u32,
    nj: u32,
    values: Vec<f32>,
    proj: Projection,
}

enum Projection {
    LatLon {
        lat_first: f64,
        lon_first: f64,
        lat_last: f64,
        lon_last: f64,
    },
    Lambert {
        lat1: f64,
        lon1: f64,
        latin1: f64,
        latin2: f64,
        lov: f64,
        dx: f64,
        dy: f64,
    },
}

const EARTH_M: f64 = 6_371_229.0;

impl NativeGrid {
    fn latlon_to_ij(&self, lat: f64, lon: f64) -> Option<(usize, usize)> {
        if self.ni < 2 || self.nj < 2 {
            return None;
        }
        match self.proj {
            Projection::LatLon {
                lat_first,
                lon_first,
                lat_last,
                lon_last,
            } => {
                let fx =
                    (lon_360(lon) - lon_360(lon_first)) / (lon_360(lon_last) - lon_360(lon_first));
                let fy = (lat_first - lat) / (lat_first - lat_last);
                if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fy) {
                    return None;
                }
                let i = (fx * f64::from(self.ni - 1)).round() as usize;
                let j = (fy * f64::from(self.nj - 1)).round() as usize;
                Some((i.min(self.ni as usize - 1), j.min(self.nj as usize - 1)))
            }
            Projection::Lambert {
                lat1,
                lon1,
                latin1,
                latin2,
                lov,
                dx,
                dy,
            } => {
                let (x1, y1) = lcc_xy(lat1, lon1, latin1, latin2, lov);
                let (x, y) = lcc_xy(lat, lon, latin1, latin2, lov);
                let i = ((x - x1) / dx).round();
                let j = ((y - y1) / dy).round();
                if i < 0.0 || j < 0.0 {
                    return None;
                }
                let i = i as usize;
                let j = j as usize;
                if i >= self.ni as usize || j >= self.nj as usize {
                    return None;
                }
                Some((i, j))
            }
        }
    }
}

fn lon_360(lon: f64) -> f64 {
    if lon < 0.0 { lon + 360.0 } else { lon }
}

/// Lambert conformal (x, y) metres relative to the cone origin at LoV.
fn lcc_xy(lat_deg: f64, lon_deg: f64, latin1: f64, latin2: f64, lov: f64) -> (f64, f64) {
    let lat = lat_deg.to_radians();
    let lon = lon_360(lon_deg).to_radians();
    let l1 = latin1.to_radians();
    let l2 = latin2.to_radians();
    let lov = lon_360(lov).to_radians();
    let n = if (l1 - l2).abs() < 1e-8 {
        l1.sin()
    } else {
        (l1.cos() / l2.cos()).ln()
            / ((std::f64::consts::FRAC_PI_4 + l2 / 2.0).tan()
                / (std::f64::consts::FRAC_PI_4 + l1 / 2.0).tan())
            .ln()
    };
    let f = l1.cos() * (std::f64::consts::FRAC_PI_4 + l1 / 2.0).tan().powf(n) / n;
    let rho = EARTH_M * f / (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().powf(n);
    let theta = n * (lon - lov);
    // y increases north: equivalent to rho0 - rho*cos(theta) up to a constant.
    (rho * theta.sin(), -rho * theta.cos())
}

/// Minimal GRIB2 simple-packing reader for one 2-D field. HRRR 10 m U/V
/// uses template 5.0 (simple packing) and 3.0/3.30 grids. This is not a
/// general GRIB library: unknown templates fail so a fixture can stand in.
fn grib_simple_grid(bytes: &[u8]) -> io::Result<NativeGrid> {
    if bytes.len() < 16 || &bytes[0..4] != b"GRIB" {
        return Err(io::Error::other("not a GRIB2 message"));
    }
    if bytes[7] != 2 {
        return Err(io::Error::other("GRIB edition 2 required"));
    }
    let mut sections = parse_sections(bytes)?;
    let grid = sections
        .remove(&3)
        .ok_or_else(|| io::Error::other("GRIB2 missing grid section"))?;
    let datarep = sections
        .remove(&5)
        .ok_or_else(|| io::Error::other("GRIB2 missing data-rep section"))?;
    let packed = sections
        .remove(&7)
        .ok_or_else(|| io::Error::other("GRIB2 missing data section"))?;
    let (ni, nj, proj) = grid_proj(&grid)?;
    let values = unpack_simple(&datarep, &packed, (ni * nj) as usize)?;
    Ok(NativeGrid {
        ni,
        nj,
        values,
        proj,
    })
}

fn parse_sections(bytes: &[u8]) -> io::Result<std::collections::HashMap<u8, Vec<u8>>> {
    let mut map = std::collections::HashMap::new();
    let mut i = 16; // after indicator
    while i + 5 <= bytes.len() {
        if &bytes[i..bytes.len().min(i + 4)] == b"7777" {
            break;
        }
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        if len < 5 || i + len > bytes.len() {
            return Err(io::Error::other("GRIB2 section length"));
        }
        let num = bytes[i + 4];
        map.insert(num, bytes[i + 5..i + len].to_vec());
        i += len;
    }
    Ok(map)
}

/// Section 3 body (after length + section number). Template 0 is regular
/// lat/lon; template 30 is Lambert, which is what HRRR CONUS uses.
fn grid_proj(section3: &[u8]) -> io::Result<(u32, u32, Projection)> {
    if section3.len() < 56 {
        return Err(io::Error::other("GRIB2 grid section too short"));
    }
    let template = u16::from_be_bytes([section3[7], section3[8]]);
    let micro =
        |at: usize| i32::from_be_bytes(section3[at..at + 4].try_into().unwrap()) as f64 * 1e-6;
    let (ni, nj, proj) = match template {
        0 => {
            let ni = u32::from_be_bytes(section3[25..29].try_into().unwrap());
            let nj = u32::from_be_bytes(section3[29..33].try_into().unwrap());
            let lat1 = micro(33);
            let lon1 = micro(37);
            let lat2 = micro(42);
            let lon2 = micro(46);
            (
                ni,
                nj,
                Projection::LatLon {
                    lat_first: lat1,
                    lon_first: lon1,
                    lat_last: lat2,
                    lon_last: lon2,
                },
            )
        }
        30 => {
            let ni = u32::from_be_bytes(section3[25..29].try_into().unwrap());
            let nj = u32::from_be_bytes(section3[29..33].try_into().unwrap());
            let lat1 = micro(33);
            let lon1 = micro(37);
            let lov = micro(46);
            let dx = f64::from(u32::from_be_bytes(section3[50..54].try_into().unwrap())) / 1000.0;
            let dy = f64::from(u32::from_be_bytes(section3[54..58].try_into().unwrap())) / 1000.0;
            let latin1 = micro(60);
            let latin2 = micro(64);
            (
                ni,
                nj,
                Projection::Lambert {
                    lat1,
                    lon1,
                    latin1,
                    latin2,
                    lov,
                    dx,
                    dy,
                },
            )
        }
        other => {
            return Err(io::Error::other(format!(
                "GRIB2 grid template {other} not supported"
            )));
        }
    };
    if ni == 0 || nj == 0 {
        return Err(io::Error::other("GRIB2 empty grid"));
    }
    Ok((ni, nj, proj))
}

/// GRIB2 signed integers are sign-magnitude, not two's complement.
fn grib_i16(bytes: [u8; 2]) -> i32 {
    let u = u16::from_be_bytes(bytes);
    let mag = i32::from(u & 0x7fff);
    if u & 0x8000 != 0 { -mag } else { mag }
}

fn unpack_simple(section5: &[u8], packed: &[u8], n: usize) -> io::Result<Vec<f32>> {
    if section5.len() < 16 {
        return Err(io::Error::other("GRIB2 data-rep too short"));
    }
    let template = u16::from_be_bytes([section5[4], section5[5]]);
    if template != 0 {
        return Err(io::Error::other(format!(
            "GRIB2 data-rep template {template} not simple packing"
        )));
    }
    let reference = f32::from_be_bytes(section5[6..10].try_into().unwrap());
    let binary_scale = grib_i16([section5[10], section5[11]]);
    let decimal_scale = grib_i16([section5[12], section5[13]]);
    let bits = section5[14] as usize;
    if bits == 0 || bits > 32 {
        return Err(io::Error::other("GRIB2 bit depth"));
    }
    let mut values = Vec::with_capacity(n);
    let scale_b = 2f32.powi(binary_scale);
    let scale_d = 10f32.powi(decimal_scale);
    for i in 0..n {
        let raw = read_bits(packed, i * bits, bits)?;
        values.push((reference + raw as f32 * scale_b) / scale_d);
    }
    Ok(values)
}

fn read_bits(data: &[u8], bit: usize, width: usize) -> io::Result<u32> {
    let mut v = 0u32;
    for k in 0..width {
        let p = bit + k;
        let byte = data
            .get(p / 8)
            .ok_or_else(|| io::Error::other("GRIB2 packed overflow"))?;
        let on = (byte >> (7 - (p % 8))) & 1;
        v = (v << 1) | u32::from(on);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_grid_becomes_speed_hypot() {
        let json = r#"{"validTime":"2026-09-12T12:00:00Z","forecastHour":0,"west":-74.0,"south":40.0,"east":-73.0,"north":41.0,"width":2,"height":2,"u":[3,0,0,-3],"v":[4,0,0,4]}"#;
        let decoded = load_json(json).unwrap();
        assert_eq!(decoded.width, 2);
        assert!((decoded.speed[0] - 5.0).abs() < 1e-5);
        assert_eq!(decoded.speed[1], 0.0);
        let (png, field) = decoded.encode().unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert_eq!(field.status, WindFieldStatus::Ok);
        assert_eq!(field.forecast_hour, 0);
    }

    #[test]
    fn hrrr_sw_corner_is_grid_origin() {
        let lat1 = 21.138123;
        let lon1 = 237.280472;
        let grid = NativeGrid {
            ni: 1799,
            nj: 1059,
            values: vec![0.0; 2],
            proj: Projection::Lambert {
                lat1,
                lon1,
                latin1: 38.5,
                latin2: 38.5,
                lov: 262.5,
                dx: 3000.0,
                dy: 3000.0,
            },
        };
        assert_eq!(grid.latlon_to_ij(lat1, lon1), Some((0, 0)));
        let nyc = grid.latlon_to_ij(40.7, -74.0);
        assert!(nyc.is_some(), "NYC should sit on the HRRR CONUS grid");
        let (i, j) = nyc.unwrap();
        assert!(i > 0 && j > 0 && i < 1798 && j < 1058, "i={i} j={j}");
    }

    #[test]
    fn idx_finds_10m_wind_ranges() {
        let idx = "\
77:43379677:d=2026091218:UGRD:10 m above ground:anl:\n\
78:45523149:d=2026091218:VGRD:10 m above ground:anl:\n\
79:47666621:d=2026091218:WIND:10 m above ground:0-0 day max fcst:
";
        let entries = parse_idx(idx);
        assert_eq!(
            field_range(&entries, "UGRD", "10 m above ground"),
            Some((43379677, 45523148))
        );
        assert_eq!(
            field_range(&entries, "VGRD", "10 m above ground"),
            Some((45523149, 47666620))
        );
    }
}
