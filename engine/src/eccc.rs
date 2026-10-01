//! ECCC GeoMet rain-rate classes. This deliberately decodes the discrete
//! WMS style, not reflectivity or raw measurements (issue #118).
use crate::grid_view::Region;
use crate::{
    live_index,
    protocol::{Coverage, Crs, Ellipsoid, Family, FrameStatus, Kind, MosaicFrame, ProductClass},
    source::{GridEvent, MosaicMeta, SourceMetadataBorrowed},
    sweep,
};
use chrono::{DateTime, SecondsFormat, Utc};
use std::{collections::HashSet, future::Future, io::Cursor, time::Duration};
use tokio::{
    sync::{Semaphore, mpsc::Sender},
    task::JoinHandle,
};
use xml::reader::{EventReader, XmlEvent};

pub const ID: &str = "eccc";
const NAME: &str = "Canada ECCC radar";
const HOST: &str = "https://geo.weather.gc.ca/geomet/";
const LAYER: &str = "RADAR_1KM_RRAI";
const STYLE: &str = "Radar-Rain_Dis-14colors";
pub const HISTORY_MAX: usize = 30;
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 1024;
pub const BODY_MAX: usize = 8 << 20;
const METADATA_MAX: usize = 512 << 10;
const MASK_LAYER: &str = "RADAR_COVERAGE_RRAI.INV";
const MASK_STYLE: &str = "Radar-Coverage_Inv-LightGray";
// GeoMet discrete legend, verified 2026-09-25. GetLegendGraphic's matplotlib
// image rounds some channels down by one; GetMap emits these integer RGBs.
const COLORS: [[u8; 3]; 14] = [
    [153, 204, 255],
    [0, 153, 255],
    [0, 255, 102],
    [0, 204, 0],
    [0, 153, 0],
    [0, 102, 0],
    [255, 255, 0],
    [255, 204, 0],
    [255, 153, 0],
    [255, 102, 0],
    [255, 0, 0],
    [255, 2, 153],
    [153, 51, 204],
    [102, 0, 153],
];
// Provider labels the last band 200+; its published class ends at 300 mm/h.
const BOUNDS: [f64; 15] = [
    0.1, 1., 2., 4., 8., 12., 16., 24., 32., 50., 64., 100., 125., 200., 300.,
];

pub struct Eccc {
    pub id: &'static str,
    coverage: Coverage,
    pub region: Region,
    pub selection_footprint: crate::protocol::SelectionFootprint,
    decode_slot: std::sync::Arc<Semaphore>,
}
impl Eccc {
    pub fn new() -> Self {
        let b = crate::envelope::ECCC;
        Self {
            id: ID,
            region: default_region(),
            selection_footprint: serde_json::from_str(include_str!("../data/eccc-selection.json"))
                .unwrap(),
            decode_slot: std::sync::Arc::new(Semaphore::new(1)),
            coverage: Coverage::Box {
                north: b.north,
                south: b.south,
                east: b.east,
                west: b.west,
            },
        }
    }
    pub fn metadata(&self) -> SourceMetadataBorrowed<'_> {
        SourceMetadataBorrowed {
            id: ID,
            family: Family::Grid,
            kind: Kind::Mosaic,
            default_product_class: ProductClass::PrecipitationRate,
            name: NAME,
            attribution: "Environment and Climate Change Canada / NOAA",
            mosaic: Some(MosaicMeta {
                coverage: &self.coverage,
                selection_footprint: Some(&self.selection_footprint),
                selection_priority: 20,
                covering: true,
            }),
        }
    }
    pub fn loading_placeholder(&self) -> Option<(MosaicFrame, Vec<u8>)> {
        let mut frame = frame_template();
        frame.width = 1;
        frame.height = 1;
        Some((frame, sweep::png(1, 1, &[0, 8, 0, 255]).ok()?))
    }
    pub fn poll(&self, events: Sender<GridEvent>, known: HashSet<String>) -> JoinHandle<()> {
        tokio::spawn(poll_loop(
            events,
            known,
            self.region,
            self.decode_slot.clone(),
        ))
    }
}
fn default_region() -> Region {
    Region::choose(
        crate::protocol::GeoPoint {
            lat: 53.5461,
            lon: -113.4938,
        },
        None,
        None,
    )
}
fn frame_template() -> MosaicFrame {
    regional_frame(default_region())
}
fn regional_frame(region: Region) -> MosaicFrame {
    MosaicFrame {
        id: format!("{ID}-loading"),
        product: "RATE".into(),
        product_name: "Rain rate estimate".into(),
        units: "mm/h".into(),
        scan_time: String::new(),
        sweep_end: None,
        status: FrameStatus::Partial,
        texture: String::new(),
        width: WIDTH,
        height: HEIGHT,
        crs: Crs::Mercator {
            ellipsoid: Ellipsoid {
                semi_major_m: 6378137.,
                inverse_flattening: 0.,
            },
            lon0_deg: 0.,
            scale: 1.,
            false_easting_m: 0.,
            false_northing_m: 0.,
            datum_transform: None,
        },
        geotransform: region.affine(),
        palette: COLORS
            .iter()
            .map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]))
            .collect(),
        bounds: BOUNDS.to_vec(),
    }
}
#[cfg(test)]
fn frame_id(stamp: DateTime<Utc>) -> String {
    regional_id(default_region(), stamp)
}
fn regional_id(region: Region, stamp: DateTime<Utc>) -> String {
    format!("{}-{}", region.key(), stamp.format("%Y%m%dT%H%M%SZ"))
}

/// Read only this layer's observed time dimension. Never invent timestamps
/// from the wall clock or accept forecast layers / silently changed cadence.
#[cfg(test)]
fn timestamps(body: &[u8]) -> Result<Vec<DateTime<Utc>>, String> {
    layer_timestamps(body, LAYER)
}
fn layer_timestamps(body: &[u8], layer: &str) -> Result<Vec<DateTime<Utc>>, String> {
    if body.len() > METADATA_MAX {
        return Err("ECCC metadata limit".into());
    }
    let mut layer_names = Vec::<String>::new();
    let mut elements = Vec::<String>::new();
    let mut in_name = false;
    let mut in_time = false;
    let mut dimension = String::new();
    let config = xml::reader::ParserConfig::new()
        .max_data_length(METADATA_MAX)
        .max_entity_expansion_length(METADATA_MAX)
        .max_entity_expansion_depth(4)
        .max_name_length(128)
        .max_attributes(32)
        .max_attribute_length(4096);
    for event in EventReader::new_with_config(body, config) {
        match event.map_err(|e| format!("ECCC capabilities: {e}"))? {
            XmlEvent::StartElement {
                name, attributes, ..
            } => {
                let layer_child = elements.last().is_some_and(|parent| parent == "Layer");
                match name.local_name.as_str() {
                    "Layer" => {
                        if layer_names.len() >= 32 {
                            return Err("ECCC layer nesting limit".into());
                        }
                        layer_names.push(String::new());
                    }
                    "Name" => in_name = layer_child,
                    "Dimension" => {
                        in_time = layer_child
                            && layer_names.last().is_some_and(|n| n.trim() == layer)
                            && attributes
                                .iter()
                                .any(|a| a.name.local_name == "name" && a.value == "time");
                        if in_time
                            && attributes.iter().any(|a| {
                                a.name.local_name == "nearestValue"
                                    && !matches!(a.value.as_str(), "0" | "false")
                            })
                        {
                            return Err("ECCC nearest TIME substitution unsupported".into());
                        }
                    }
                    _ => {}
                }
                if elements.len() >= 64 {
                    return Err("ECCC XML nesting limit".into());
                }
                elements.push(name.local_name);
            }
            XmlEvent::Characters(s) | XmlEvent::CData(s) => {
                if in_name && let Some(n) = layer_names.last_mut() {
                    n.push_str(&s);
                }
                if in_time {
                    if dimension.len().saturating_add(s.len()) > 1024 {
                        return Err("ECCC time dimension limit".into());
                    }
                    dimension.push_str(&s);
                }
            }
            XmlEvent::EndElement { name } => {
                match name.local_name.as_str() {
                    "Layer" => {
                        layer_names.pop();
                    }
                    "Name" => in_name = false,
                    "Dimension" => in_time = false,
                    _ => {}
                }
                elements.pop();
            }
            _ => {}
        }
    }
    let parts: Vec<_> = dimension.trim().split('/').collect();
    if parts.len() != 3 || parts[2] != "PT6M" {
        return Err("ECCC missing or unsupported observed time dimension".into());
    }
    let parse = |s: &str| {
        DateTime::parse_from_rfc3339(s)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|e| format!("ECCC time: {e}"))
    };
    let start = parse(parts[0])?;
    let end = parse(parts[1])?;
    if end < start || (end - start).num_seconds() % 360 != 0 {
        return Err("ECCC invalid time interval".into());
    }
    Ok((0..HISTORY_MAX)
        .map(|i| end - chrono::Duration::minutes(i as i64 * 6))
        .take_while(|s| *s >= start)
        .collect())
}

fn map_url(stamp: DateTime<Utc>, region: Region, mask: bool) -> String {
    let [w, s, e, n] = region.bbox();
    let (layer, style) = if mask {
        (MASK_LAYER, MASK_STYLE)
    } else {
        (LAYER, STYLE)
    };
    format!(
        "{HOST}?SERVICE=WMS&VERSION=1.1.1&REQUEST=GetMap&LAYERS={layer}&STYLES={style}&SRS=EPSG:3857&BBOX={w},{s},{e},{n}&WIDTH={WIDTH}&HEIGHT={HEIGHT}&FORMAT=image/png&TRANSPARENT=TRUE&TIME={}",
        stamp.to_rfc3339_opts(SecondsFormat::Secs, true)
    )
}

/// Inspect the original IHDR before expansion. Only the verified unprofiled
/// eight-bit RGBA/indexed formats are accepted; compressed size is independent.
fn rgba(body: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if body.len() > BODY_MAX || width == 0 || height == 0 || width > WIDTH || height > HEIGHT {
        return Err("ECCC image limit".into());
    }
    if !body.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("ECCC expected PNG, not service exception".into());
    }
    let mut at = 8usize;
    while at < body.len() {
        let header = body.get(at..at + 8).ok_or("ECCC truncated chunk")?;
        let n = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = at
            .checked_add(n)
            .and_then(|v| v.checked_add(12))
            .ok_or("ECCC chunk overflow")?;
        if end > body.len() {
            return Err("ECCC truncated chunk".into());
        }
        if matches!(&header[4..8], b"acTL" | b"fcTL" | b"fdAT") {
            return Err("ECCC animated PNG unsupported".into());
        }
        if &header[4..8] == b"IEND" && end != body.len() {
            return Err("ECCC trailing PNG data".into());
        }
        if matches!(
            &header[4..8],
            b"iCCP" | b"sRGB" | b"gAMA" | b"cHRM" | b"cICP" | b"mDCV" | b"cLLI"
        ) {
            return Err("ECCC unsupported colour profile".into());
        }
        at = end;
    }
    let mut decoder = png::Decoder::new(Cursor::new(body));
    decoder.set_ignore_text_chunk(true);
    decoder.set_transformations(png::Transformations::EXPAND);
    decoder.set_limits(png::Limits { bytes: BODY_MAX });
    let mut reader = decoder.read_info().map_err(|e| format!("ECCC PNG: {e}"))?;
    let info = reader.info();
    if info.width != width
        || info.height != height
        || info.bit_depth != png::BitDepth::Eight
        || !matches!(
            info.color_type,
            png::ColorType::Rgba | png::ColorType::Indexed
        )
    {
        return Err("ECCC unexpected PNG dimensions, type or bit depth".into());
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|v| v.checked_mul(4))
        .ok_or("ECCC pixel overflow")?;
    let size = reader
        .output_buffer_size()
        .filter(|n| *n == expected)
        .ok_or("ECCC expected RGBA expansion")?;
    let mut pixels = vec![0; size];
    let output = reader
        .next_frame(&mut pixels)
        .map_err(|e| format!("ECCC PNG: {e}"))?;
    if output.color_type != png::ColorType::Rgba || output.buffer_size() != expected {
        return Err("ECCC PNG must include transparency".into());
    }
    reader.finish().map_err(|e| format!("ECCC PNG end: {e}"))?;
    Ok(pixels)
}

/// Endpoint neighbourhood certainty, using two bounded summed-area tables.
/// Each query is constant work even at the coarsest/polar regional tier.
fn mask_flags(mask: &[u8], width: usize, height: usize, radius: usize) -> Result<Vec<u8>, String> {
    let stride = width + 1;
    let mut outside = vec![0u32; stride * (height + 1)];
    let mut ambiguous = vec![0u32; outside.len()];
    for y in 0..height {
        let mut o = 0;
        let mut a = 0;
        for x in 0..width {
            let p = &mask[(y * width + x) * 4..][..4];
            if p[3] > 128
                || (p[3] != 0 && (p[0] != p[1] || p[1] != p[2]))
                || (p[3] == 128 && p[..3] != [181, 181, 181])
            {
                return Err("ECCC changed inverse coverage style".into());
            }
            o += u32::from(p[3] == 128);
            a += u32::from(p[3] != 0 && p[3] != 128);
            outside[(y + 1) * stride + x + 1] = outside[y * stride + x + 1] + o;
            ambiguous[(y + 1) * stride + x + 1] = ambiguous[y * stride + x + 1] + a;
        }
    }
    let mut flags = vec![8; width * height];
    if radius >= width || radius >= height {
        return Ok(flags);
    }
    let area = ((2 * radius + 1) * (2 * radius + 1)) as u32;
    let sum = |table: &[u32], x: usize, y: usize| {
        let (l, r, t, b) = (x - radius, x + radius + 1, y - radius, y + radius + 1);
        table[b * stride + r] + table[t * stride + l]
            - table[t * stride + r]
            - table[b * stride + l]
    };
    for y in radius..height - radius {
        for x in radius..width - radius {
            if sum(&ambiguous, x, y) == 0 {
                flags[y * width + x] = match sum(&outside, x, y) {
                    0 => 2,
                    n if n == area => 4,
                    _ => 8,
                };
            }
        }
    }
    Ok(flags)
}

fn decode_pair(
    body: &[u8],
    mask: Option<&[u8]>,
    width: u32,
    height: u32,
    margin: usize,
) -> Result<Vec<u8>, String> {
    let started = std::time::Instant::now();
    let mut pixels = rgba(body, width, height)?;
    let mask_pixels = mask.and_then(|m| {
        rgba(m, width, height)
            .map_err(|e| eprintln!("ECCC mask: {e}"))
            .ok()
    });
    let decode_time = started.elapsed();
    let started = std::time::Instant::now();
    let flags = mask_pixels.as_deref().and_then(|p| {
        mask_flags(p, width as usize, height as usize, margin)
            .map_err(|e| eprintln!("ECCC mask: {e}"))
            .ok()
    });
    drop(mask_pixels);
    for pixel in pixels.as_chunks_mut::<4>().0 {
        if pixel[3] == 0 {
            pixel.copy_from_slice(&[0, 8, 0, 255]);
        } else {
            if pixel[3] != 255 {
                return Err("ECCC unexpected translucent radar pixel".into());
            }
            let class = COLORS
                .iter()
                .position(|c| pixel[..3] == c[..])
                .ok_or_else(|| format!("ECCC unexpected radar colour {:?}", &pixel[..3]))?;
            pixel.copy_from_slice(&[class as u8 + 1, 0, 0, 255]);
        }
    }
    if let Some(flags) = flags {
        // A deep contradiction invalidates all blank-cell certainty for this
        // pair. Never erase measured weather to make a mask agree.
        let contradicted = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(&flags)
            .any(|(p, f)| p[0] != 0 && *f == 4);
        if !contradicted {
            for (p, f) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(flags) {
                if p[0] == 0 {
                    p[1] = f;
                }
            }
        }
    }
    let classify_time = started.elapsed();
    let started = std::time::Instant::now();
    let encoded = sweep::png(width, height, &pixels).map_err(|e| format!("ECCC texture: {e}"))?;
    eprintln!(
        "ECCC stages: decode {decode_time:?} classify {classify_time:?} encode {:?}",
        started.elapsed()
    );
    Ok(encoded)
}
#[cfg(test)]
fn decode(body: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    decode_pair(body, None, width, height, 2)
}

async fn get(url: String, cap: usize) -> Result<Vec<u8>, String> {
    // Isolated mock-provider replay, as with the METAR endpoint overrides.
    let url = if let Ok(endpoint) = std::env::var("OMASTORM_ECCC_URL") {
        format!(
            "{}?{}",
            endpoint.trim_end_matches('?'),
            url.split_once('?').ok_or("ECCC URL missing query")?.1
        )
    } else {
        url
    };
    let started = std::time::Instant::now();
    let response = live_index::http_client()
        .get(&url)
        .header(reqwest::header::USER_AGENT, "omastorm/Canada-radar")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let body = live_index::take_body(response, cap)
        .await
        .map_err(|e| e.to_string())?;
    eprintln!(
        "ECCC response: {url} bytes {} elapsed {:?}",
        body.len(),
        started.elapsed()
    );
    Ok(body)
}
/// Files (including orphan/atomic revisions) are the ledger. Inputs are
/// memory-only, so reserving their full compressed caps is conservative.
/// Every worker checks admission under the shared decode permit; cancelled
/// blocking workers keep that permit through publication.
fn admitted(dir: &std::path::Path, region: Region) -> Result<bool, String> {
    let mut bytes = 0u64;
    let mut regions = HashSet::new();
    let tex = dir.join("tex");
    if !tex.exists() {
        return Ok(true);
    }
    for entry in std::fs::read_dir(tex).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("mosaic-eccc-") {
            continue;
        }
        let size = match entry.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        bytes = bytes.checked_add(size).ok_or("ECCC ledger overflow")?;
        if let Some((dated, _)) = name.strip_prefix("mosaic-").unwrap().split_once("Z-")
            && let Some((key, _)) = dated.rsplit_once('-')
        {
            regions.insert(key.to_string());
        }
    }
    // 16 MiB input + 8 MiB output reservation leaves 40 MiB retained.
    Ok(bytes <= (40 << 20) && (regions.contains(&region.key()) || regions.len() < 2))
}
async fn load(
    stamp: DateTime<Utc>,
    region: Region,
    slot: std::sync::Arc<Semaphore>,
) -> Result<(MosaicFrame, Vec<u8>), String> {
    let permit = slot.acquire_owned().await.map_err(|e| e.to_string())?;
    let dir = crate::runtime_path().map_err(|e| e.to_string())?;
    while !admitted(&dir, region)? {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let started = std::time::Instant::now();
    let body = get(map_url(stamp, region, false), BODY_MAX).await?;
    let mask = get(map_url(stamp, region, true), BODY_MAX)
        .await
        .map_err(|e| eprintln!("ECCC mask unavailable: {e}"))
        .ok();
    let download = started.elapsed();
    let (frame, texture) =
        tokio::task::spawn_blocking(move || -> Result<(MosaicFrame, Vec<u8>), String> {
            let _permit = permit;
            let started = std::time::Instant::now();
            let texture = decode_pair(&body, mask.as_deref(), WIDTH, HEIGHT, region.margin())?;
            let cpu = started.elapsed();
            drop(body);
            drop(mask);
            let mut frame = regional_frame(region);
            frame.id = regional_id(region, stamp);
            frame.scan_time = stamp.to_rfc3339_opts(SecondsFormat::Secs, true);
            frame.status = FrameStatus::Complete;
            if texture.len() > BODY_MAX {
                return Err("ECCC classified output limit".into());
            }
            frame.texture =
                crate::publish(&dir, "mosaic", &frame.id, &texture).map_err(|e| e.to_string())?;
            eprintln!(
                "ECCC {}: download {download:?} decode/classify/encode {cpu:?} output {}",
                frame.id,
                texture.len()
            );
            Ok((frame, Vec::new()))
        })
        .await
        .map_err(|e| e.to_string())??;
    Ok((frame, texture))
}
async fn advertised() -> Result<Vec<DateTime<Utc>>, String> {
    let mut times = Vec::new();
    for layer in [LAYER, MASK_LAYER] {
        let body = get(
            format!("{HOST}?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetCapabilities&LAYERS={layer}"),
            METADATA_MAX,
        )
        .await?;
        let stamps = layer_timestamps(&body, layer)?;
        if layer == LAYER {
            times = stamps;
        } else {
            times.retain(|t| stamps.contains(t));
        }
    }
    if times.is_empty() {
        return Err("ECCC no common observed TIME".into());
    }
    Ok(times)
}
async fn poll_loop(
    events: Sender<GridEvent>,
    mut known: HashSet<String>,
    region: Region,
    slot: std::sync::Arc<Semaphore>,
) {
    loop {
        let stamps = match advertised().await {
            Ok(stamps) => stamps,
            Err(reason) => {
                if events
                    .send(GridEvent::Offline {
                        source_id: ID.into(),
                        reason,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
        };
        if !refresh_frames_region(&events, &mut known, stamps, region, |stamp| {
            load(stamp, region, slot.clone())
        })
        .await
        {
            return;
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// One refresh, with an injectable loader for failure/retry/cancellation tests.
/// Newest-frame failure reports offline; a missing historical frame must not
/// prevent other history from loading. Only published frames become known.
/// Returns false when the event receiver has closed.
async fn refresh_frames_region<L, F>(
    events: &Sender<GridEvent>,
    known: &mut HashSet<String>,
    stamps: Vec<DateTime<Utc>>,
    region: Region,
    mut load: L,
) -> bool
where
    L: FnMut(DateTime<Utc>) -> F,
    F: Future<Output = Result<(MosaicFrame, Vec<u8>), String>>,
{
    let ids: HashSet<_> = stamps
        .iter()
        .copied()
        .map(|stamp| regional_id(region, stamp))
        .collect();
    known.retain(|id| ids.contains(id));
    // Recheck the newest observation even when history is slow.
    let fill_started = std::time::Instant::now();
    for (i, stamp) in stamps.into_iter().enumerate() {
        if i > 0 && fill_started.elapsed() >= Duration::from_secs(60) {
            break;
        }
        if events.is_closed() {
            return false;
        }
        let id = regional_id(region, stamp);
        if known.contains(&id) {
            continue;
        }
        match load(stamp).await {
            Ok((frame, texture)) => {
                let event = if i == 0 {
                    GridEvent::Frame {
                        source_id: ID.into(),
                        frame: Box::new(frame),
                        texture,
                        start_ms: stamp.timestamp_millis(),
                    }
                } else {
                    GridEvent::Backfill {
                        source_id: ID.into(),
                        frame: Box::new(frame),
                        texture,
                        start_ms: stamp.timestamp_millis(),
                    }
                };
                if events.send(event).await.is_err() {
                    return false;
                }
                known.insert(id);
                // Give source switches time to cancel before downloading history.
                if i == 0 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
            Err(reason) => {
                if i > 0 {
                    eprintln!("ECCC history: {reason}");
                    continue;
                }
                if events
                    .send(GridEvent::Offline {
                        source_id: ID.into(),
                        reason,
                    })
                    .await
                    .is_err()
                {
                    return false;
                }
                break;
            }
        }
    }
    !events.is_closed()
}

#[cfg(test)]
async fn refresh_frames<L, F>(
    events: &Sender<GridEvent>,
    known: &mut HashSet<String>,
    stamps: Vec<DateTime<Utc>>,
    load: L,
) -> bool
where
    L: FnMut(DateTime<Utc>) -> F,
    F: Future<Output = Result<(MosaicFrame, Vec<u8>), String>>,
{
    refresh_frames_region(events, known, stamps, default_region(), load).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_keeps_the_blocking_decode_permit_until_worker_exit() {
        runtime().block_on(async {
            let slot = std::sync::Arc::new(Semaphore::new(1));
            let permit = slot.clone().acquire_owned().await.unwrap();
            let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(1);
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let task = tokio::spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    started_tx.blocking_send(()).unwrap();
                    release_rx.recv().unwrap();
                })
                .await
                .unwrap();
            });
            started_rx.recv().await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert!(slot.try_acquire().is_err());
            release_tx.send(()).unwrap();
            let _permit = tokio::time::timeout(Duration::from_secs(1), slot.acquire())
                .await
                .unwrap()
                .unwrap();
        });
    }
    #[test]
    fn byte_ledger_includes_atomic_retiring_and_duplicate_revisions() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../target/eccc-ledger-{}", std::process::id()));
        let tex = dir.join("tex");
        std::fs::create_dir_all(&tex).unwrap();
        let a = default_region();
        let b = Region { x: a.x + 2, ..a };
        let c = Region { x: a.x + 4, ..a };
        let write = |region: Region, revision: u32, size: u64, suffix: &str| {
            let path = tex.join(format!(
                "mosaic-{}-20261001T133600Z-20200-r{revision}.png{suffix}",
                region.key()
            ));
            std::fs::File::create(path).unwrap().set_len(size).unwrap();
        };
        write(a, 1, 20 << 20, "");
        write(a, 2, 20 << 20, ".tmp");
        assert!(admitted(&dir, a).unwrap());
        write(b, 3, 1, "");
        assert!(!admitted(&dir, a).unwrap());
        std::fs::remove_file(tex.join(format!(
            "mosaic-{}-20261001T133600Z-20200-r2.png.tmp",
            a.key()
        )))
        .unwrap();
        assert!(admitted(&dir, b).unwrap());
        assert!(!admitted(&dir, c).unwrap());
        // The ledger has not deleted any referenced/retiring file to admit C.
        assert_eq!(std::fs::read_dir(&tex).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
    fn pixels(encoded: &[u8]) -> Vec<u8> {
        let mut reader = png::Decoder::new(Cursor::new(encoded)).read_info().unwrap();
        let mut out = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut out).unwrap();
        out
    }
    #[test]
    fn time_aligned_mask_loss_edges_and_contradictions_preserve_weather() {
        let (w, h) = (16, 16);
        let mut rain = vec![0u8; w * h * 4];
        rain[4 * (8 * w + 8)..][..4].copy_from_slice(&[153, 204, 255, 255]);
        let rain = sweep::png(w as u32, h as u32, &rain).unwrap();
        let covered = sweep::png(w as u32, h as u32, &vec![0; w * h * 4]).unwrap();
        let outside = sweep::png(w as u32, h as u32, &[181, 181, 181, 128].repeat(w * h)).unwrap();
        let known = pixels(&decode_pair(&rain, Some(&covered), 16, 16, 2).unwrap());
        assert_eq!(&known[4 * (8 * w + 8)..][..4], &[1, 0, 0, 255]);
        assert_eq!(known[4 * (5 * w + 5) + 1], 2);
        assert_eq!(known[1], 8); // fetch edge cannot certify observation coverage
        for mask in [
            None,
            Some(outside.as_slice()),
            Some(b"<NoMatch/>".as_slice()),
        ] {
            let result = pixels(&decode_pair(&rain, mask, 16, 16, 2).unwrap());
            assert_eq!(&result[4 * (8 * w + 8)..][..4], &[1, 0, 0, 255]);
            assert_eq!(result[4 * (5 * w + 5) + 1], 8); // unavailable mask/deep contradiction
        }
        let mut mask = vec![0; w * h * 4];
        for y in 0..h {
            for x in 0..6 {
                mask[4 * (y * w + x)..][..4].copy_from_slice(&[181, 181, 181, 128]);
            }
        }
        for y in 0..h {
            mask[4 * (y * w + 6)..][..4].copy_from_slice(&[181, 181, 181, 64]);
        }
        let mask = sweep::png(16, 16, &mask).unwrap();
        let result = pixels(&decode_pair(&rain, Some(&mask), 16, 16, 2).unwrap());
        assert_eq!(result[4 * (5 * w + 3) + 1], 4);
        assert_eq!(result[4 * (5 * w + 6) + 1], 8);
        assert_eq!(result[4 * (5 * w + 10) + 1], 2);
        // A next-time contributor loss changes blank flags, never intensities.
        let clear = sweep::png(16, 16, &vec![0; w * h * 4]).unwrap();
        let lost = pixels(&decode_pair(&clear, Some(&outside), 16, 16, 2).unwrap());
        assert_eq!(lost[4 * (5 * w + 5) + 1], 4);
    }
    #[test]
    fn indexed_equivalence_and_untrusted_headers_are_bounded() {
        let mut indexed = Vec::new();
        let mut encoder = png::Encoder::new(&mut indexed, 15, 1);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(
            COLORS
                .iter()
                .flat_map(|c| *c)
                .chain([1, 2, 3])
                .collect::<Vec<_>>(),
        );
        encoder.set_trns([vec![255; 14], vec![0]].concat());
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&(0..15).collect::<Vec<_>>())
            .unwrap();
        let rgba = sweep::png(
            15,
            1,
            &COLORS
                .iter()
                .flat_map(|c| [c[0], c[1], c[2], 255])
                .chain([1, 2, 3, 0])
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            pixels(&decode(&indexed, 15, 1).unwrap()),
            pixels(&decode(&rgba, 15, 1).unwrap())
        );
        assert!(decode(&vec![0; BODY_MAX + 1], 1, 1).is_err());
        let bomb = sweep::png(1025, 1, &[0, 0, 0, 0].repeat(1025)).unwrap();
        assert!(decode(&bomb, 1025, 1).is_err());
        for color in [png::ColorType::Rgb, png::ColorType::GrayscaleAlpha] {
            let mut input = Vec::new();
            let mut e = png::Encoder::new(&mut input, 1, 1);
            e.set_color(color);
            e.set_depth(png::BitDepth::Eight);
            e.write_header()
                .unwrap()
                .write_image_data(&vec![0; color.samples()])
                .unwrap();
            assert!(decode(&input, 1, 1).is_err());
        }
        let mut input = Vec::new();
        let mut e = png::Encoder::new(&mut input, 1, 1);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.set_source_gamma(png::ScaledFloat::new(0.45455));
        e.write_header()
            .unwrap()
            .write_image_data(&[0, 0, 0, 0])
            .unwrap();
        assert!(decode(&input, 1, 1).is_err());
    }
    #[test]
    fn frozen_selection_preserves_disconnected_areas_and_internal_holes() {
        let source = Eccc::new();
        let footprint = &source.selection_footprint;
        assert_eq!(footprint.kind, "MultiPolygon");
        assert_eq!(footprint.coordinates.len(), 4);
        assert_eq!(
            footprint
                .coordinates
                .iter()
                .map(|p| p.len() - 1)
                .sum::<usize>(),
            3
        );
        let mut vertices = 0;
        for polygon in &footprint.coordinates {
            for ring in polygon {
                assert_eq!(ring.first(), ring.last());
                vertices += ring.len();
                for [lon, lat] in ring {
                    assert!((-170.32..=-50.).contains(lon));
                    assert!((16.93..=67.19).contains(lat));
                }
            }
        }
        assert!(vertices <= 4096);
        assert!(footprint.contains(crate::protocol::GeoPoint {
            lat: 53.5461,
            lon: -113.4938
        }));
        assert!(!footprint.contains(crate::protocol::GeoPoint {
            lat: 52.8,
            lon: -118.5
        }));
        assert!(!footprint.contains(crate::protocol::GeoPoint {
            lat: 62.454,
            lon: -114.377
        }));
        assert!(serde_json::to_vec(footprint).unwrap().len() <= METADATA_MAX);
    }
    #[test]
    fn observed_times_are_bounded_and_newest_first() {
        let xml = br#"<Layer><Name>other</Name><Dimension name="time">bogus</Dimension><Layer><Name>RADAR_1KM_RRAI</Name><Dimension name="time">2026-09-25T12:00:00Z/2026-09-25T15:00:00Z/PT6M</Dimension></Layer></Layer>"#;
        let times = timestamps(xml).unwrap();
        assert_eq!(times.len(), HISTORY_MAX);
        assert_eq!(times[0].to_rfc3339(), "2026-09-25T15:00:00+00:00");
        assert_eq!(times[29].to_rfc3339(), "2026-09-25T12:06:00+00:00");
        assert!(timestamps(b"<Layer/>").is_err());
        assert!(
            timestamps(
                &String::from_utf8_lossy(xml)
                    .replace("name=\"time\"", "name=\"time\" nearestValue=\"1\"")
                    .into_bytes()
            )
            .is_err()
        );
        assert!(
            timestamps(
                &String::from_utf8_lossy(xml)
                    .replace("PT6M", "PT5M")
                    .into_bytes()
            )
            .is_err()
        );
        assert!(
            timestamps(
                &String::from_utf8_lossy(xml)
                    .replace("15:00:00", "11:00:00")
                    .into_bytes()
            )
            .is_err()
        );
    }
    #[test]
    fn time_dimension_belongs_to_layer_not_nested_style() {
        let xml = br#"<Layer><Name>RADAR_1KM_RRAI</Name>
          <Style><Name>Radar-Rain_Dis-14colors</Name></Style>
          <Dimension name="time">2026-09-25T12:00:00Z/2026-09-25T12:06:00Z/PT6M</Dimension>
          <Layer><Name>other</Name><Dimension name="time">invalid</Dimension></Layer>
        </Layer>"#;
        assert_eq!(timestamps(xml).unwrap().len(), 2);
        let nested_only = br#"<Layer><Style><Name>RADAR_1KM_RRAI</Name></Style>
          <Dimension name="time">2026-09-25T12:00:00Z/2026-09-25T12:06:00Z/PT6M</Dimension>
        </Layer>"#;
        assert!(timestamps(nested_only).is_err());
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
    fn test_stamps() -> Vec<DateTime<Utc>> {
        let newest = DateTime::parse_from_rfc3339("2026-09-25T12:12:00Z")
            .unwrap()
            .with_timezone(&Utc);
        (0..3)
            .map(|i| newest - chrono::Duration::minutes(i * 6))
            .collect()
    }
    fn loaded(stamp: DateTime<Utc>) -> (MosaicFrame, Vec<u8>) {
        let mut frame = frame_template();
        frame.id = frame_id(stamp);
        frame.scan_time = stamp.to_rfc3339();
        frame.status = FrameStatus::Complete;
        (frame, vec![])
    }

    #[test]
    fn history_gap_does_not_block_older_frames_and_is_retried() {
        runtime().block_on(async {
            let stamps = test_stamps();
            let missing = stamps[1];
            let mut known = HashSet::from([frame_id(stamps[0]), "expired".into()]);
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            assert!(refresh_frames(&tx, &mut known, stamps.clone(), |stamp| async move {
                if stamp == missing { Err("missing history frame".into()) } else { Ok(loaded(stamp)) }
            }).await);
            assert!(matches!(rx.try_recv(), Ok(GridEvent::Backfill { start_ms, .. }) if start_ms == stamps[2].timestamp_millis()));
            assert!(rx.try_recv().is_err()); // Historical failure does not mark the feed offline.
            assert!(!known.contains("expired"));
            assert!(!known.contains(&frame_id(missing)));
            let mut calls = vec![];
            assert!(refresh_frames(&tx, &mut known, stamps.clone(), |stamp| {
                calls.push(stamp);
                std::future::ready(Ok(loaded(stamp)))
            }).await);
            assert_eq!(calls, vec![missing]);
            assert!(matches!(rx.try_recv(), Ok(GridEvent::Backfill { start_ms, .. }) if start_ms == missing.timestamp_millis()));
            calls.clear();
            assert!(refresh_frames(&tx, &mut known, stamps, |stamp| {
                calls.push(stamp);
                std::future::ready(Ok(loaded(stamp)))
            }).await);
            assert!(calls.is_empty(), "published frames must not be fetched again");
        });
    }

    #[test]
    fn newest_failure_reports_offline_and_remains_retryable() {
        runtime().block_on(async {
            let stamps = test_stamps();
            let mut known = HashSet::new();
            let mut calls = vec![];
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            for _ in 0..2 {
                assert!(refresh_frames(&tx, &mut known, stamps.clone(), |stamp| {
                    calls.push(stamp);
                    std::future::ready(Err("fetch failed".into()))
                }).await);
                assert!(matches!(rx.try_recv(), Ok(GridEvent::Offline { source_id, .. }) if source_id == ID));
                assert!(known.is_empty());
            }
            assert_eq!(calls, vec![stamps[0], stamps[0]]);
        });
    }

    #[test]
    fn aborting_refresh_drops_download_and_does_not_start_more_history() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        runtime().block_on(async {
            let dropped = Arc::new(AtomicBool::new(false));
            let calls = Arc::new(AtomicUsize::new(0));
            let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(1);
            let (tx, _rx) = tokio::sync::mpsc::channel(8);
            let task = tokio::spawn({
                let dropped = dropped.clone();
                let calls = calls.clone();
                async move {
                    let stamps = test_stamps();
                    let mut known = HashSet::from([frame_id(stamps[0])]);
                    refresh_frames(&tx, &mut known, stamps, |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let guard = Dropped(dropped.clone());
                        let started = started_tx.clone();
                        async move {
                            let _guard = guard;
                            started.send(()).await.unwrap();
                            std::future::pending().await
                        }
                    })
                    .await;
                }
            });
            tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
                .await
                .unwrap()
                .unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert!(dropped.load(Ordering::SeqCst));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn synthetic_palette_decodes_every_class_and_missing() {
        let mut raw: Vec<_> = COLORS
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect();
        raw.extend([99, 98, 97, 0]);
        let input = sweep::png(15, 1, &raw).unwrap();
        let result = decode(&input, 15, 1).unwrap();
        let mut reader = png::Decoder::new(Cursor::new(result)).read_info().unwrap();
        let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut rgba).unwrap();
        for i in 0..14 {
            assert_eq!(&rgba[i * 4..i * 4 + 4], &[i as u8 + 1, 0, 0, 255]);
        }
        assert_eq!(&rgba[56..], &[0, 8, 0, 255]);
        assert!(decode(&input, 16, 1).is_err());
        assert!(decode(b"<ServiceException/>", 15, 1).is_err());
        for pixel in [[1, 2, 3, 255], [153, 204, 255, 128]] {
            assert!(decode(&sweep::png(1, 1, &pixel).unwrap(), 1, 1).is_err());
        }
    }
    #[test]
    fn wire_preserves_rate_units_and_georeference() {
        let f = frame_template();
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["units"], "mm/h");
        assert_eq!(json["productName"], "Rain rate estimate");
        assert_eq!(json["bounds"][0], 0.1);
        assert!(json.get("elevationDeg").is_none());
        assert_eq!(f.bounds.len(), f.palette.len() + 1);
        let [_, south, east, _] = default_region().bbox();
        assert!((f.geotransform[0] + WIDTH as f64 * f.geotransform[1] - east).abs() < 1e-9);
        assert!((f.geotransform[3] + HEIGHT as f64 * f.geotransform[5] - south).abs() < 1e-9);
        let stamp = DateTime::parse_from_rfc3339("2026-09-25T17:24:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let url = map_url(stamp, default_region(), false);
        assert!(url.contains("VERSION=1.1.1"));
        assert!(url.contains("SRS=EPSG:3857"));
        assert!(!url.contains("INTERPOLATION"));
        assert!(url.contains("TIME=2026-09-25T17:24:00Z"));
    }
    #[test]
    #[ignore = "manual live GeoMet verification"]
    fn live_frame_decodes() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let body = get(
                    format!(
                        "{HOST}?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetCapabilities&LAYERS={LAYER}"
                    ),
                    1 << 20,
                )
                .await
                .unwrap();
                let stamps = timestamps(&body).unwrap();
                let (frame, _) = load(
                    stamps[0],
                    default_region(),
                    std::sync::Arc::new(Semaphore::new(1)),
                )
                .await
                .unwrap();
                assert_eq!(frame.status, FrameStatus::Complete);
                assert!(!frame.texture.is_empty());
            });
    }
}
