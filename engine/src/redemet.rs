//! Brazilian radar data provider via DECEA / REDEMET MAXCAPPI API.
//!
//! Decodes georeferenced raster MAXCAPPI images from Brazil's 29 operational
//! Doppler radar stations and samples them into standard polar `Sweep` radials
//! and gates, fully adhering to `docs/protocol.md` and `DESIGN.md`.

use crate::live::Event;
use crate::sweep::{Ray, Sweep};
use chrono::{NaiveDateTime, SecondsFormat, Utc};
use serde::Deserialize;
use std::env;
use std::io::Cursor;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::time::{sleep, timeout};

pub const DEFAULT_API_KEY: &str = "ouyaq0gZ4pEyTFIz86fJyby2snpspM66yU728dB2";
const API_URL: &str = "https://api-redemet.decea.mil.br/produtos/radar/maxcappi";
const BUCKET_NAME: &str = "redemet-radar";
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

const RAYS_COUNT: usize = 720; // 0.5 deg resolution
const GATES_COUNT: u16 = 800; // 500 m spacing -> 400 km
const GATE_SPACING_M: u32 = 500;
const FIRST_GATE_M: u32 = 0;
const MOMENT_SCALE: f32 = 2.0;
const MOMENT_OFFSET: f32 = 66.0;
const EARTH_RADIUS_M: f64 = 6_371_000.0;

fn log(site: &str, msg: impl std::fmt::Display) {
    eprintln!(
        "{} Live {site}: {msg}",
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    );
}

#[derive(Deserialize, Debug)]
pub struct ApiResponse {
    pub data: Option<ApiData>,
}

#[derive(Deserialize, Debug)]
pub struct ApiData {
    pub radar: Option<Vec<Vec<RadarSiteEntry>>>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct RadarSiteEntry {
    pub localidade: String,
    pub nome: Option<String>,
    pub raio: Option<f64>,
    pub lat_center: Option<String>,
    pub lon_center: Option<String>,
    pub lat_min: Option<String>,
    pub lat_max: Option<String>,
    pub lon_min: Option<String>,
    pub lon_max: Option<String>,
    pub path: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct BBox {
    pub lat_min: f64,
    pub lat_max: f64,
    pub lon_min: f64,
    pub lon_max: f64,
}

/// Map REDEMET RGB color palette to reflectivity in dBZ.
/// REDEMET palette ranges:
/// - Cyan to Blue (0 - 20 dBZ): light rain
/// - Green to Dark Green (20 - 30 dBZ): moderate rain
/// - Yellow to Orange (30 - 45 dBZ): heavy rain
/// - Red to Dark Red (45 - 63 dBZ): intense storm
/// - Magenta to Purple (63 - 75 dBZ): severe storm / hail
pub fn rgb_to_dbz(r: u8, g: u8, b: u8) -> f32 {
    if r > 200 && b > 200 {
        65.0
    } else if r > 180 && g < 100 {
        50.0
    } else if r > 200 && g > 150 && b < 100 {
        38.0
    } else if g > 120 && r < 120 && b < 100 {
        25.0
    } else if b > 150 || (g > 150 && b > 150) {
        12.0
    } else {
        8.0
    }
}

pub fn rgb_to_code(r: u8, g: u8, b: u8, a: u8) -> u8 {
    if a == 0 {
        0 // Below threshold / transparent
    } else {
        let dbz = rgb_to_dbz(r, g, b);
        ((dbz * MOMENT_SCALE + MOMENT_OFFSET).round() as u32).clamp(2, 255) as u8
    }
}

pub fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("PNG read_info: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("PNG next_frame: {e}"))?;
    buf.truncate(info.buffer_size());
    if info.color_type == png::ColorType::Rgba {
        Ok((buf, info.width, info.height))
    } else {
        Err(format!("Unsupported PNG color type: {:?}", info.color_type))
    }
}

/// Sample a georeferenced MAXCAPPI PNG image into a standard polar `Sweep`.
pub fn raster_to_sweep(
    image_rgba: &[u8],
    width: u32,
    height: u32,
    lat_center: f64,
    lon_center: f64,
    bbox: BBox,
    time_ms: i64,
) -> Result<Sweep, String> {
    let lat0_rad = lat_center.to_radians();
    let lon0_rad = lon_center.to_radians();

    // Precalculate sin and cos of angular distance for each gate
    let mut sin_dist = Vec::with_capacity(GATES_COUNT as usize);
    let mut cos_dist = Vec::with_capacity(GATES_COUNT as usize);
    for g in 0..GATES_COUNT {
        let dist_m = g as f64 * GATE_SPACING_M as f64;
        let delta = dist_m / EARTH_RADIUS_M;
        sin_dist.push(delta.sin());
        cos_dist.push(delta.cos());
    }

    let mut rays = Vec::with_capacity(RAYS_COUNT);
    let stride = width as usize * 4;

    for r in 0..RAYS_COUNT {
        let az_deg = r as f32 * 0.5;
        let az_rad = (az_deg as f64).to_radians();
        let sin_az = az_rad.sin();
        let cos_az = az_rad.cos();

        let mut codes = Vec::with_capacity(GATES_COUNT as usize);

        for g in 0..GATES_COUNT as usize {
            let sin_d = sin_dist[g];
            let cos_d = cos_dist[g];

            let sin_lat = lat0_rad.sin() * cos_d + lat0_rad.cos() * sin_d * cos_az;
            let lat = sin_lat.asin();
            let d_lon = (sin_az * sin_d * lat0_rad.cos()).atan2(cos_d - lat0_rad.sin() * sin_lat);
            let lon = lon0_rad + d_lon;

            let lat_deg = lat.to_degrees();
            let lon_deg = lon.to_degrees();

            if lat_deg >= bbox.lat_min
                && lat_deg <= bbox.lat_max
                && lon_deg >= bbox.lon_min
                && lon_deg <= bbox.lon_max
            {
                let px = ((lon_deg - bbox.lon_min) / (bbox.lon_max - bbox.lon_min)
                    * (width as f64 - 1.0))
                    .round() as usize;
                let py = ((bbox.lat_max - lat_deg) / (bbox.lat_max - bbox.lat_min)
                    * (height as f64 - 1.0))
                    .round() as usize;

                let px = px.min(width as usize - 1);
                let py = py.min(height as usize - 1);

                let offset = py * stride + px * 4;
                if offset + 3 < image_rgba.len() {
                    let r = image_rgba[offset];
                    let g = image_rgba[offset + 1];
                    let b = image_rgba[offset + 2];
                    let a = image_rgba[offset + 3];
                    codes.push(rgb_to_code(r, g, b, a));
                } else {
                    codes.push(0);
                }
            } else {
                codes.push(0);
            }
        }

        rays.push(Ray {
            azimuth_deg: az_deg,
            elevation_deg: 0.5,
            time_ms,
            codes,
        });
    }

    Ok(Sweep {
        rays,
        start_ms: time_ms,
        end_ms: time_ms + 60_000,
        gates: GATES_COUNT,
        first_gate_m: FIRST_GATE_M,
        gate_spacing_m: GATE_SPACING_M,
        scale: MOMENT_SCALE,
        offset: MOMENT_OFFSET,
    })
}

pub fn parse_timestamp(s: &str) -> Option<i64> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|dt| dt.and_utc().timestamp_millis())
}

/// Live polling loop for Brazilian radar stations.
pub async fn poll(site: String, events: Sender<Event>, cached: Vec<i64>, skip_known: bool) {
    let localidade = site.strip_prefix("SB").unwrap_or(&site).to_lowercase();

    let api_key = env::var("REDEMET_API_KEY").unwrap_or_else(|_| DEFAULT_API_KEY.to_string());
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .unwrap_or_default();

    let mut known = cached;
    let mut skip_known = skip_known;

    log(&site, "polling REDEMET MAXCAPPI feed");

    loop {
        let url = format!("{API_URL}?api_key={api_key}&anima=8");
        let fetch_res = timeout(HTTP_TIMEOUT, client.get(&url).send()).await;

        let response = match fetch_res {
            Ok(Ok(res)) => res,
            Ok(Err(e)) => {
                let reason = format!("HTTP request failed: {e}");
                log(&site, &reason);
                let _ = events
                    .send(Event::Offline {
                        site: site.clone(),
                        reason,
                    })
                    .await;
                sleep(POLL_INTERVAL).await;
                continue;
            }
            Err(_) => {
                let reason = "HTTP request timed out".to_string();
                log(&site, &reason);
                let _ = events
                    .send(Event::Offline {
                        site: site.clone(),
                        reason,
                    })
                    .await;
                sleep(POLL_INTERVAL).await;
                continue;
            }
        };

        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                let reason = format!("Reading response body failed: {e}");
                log(&site, &reason);
                let _ = events
                    .send(Event::Offline {
                        site: site.clone(),
                        reason,
                    })
                    .await;
                sleep(POLL_INTERVAL).await;
                continue;
            }
        };

        let parsed: ApiResponse = match serde_json::from_slice(&body_bytes) {
            Ok(p) => p,
            Err(e) => {
                let reason = format!("Parsing JSON failed: {e}");
                log(&site, &reason);
                let _ = events
                    .send(Event::Silent {
                        site: site.clone(),
                        reason,
                    })
                    .await;
                sleep(POLL_INTERVAL).await;
                continue;
            }
        };

        let Some(data) = parsed.data else {
            let reason = "Empty data in REDEMET response".to_string();
            let _ = events
                .send(Event::Silent {
                    site: site.clone(),
                    reason,
                })
                .await;
            sleep(POLL_INTERVAL).await;
            continue;
        };

        let Some(radar_frames) = data.radar else {
            let reason = "No radar frames in REDEMET response".to_string();
            let _ = events
                .send(Event::Silent {
                    site: site.clone(),
                    reason,
                })
                .await;
            sleep(POLL_INTERVAL).await;
            continue;
        };

        // Collect matching entries for this station across all animation frames
        let mut entries: Vec<RadarSiteEntry> = Vec::new();
        for frame_group in radar_frames {
            for entry in frame_group {
                if entry.localidade.eq_ignore_ascii_case(&localidade) && entry.path.is_some() {
                    entries.push(entry);
                }
            }
        }

        if entries.is_empty() {
            let reason = format!("Station {localidade} not found in REDEMET radar list");
            let _ = events
                .send(Event::Silent {
                    site: site.clone(),
                    reason,
                })
                .await;
            sleep(POLL_INTERVAL).await;
            continue;
        }

        // Deduplicate and sort entries chronologically
        entries.sort_by_key(|e| e.data.clone().unwrap_or_default());
        entries.dedup_by(|a, b| a.data == b.data);

        // Process frames: earlier frames as Backfill, newest frame as Sweep
        let total = entries.len();
        for (i, entry) in entries.iter().enumerate() {
            let Some(date_str) = entry.data.as_deref() else {
                continue;
            };
            let Some(time_ms) = parse_timestamp(date_str) else {
                continue;
            };

            if known.contains(&time_ms) {
                if skip_known && i + 1 == total {
                    // Skip catalogued replay if requested
                    continue;
                }
                if i + 1 < total {
                    continue;
                }
            }

            let Some(img_url) = entry.path.as_deref() else {
                continue;
            };
            let img_bytes = match client.get(img_url).send().await {
                Ok(res) => match res.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        log(&site, format_args!("image download failed: {e}"));
                        continue;
                    }
                },
                Err(e) => {
                    log(&site, format_args!("image request failed: {e}"));
                    continue;
                }
            };

            let (rgba, width, height) = match decode_png(&img_bytes) {
                Ok(img) => img,
                Err(e) => {
                    log(&site, format_args!("PNG decode failed: {e}"));
                    continue;
                }
            };

            let lat_center = entry
                .lat_center
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let lon_center = entry
                .lon_center
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let lat_min = entry
                .lat_min
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(lat_center - 3.5);
            let lat_max = entry
                .lat_max
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(lat_center + 3.5);
            let lon_min = entry
                .lon_min
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(lon_center - 3.5);
            let lon_max = entry
                .lon_max
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(lon_center + 3.5);

            let bbox = BBox {
                lat_min,
                lat_max,
                lon_min,
                lon_max,
            };
            let sweep = match raster_to_sweep(
                &rgba, width, height, lat_center, lon_center, bbox, time_ms,
            ) {
                Ok(s) => s,
                Err(e) => {
                    log(&site, format_args!("raster_to_sweep failed: {e}"));
                    continue;
                }
            };

            let provenance = format!("{BUCKET_NAME}/{site}/{date_str}");
            if i + 1 < total {
                let _ = events
                    .send(Event::Backfill {
                        site: site.clone(),
                        sweep,
                        provenance,
                    })
                    .await;
            } else {
                let _ = events
                    .send(Event::Sweep {
                        site: site.clone(),
                        sweep,
                        complete: true,
                        provenance,
                    })
                    .await;
            }

            if !known.contains(&time_ms) {
                known.push(time_ms);
            }
        }

        skip_known = true;
        sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_mapping_produces_consistent_dbz() {
        assert_eq!(rgb_to_dbz(255, 0, 255), 65.0); // Magenta
        assert_eq!(rgb_to_dbz(255, 0, 0), 50.0); // Red
        assert_eq!(rgb_to_dbz(255, 200, 0), 38.0); // Orange/Yellow
        assert_eq!(rgb_to_dbz(0, 200, 0), 25.0); // Green
        assert_eq!(rgb_to_dbz(0, 0, 255), 12.0); // Blue
    }

    #[test]
    fn timestamp_parser_handles_standard_format() {
        let ts = parse_timestamp("2026-09-14 02:00:00").unwrap();
        assert!(ts > 1_700_000_000_000);
    }

    #[test]
    fn raster_sampling_generates_valid_sweep() {
        let width = 10;
        let height = 10;
        let rgba = vec![255; (width * height * 4) as usize];
        let bbox = BBox {
            lat_min: -24.0,
            lat_max: -23.0,
            lon_min: -48.0,
            lon_max: -47.0,
        };
        let sweep = raster_to_sweep(&rgba, width, height, -23.5, -47.5, bbox, 1_000_000).unwrap();
        assert_eq!(sweep.rays.len(), 720);
        assert_eq!(sweep.gates, 800);
        assert_eq!(sweep.first_gate_m, 0);
        assert_eq!(sweep.gate_spacing_m, 500);
    }

    #[test]
    fn rgb_to_code_handles_transparency_and_measured_values() {
        assert_eq!(rgb_to_code(255, 0, 0, 0), 0);
        let code = rgb_to_code(255, 0, 0, 255);
        assert!(code >= 2);
        // Verify dBZ calculation: (50.0 * 2.0 + 66.0) = 166
        assert_eq!(code, 166);
    }

    #[test]
    fn brazilian_sites_in_site_table_have_valid_envelope() {
        let table: crate::protocol::SiteTable =
            serde_json::from_str(include_str!("../data/sites.json")).unwrap();
        let brazilian_sites: Vec<_> = table
            .sites
            .iter()
            .filter(|s| s.id.starts_with("SB"))
            .collect();
        assert_eq!(brazilian_sites.len(), 29);
        for s in brazilian_sites {
            assert!(
                s.lat >= -35.0 && s.lat <= 6.0,
                "Site {} lat {} out of range",
                s.id,
                s.lat
            );
            assert!(
                s.lon >= -75.0 && s.lon <= -30.0,
                "Site {} lon {} out of range",
                s.id,
                s.lon
            );
            assert!(!s.name.is_empty());
            assert!(!s.state.is_empty());
        }
    }
}
