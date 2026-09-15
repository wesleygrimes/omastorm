//! Hong Kong Observatory rainfall-rate imagery: the 256 km product the
//! Observatory renders and publishes every six minutes
//! (`docs/radar-fetch.md`, rendered products).
//!
//! The file is a picture, not a volume. The Observatory draws its own map,
//! terrain, legend and colour ramp into it, and the values behind that ramp
//! never leave the Observatory, so the engine publishes the image exactly as
//! it arrived. It adds only what the Observatory states about the picture: the
//! ground box from its own KML ground overlay and its own band edges, so the
//! map shows what the product shows and claims nothing more. Nothing here
//! decodes or resamples the pixels.

use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::Sender;
use tokio::time::{sleep, timeout};

use crate::live::Event;
use crate::protocol::{Crop, Overlay};

/// The feed's display name, shown beside the product (`frame.source`).
pub const NETWORK: &str = "HKO 256 KM";
/// The product's code; the Observatory publishes one product per range.
pub const PRODUCT: &str = "RATE";
/// The picture is the rainfall rate at 3 km, not a tilt, so the name says so.
pub const PRODUCT_NAME: &str = "Rain rate (3 km)";
pub const UNITS: &str = "mm/h";

const INDEX: &str = "https://www.hko.gov.hk/wxinfo/radars/temp_json/nradar_img.json";
const FRAMES: &str = "https://www.hko.gov.hk/wxinfo/radars/";
const USER_AGENT: &str = "omastorm-engine";
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// The product refreshes every six minutes; a minute is enough to notice.
const POLL: Duration = Duration::from_secs(60);
const BACK_OFF: Duration = Duration::from_secs(15);
const MAX_BACK_OFF: Duration = Duration::from_secs(300);
/// Frames fetched once on joining to give the timeline a loop behind the
/// newest one. The Observatory publishes the last two hours.
const BACKFILL: usize = 12;

/// The Observatory's band edges in mm/h, lowest first. Its legend prints
/// fifteen ranges of two values and one of `>300`, so the last entry only
/// closes the ramp's last colour.
pub const LEVELS: [f64; 17] = [
    0.15, 0.5, 1.0, 2.0, 3.0, 5.0, 7.0, 10.0, 15.0, 30.0, 50.0, 75.0, 100.0, 150.0, 200.0, 300.0,
    3000.0,
];

/// The colours the Observatory paints those bands with, lowest first, read
/// from the ramp the product itself carries.
pub const PALETTE: [&str; 16] = [
    "#00c9fc", "#0090f4", "#3c96ff", "#017f49", "#01993c", "#02bd27", "#00d001", "#00fd07",
    "#98ff01", "#e0d000", "#ffd401", "#efb101", "#f08301", "#ea350a", "#ec0003", "#c5000b",
];

/// Where the product's map sits on the ground, and in its file.
///
/// The box is the Observatory's own statement: `radar_256_kml/Radar_256.kml`
/// carries a ground overlay with north 24.60560, south 20.00107, west
/// 111.68321, east 116.66013 (the file has the last two swapped). The crop is
/// the map area of the published file; the columns beside it carry the
/// Observatory's legend panel. The map area's 398 × 395 pixels against that
/// box are square on the ground (1.288 km per pixel both ways), which is what
/// fixes the two numbers.
pub fn overlay() -> Overlay {
    Overlay {
        north: 24.60560,
        south: 20.00107,
        east: 116.66013,
        west: 111.68321,
        crop: Crop {
            x: 0,
            y: 0,
            width: 398,
            height: 395,
        },
        levels: LEVELS.to_vec(),
    }
}

/// One published frame: its path under [`FRAMES`], and its collection time in
/// milliseconds since the Unix epoch.
pub struct Product {
    pub path: String,
    pub start_ms: i64,
}

/// The 256 km product's frames, newest first, from the Observatory's frame
/// index. Its entries are JavaScript assignments rather than bare names.
pub fn products(index: &[u8]) -> Result<Vec<Product>, String> {
    let root: Value =
        serde_json::from_slice(index).map_err(|e| format!("parsing the frame index: {e}"))?;
    let images = root
        .get("radar")
        .and_then(|radar| radar.get("range0"))
        .and_then(|range| range.get("image"))
        .and_then(Value::as_array)
        .ok_or("the frame index carries no 256 km images")?;
    let mut out: Vec<Product> = images
        .iter()
        .filter_map(Value::as_str)
        .filter_map(quoted_path)
        .filter_map(|path| scan_ms(&path).map(|start_ms| Product { path, start_ms }))
        .collect();
    out.sort_by_key(|product| std::cmp::Reverse(product.start_ms));
    out.dedup_by_key(|product| product.start_ms);
    Ok(out)
}

/// The path inside an index entry such as
/// `picture[0][19]="rad_256_png/2d256nradar_202609151430.jpg";`.
fn quoted_path(entry: &str) -> Option<String> {
    let start = entry.find('"')? + 1;
    let end = entry[start..].find('"')?;
    Some(entry[start..start + end].to_owned())
}

/// The collection time in `2d256nradar_202609151430.jpg` as milliseconds since
/// the Unix epoch. The Observatory stamps Hong Kong time (UTC+8), which is
/// what its own page prints beside the same image.
fn scan_ms(path: &str) -> Option<i64> {
    let name = path.rsplit('/').next()?;
    let digits: String = name.chars().filter(char::is_ascii_digit).collect();
    let stamp = digits.get(digits.len().checked_sub(12)?..)?;
    let naive = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M").ok()?;
    Some((naive.and_utc() - chrono::Duration::hours(8)).timestamp_millis())
}

/// Poll the Observatory's index until the task is aborted or the event
/// channel closes, publishing each new frame. `cached` holds the start times
/// already catalogued for the station, so a restart neither refetches them nor
/// republishes them; the timeline keeps them.
pub async fn poll(site: String, events: Sender<Event>, cached: Vec<i64>, _skip_known: bool) {
    let client = match reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(CALL_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            offline(&events, &site, format!("building the HTTP client: {e}")).await;
            return;
        }
    };
    let mut known = cached;
    let mut back_off = BACK_OFF;
    let mut backfilled = false;
    loop {
        let index = match fetch(&client, INDEX).await {
            Ok(bytes) => bytes,
            Err(e) => {
                if !offline(&events, &site, e).await {
                    return;
                }
                sleep(back_off).await;
                back_off = (back_off * 2).min(MAX_BACK_OFF);
                continue;
            }
        };
        let list = match products(&index) {
            Ok(list) => list,
            Err(e) => {
                if !offline(&events, &site, e).await {
                    return;
                }
                sleep(back_off).await;
                back_off = (back_off * 2).min(MAX_BACK_OFF);
                continue;
            }
        };
        back_off = BACK_OFF;
        let Some(newest) = list.first() else {
            if events
                .send(Event::Silent {
                    site: site.clone(),
                    reason: "the frame index lists no images".into(),
                })
                .await
                .is_err()
            {
                return;
            }
            sleep(POLL).await;
            continue;
        };
        let mut pending: Vec<(i64, String, bool)> = Vec::new();
        if !known.contains(&newest.start_ms) {
            pending.push((newest.start_ms, newest.path.clone(), true));
        }
        if !backfilled {
            pending.extend(
                list.iter()
                    .skip(1)
                    .filter(|product| !known.contains(&product.start_ms))
                    .take(BACKFILL)
                    .map(|product| (product.start_ms, product.path.clone(), false)),
            );
            backfilled = true;
        }
        for (start_ms, path, show) in pending {
            let url = format!("{FRAMES}{path}");
            match fetch(&client, &url).await {
                Ok(image) => {
                    known.push(start_ms);
                    let provenance = format!("{NETWORK} {url}");
                    eprintln!(
                        "{} Live {site}: {} {} ({:.0} KiB)",
                        iso(start_ms),
                        if show { "published" } else { "backfilled" },
                        path,
                        image.len() as f64 / 1024.0
                    );
                    if events
                        .send(Event::Overlay {
                            site: site.clone(),
                            start_ms,
                            image,
                            show,
                            provenance,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(e) => eprintln!("{} Live {site}: fetching {path}: {e}", iso(start_ms)),
            }
        }
        sleep(POLL).await;
    }
}

/// One HTTPS GET with the call timeout applied by the client.
async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = timeout(CALL_TIMEOUT, client.get(url).send())
        .await
        .map_err(|_| "the request timed out".to_owned())
        .and_then(|r| r.map_err(|e| e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the server answered {status}"));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| e.to_string())
}

/// Report an unreachable feed; false once the channel is closed.
async fn offline(events: &Sender<Event>, site: &str, reason: String) -> bool {
    events
        .send(Event::Offline {
            site: site.to_owned(),
            reason,
        })
        .await
        .is_ok()
}

/// Milliseconds since the Unix epoch as a UTC timestamp, for the log.
fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| ms.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_index_and_converts_hong_kong_time() {
        let index = br#"{"radar":{"range0":{"image":[
            "picture[0][0]=\"rad_256_png/2d256nradar_202609151236.jpg\";",
            "picture[0][1]=\"rad_256_png/2d256nradar_202609151430.jpg\";",
            "picture[0][2]=\"rad_256_png/2d256nradar_202609151430.jpg\";",
            "picture[0][3]=\"not a frame\";"
        ]}}}"#;
        let products = products(index).unwrap();
        assert_eq!(products.len(), 2, "duplicates and junk entries drop out");
        assert_eq!(products[0].path, "rad_256_png/2d256nradar_202609151430.jpg");
        assert_eq!(products[1].path, "rad_256_png/2d256nradar_202609151236.jpg");
        // 14:30 HKT is 06:30 UTC.
        assert_eq!(products[0].start_ms, 1789453800000);
    }

    #[test]
    fn an_index_without_the_range_is_an_error() {
        assert!(products(br#"{"radar":{}}"#).is_err());
    }

    #[test]
    fn the_overlay_is_closed_and_ordered() {
        let overlay = overlay();
        assert!(overlay.north > overlay.south && overlay.east > overlay.west);
        assert_eq!(
            overlay.levels.len(),
            PALETTE.len() + 1,
            "every colour has a band edge"
        );
        assert!(overlay.levels.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(overlay.crop.width, 398);
        assert_eq!(overlay.crop.height, 395);
    }
}
