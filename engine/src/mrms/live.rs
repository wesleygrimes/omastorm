//! Explicitly selected MRMS feed with a bounded hour of encoded history. All discovery and large work
//! starts in `poll`, never in construction or metadata lookup.

use super::{BODY_MAX, BoundedBytes, ID, decode_frame};
use crate::{
    envelope::MRMS_CONUS,
    live_index,
    protocol::{Coverage, Crs, Family, FrameStatus, Kind, MosaicFrame, ProductClass},
    source::{GridEvent, GridSender, HistoryPolicy, MosaicMeta, SourceMetadataBorrowed},
    sweep,
};
use chrono::{NaiveDate, NaiveDateTime, Utc};
use std::{
    collections::{BTreeSet, HashSet},
    future::Future,
    io::Write,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::Semaphore,
    task::{JoinHandle, spawn_blocking},
    time::{Instant, sleep_until, timeout},
};
use xml::{ParserConfig, reader::XmlEvent};

const HOST: &str = "https://noaa-mrms-pds.s3.amazonaws.com/";
const PRODUCT: &str = "MergedBaseReflectivityQC_00.50";
const LISTING_MAX: usize = 2 << 20;
const PAGES_MAX: usize = 4;
const FALLBACK_MAX: usize = 3;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(30);
pub const HISTORY: HistoryPolicy = HistoryPolicy {
    max_frames: 30,
    window_ms: Some(60 * 60 * 1000),
};

pub struct Mrms {
    pub id: &'static str,
    coverage: Coverage,
    palette: Arc<Vec<String>>,
    bounds: Arc<Vec<f64>>,
    // Lives on the registry adapter, not the poll task: a cancelled session's
    // blocking decode retains its permit until it actually exits.
    work: Arc<Semaphore>,
}

impl Mrms {
    pub fn new() -> Self {
        let template: crate::protocol::Frame =
            serde_json::from_str(include_str!("../../data/product.json")).unwrap();
        Self {
            id: ID,
            coverage: Coverage::Box {
                north: MRMS_CONUS.north,
                south: MRMS_CONUS.south,
                west: MRMS_CONUS.west,
                east: MRMS_CONUS.east,
            },
            palette: Arc::new(template.palette),
            bounds: Arc::new(template.bounds.into_iter().map(f64::from).collect()),
            work: Arc::new(Semaphore::new(1)),
        }
    }

    pub fn metadata(&self) -> SourceMetadataBorrowed<'_> {
        SourceMetadataBorrowed {
            id: ID,
            family: Family::Grid,
            kind: Kind::Mosaic,
            default_product_class: ProductClass::Reflectivity,
            name: "NOAA MRMS — Contiguous U.S. mosaic",
            attribution: "NOAA/NSSL MRMS",
            mosaic: Some(MosaicMeta {
                coverage: &self.coverage,
                selection_priority: 0,
                covering: false,
            }),
        }
    }

    pub fn loading_placeholder(&self) -> Option<(MosaicFrame, Vec<u8>)> {
        Some((
            MosaicFrame {
                id: format!("{ID}-loading"),
                product: "REF".into(),
                product_name: "QC Base Reflectivity".into(),
                units: "dBZ".into(),
                scan_time: String::new(),
                sweep_end: None,
                status: FrameStatus::Partial,
                texture: String::new(),
                width: 1,
                height: 1,
                crs: Crs::wgs84_geographic(),
                geotransform: [-130.0, 70.0, 0.0, 55.0, 0.0, -35.0],
                palette: self.palette.as_ref().clone(),
                bounds: self.bounds.as_ref().clone(),
            },
            sweep::png(1, 1, &[0, 1, 0, 255]).ok()?,
        ))
    }

    pub fn poll(&self, events: GridSender, retained: Vec<i64>) -> JoinHandle<()> {
        let palette = Arc::clone(&self.palette);
        let bounds = Arc::clone(&self.bounds);
        tokio::spawn(poll_loop(
            events,
            retained,
            list_http,
            get_http,
            Arc::new(move |bytes, stamp| decode_frame(bytes, stamp, &palette, &bounds)),
            Arc::clone(&self.work),
            POLL_INTERVAL,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Object {
    key: String,
    stamp: NaiveDateTime,
}

fn prefix(day: NaiveDate) -> String {
    format!("CONUS/{PRODUCT}/{}/", day.format("%Y%m%d"))
}

fn object(key: &str, day: NaiveDate) -> Option<Object> {
    let head = format!("{}MRMS_{PRODUCT}_", prefix(day));
    let time = key.strip_prefix(&head)?.strip_suffix(".grib2.gz")?;
    if time.len() != 15 {
        return None;
    }
    let stamp = NaiveDateTime::parse_from_str(time, "%Y%m%d-%H%M%S").ok()?;
    if stamp.date() != day || stamp.format("%Y%m%d-%H%M%S").to_string() != time {
        return None;
    }
    Some(Object {
        key: key.into(),
        stamp,
    })
}

struct Page {
    objects: Vec<Object>,
    next: Option<String>,
}

fn parse_page(bytes: &[u8], day: NaiveDate) -> Result<Page, String> {
    if bytes.len() > LISTING_MAX {
        return Err("MRMS listing exceeds byte limit".into());
    }
    let parser = ParserConfig::new()
        .trim_whitespace(true)
        .ignore_comments(true)
        .max_name_length(128)
        .max_attribute_length(4096)
        .max_data_length(4096)
        .max_entity_expansion_length(4096)
        .create_reader(bytes);
    let mut path = Vec::new();
    let mut text = String::new();
    let mut truncated = None;
    let mut next = None;
    let mut objects = Vec::new();
    let mut count = 0;
    let mut root_seen = false;
    for event in parser {
        match event.map_err(|e| format!("MRMS listing XML: {e}"))? {
            XmlEvent::StartElement { name, .. } => {
                if path.is_empty() {
                    if root_seen || name.local_name != "ListBucketResult" {
                        return Err("MRMS listing: unexpected root".into());
                    }
                    root_seen = true;
                }
                path.push(name.local_name);
                if path.len() > 4 {
                    return Err("MRMS listing: excessive XML nesting".into());
                }
                if path.len() == 2 && path[1] == "Contents" {
                    count += 1;
                    if count > 1000 {
                        return Err("MRMS listing: too many objects in a page".into());
                    }
                }
                text.clear();
            }
            XmlEvent::Characters(value) | XmlEvent::CData(value) => {
                if text.len() + value.len() > 4096 {
                    return Err("MRMS listing: oversized field".into());
                }
                text.push_str(&value);
            }
            XmlEvent::EndElement { .. } => {
                if path.len() == 3 && path[1] == "Contents" && path[2] == "Key" {
                    if let Some(obj) = object(&text, day) {
                        objects.push(obj);
                    }
                } else if path.len() == 2 && path[1] == "IsTruncated" {
                    if truncated.is_some() {
                        return Err("MRMS listing: duplicate truncation flag".into());
                    }
                    truncated = Some(match text.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return Err("MRMS listing: invalid truncation flag".into()),
                    });
                } else if path.len() == 2 && path[1] == "NextContinuationToken" {
                    if next.is_some() || text.is_empty() {
                        return Err("MRMS listing: invalid continuation token".into());
                    }
                    next = Some(text.clone());
                }
                path.pop();
                text.clear();
            }
            _ => {}
        }
    }
    if !root_seen || truncated != Some(next.is_some()) {
        return Err("MRMS listing: missing or inconsistent pagination metadata".into());
    }
    Ok(Page { objects, next })
}

async fn discover<L, LF>(today: NaiveDate, list: &L) -> Result<Vec<Object>, String>
where
    L: Fn(NaiveDate, Option<String>) -> LF,
    LF: Future<Output = Result<Vec<u8>, String>>,
{
    let yesterday = today.pred_opt().ok_or("MRMS discovery: invalid UTC date")?;
    let mut objects = Vec::new();
    for day in [today, yesterday] {
        let mut token = None;
        let mut tokens = HashSet::new();
        for page_index in 0..PAGES_MAX {
            let bytes = list(day, token).await?;
            let page = parse_page(&bytes, day)?;
            objects.extend(page.objects);
            match page.next {
                None => break,
                Some(next) if page_index + 1 < PAGES_MAX && tokens.insert(next.clone()) => {
                    token = Some(next)
                }
                Some(_) => {
                    return Err(
                        "MRMS listing: incomplete discovery (page limit or repeated token)".into(),
                    );
                }
            }
        }
    }
    objects.sort_by_key(|o| o.stamp);
    objects.dedup_by_key(|o| o.stamp);
    Ok(objects)
}

type Decoded = (MosaicFrame, Vec<u8>, i64);

/// At most three newest candidates within the discovered hour. Known/older
/// observations recover feed status without another download or clock rollback.
async fn newest_valid<G, GF, D>(
    objects: &[Object],
    newest_ms: Option<i64>,
    get: &G,
    decode: Arc<D>,
    work: Arc<Semaphore>,
) -> Result<Option<Decoded>, String>
where
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
    D: Fn(Vec<u8>, NaiveDateTime) -> Result<Decoded, String> + Send + Sync + 'static,
{
    let mut failure = "MRMS: no valid observation".to_owned();
    for obj in objects.iter().rev().take(FALLBACK_MAX) {
        if !HISTORY.contains(
            obj.stamp.and_utc().timestamp_millis(),
            objects.last().unwrap().stamp.and_utc().timestamp_millis(),
        ) {
            break;
        }
        if newest_ms.is_some_and(|ms| obj.stamp.and_utc().timestamp_millis() <= ms) {
            return Ok(None);
        }
        let result = load_object(
            obj,
            get,
            Arc::clone(&decode),
            Arc::clone(&work),
            HTTP_TIMEOUT,
        )
        .await;
        match result {
            Ok(frame) => return Ok(Some(frame)),
            Err(e) => {
                failure = format!("{}: {e}", obj.key);
                eprintln!("MRMS {failure}");
            }
        }
    }
    Err(failure)
}

/// Only one object download/decode exists across live and history work. A
/// history download ends at the next poll deadline; an already running bounded
/// blocking decode finishes before the live poll gets the permit.
async fn load_object<G, GF, D>(
    obj: &Object,
    get: &G,
    decode: Arc<D>,
    work: Arc<Semaphore>,
    download_timeout: Duration,
) -> Result<Decoded, String>
where
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
    D: Fn(Vec<u8>, NaiveDateTime) -> Result<Decoded, String> + Send + Sync + 'static,
{
    // Waiting for a cancelled session's decoder also consumes the deadline.
    // Do not start a historical GET after live polling is already due.
    let (permit, bytes) = timeout(download_timeout, async {
        let permit = work.acquire_owned().await.map_err(|e| e.to_string())?;
        let bytes = get(obj.key.clone()).await?;
        Ok::<_, String>((permit, bytes))
    })
    .await
    .map_err(|_| "MRMS download timed out".to_owned())??;
    let stamp = obj.stamp;
    spawn_blocking(move || {
        let _permit = permit;
        decode(bytes, stamp)
    })
    .await
    .map_err(|e| format!("MRMS decode task: {e}"))?
}

/// Retained timestamps survive a poll restart; only acknowledged publications
/// enter this set. It stays bounded by the same policy as the engine timeline.
fn retain_known(known: &mut BTreeSet<i64>) {
    if let Some(newest) = known.last().copied() {
        known.retain(|&ms| HISTORY.contains(ms, newest));
        while known.len() > HISTORY.max_frames {
            known.pop_first();
        }
    }
}

fn history_candidates(objects: &[Object], known: &BTreeSet<i64>) -> Vec<Object> {
    let Some(&newest) = known.last() else {
        return vec![];
    };
    // Count already retained frames even if a later listing omits their keys.
    let mut slots = known.clone();
    slots.extend(
        objects
            .iter()
            .map(|o| o.stamp.and_utc().timestamp_millis())
            .filter(|&ms| HISTORY.contains(ms, newest)),
    );
    retain_known(&mut slots);
    objects
        .iter()
        .rev()
        .filter(|o| {
            let ms = o.stamp.and_utc().timestamp_millis();
            slots.contains(&ms) && !known.contains(&ms)
        })
        .cloned()
        .collect()
}

async fn poll_loop<L, LF, G, GF, D>(
    events: GridSender,
    retained: Vec<i64>,
    list: L,
    get: G,
    decode: Arc<D>,
    work: Arc<Semaphore>,
    poll_interval: Duration,
) where
    L: Fn(NaiveDate, Option<String>) -> LF,
    LF: Future<Output = Result<Vec<u8>, String>>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Vec<u8>, String>>,
    D: Fn(Vec<u8>, NaiveDateTime) -> Result<Decoded, String> + Send + Sync + 'static,
{
    let mut known: BTreeSet<i64> = retained.into_iter().collect();
    retain_known(&mut known);
    loop {
        let Ok(slot) = events.reserve().await else {
            return;
        };
        let next_poll = Instant::now() + poll_interval;
        let listed = timeout(HTTP_TIMEOUT, discover(Utc::now().date_naive(), &list))
            .await
            .map_err(|_| "MRMS discovery timed out".to_owned())
            .and_then(|r| r);
        let event = match &listed {
            Ok(objects) if objects.is_empty() => GridEvent::Silent {
                source_id: ID.into(),
                reason: "No QC base observations in current/previous UTC dates".into(),
            },
            Ok(objects) => match newest_valid(
                objects,
                known.last().copied(),
                &get,
                Arc::clone(&decode),
                Arc::clone(&work),
            )
            .await
            {
                Ok(Some((frame, texture, start_ms))) => GridEvent::Frame {
                    source_id: ID.into(),
                    frame: Box::new(frame),
                    texture,
                    start_ms,
                },
                Ok(None) => GridEvent::Online {
                    source_id: ID.into(),
                },
                Err(reason) => GridEvent::Offline {
                    source_id: ID.into(),
                    reason,
                },
            },
            Err(reason) => GridEvent::Offline {
                source_id: ID.into(),
                reason: reason.clone(),
            },
        };
        let accepted_ms = match &event {
            GridEvent::Frame { start_ms, .. } => Some(*start_ms),
            _ => None,
        };
        // Persist before marking known, so a storage failure can retry.
        if slot.send(event).await.unwrap_or(false)
            && let Some(ms) = accepted_ms
        {
            known.insert(ms);
            retain_known(&mut known);
        }
        if let Ok(objects) = listed {
            for obj in history_candidates(&objects, &known) {
                if Instant::now() >= next_poll {
                    break;
                }
                let Ok(slot) = events.reserve().await else {
                    return;
                };
                let remaining = next_poll.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match load_object(
                    &obj,
                    &get,
                    Arc::clone(&decode),
                    Arc::clone(&work),
                    remaining.min(HTTP_TIMEOUT),
                )
                .await
                {
                    Ok((frame, texture, start_ms)) => {
                        if slot
                            .send(GridEvent::Backfill {
                                source_id: ID.into(),
                                frame: Box::new(frame),
                                texture,
                                start_ms,
                            })
                            .await
                            .unwrap_or(false)
                        {
                            known.insert(start_ms);
                            retain_known(&mut known);
                        }
                    }
                    // A history gap must not overwrite healthy live status.
                    Err(reason) => eprintln!("MRMS history {}: {reason}", obj.key),
                }
            }
        }
        sleep_until(next_poll).await;
    }
}

fn listing_url(host: &str, day: NaiveDate, token: Option<&str>) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(host).map_err(|e| e.to_string())?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("list-type", "2")
            .append_pair("prefix", &prefix(day))
            .append_pair("max-keys", "1000");
        if let Some(token) = token {
            query.append_pair("continuation-token", token);
        }
    }
    Ok(url)
}

async fn fetch(url: reqwest::Url, limit: usize) -> Result<Vec<u8>, String> {
    let mut response = live_index::http_client()
        .get(url)
        .timeout(HTTP_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("MRMS HTTP: {e}"))?;
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err("MRMS HTTP body exceeds byte limit".into());
    }
    let mut body = BoundedBytes::new(limit);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("MRMS HTTP body: {e}"))?
    {
        body.write_all(&chunk)
            .map_err(|e| format!("MRMS HTTP body: {e}"))?;
    }
    Ok(body.bytes)
}

async fn list_http(day: NaiveDate, token: Option<String>) -> Result<Vec<u8>, String> {
    fetch(listing_url(HOST, day, token.as_deref())?, LISTING_MAX).await
}

async fn get_http(key: String) -> Result<Vec<u8>, String> {
    // Keys only come from the strict product/date parser, never arbitrary URLs.
    fetch(
        reqwest::Url::parse(&format!("{HOST}{key}")).map_err(|e| e.to_string())?,
        BODY_MAX,
    )
    .await
}

#[cfg(test)]
mod tests;
