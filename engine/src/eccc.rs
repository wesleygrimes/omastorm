//! ECCC GeoMet rain-rate classes. This deliberately decodes the discrete
//! WMS style, not reflectivity or raw measurements (issue #118).
use crate::{
    live_index,
    protocol::{Coverage, Crs, Ellipsoid, Family, FrameStatus, Kind, MosaicFrame, ProductClass},
    source::{GridEvent, MosaicMeta, SourceMetadataBorrowed},
    sweep,
};
use chrono::{DateTime, SecondsFormat, Utc};
use std::{collections::HashSet, future::Future, io::Cursor, time::Duration};
use tokio::{sync::mpsc::Sender, task::JoinHandle};
use xml::reader::{EventReader, XmlEvent};

pub const ID: &str = "eccc";
const NAME: &str = "Canada ECCC radar";
const HOST: &str = "https://geo.weather.gc.ca/geomet/";
const LAYER: &str = "RADAR_1KM_RRAI";
const STYLE: &str = "Radar-Rain_Dis-14colors";
pub const HISTORY_MAX: usize = 30;
const WIDTH: u32 = 4096;
const HEIGHT: u32 = 1711;
const BODY_MAX: usize = 32 << 20;
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
}
impl Eccc {
    pub fn new() -> Self {
        let b = crate::envelope::ECCC;
        Self {
            id: ID,
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
            attribution: "Environment and Climate Change Canada",
            mosaic: Some(MosaicMeta {
                coverage: &self.coverage,
                selection_priority: 20,
                covering: true,
            }),
        }
    }
    pub fn loading_placeholder(&self) -> Option<(MosaicFrame, Vec<u8>)> {
        let mut frame = frame_template();
        frame.width = 1;
        frame.height = 1;
        Some((frame, sweep::png(1, 1, &[0, 1, 0, 255]).ok()?))
    }
    pub fn poll(&self, events: Sender<GridEvent>, known: HashSet<String>) -> JoinHandle<()> {
        tokio::spawn(poll_loop(events, known))
    }
}
fn frame_template() -> MosaicFrame {
    let b = crate::envelope::ECCC;
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
        crs: Crs::Geographic {
            ellipsoid: Ellipsoid::WGS84,
            datum_transform: None,
        },
        geotransform: [
            b.west,
            (b.east - b.west) / WIDTH as f64,
            0.,
            b.north,
            0.,
            (b.south - b.north) / HEIGHT as f64,
        ],
        palette: COLORS
            .iter()
            .map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]))
            .collect(),
        bounds: BOUNDS.to_vec(),
    }
}
fn frame_id(stamp: DateTime<Utc>) -> String {
    format!("{ID}-{}", stamp.format("%Y%m%dT%H%M%SZ"))
}

/// Read only this layer's observed time dimension. Never invent timestamps
/// from the wall clock or accept forecast layers / silently changed cadence.
fn timestamps(body: &[u8]) -> Result<Vec<DateTime<Utc>>, String> {
    let mut layer_names = Vec::<String>::new();
    let mut elements = Vec::<String>::new();
    let mut in_name = false;
    let mut in_time = false;
    let mut dimension = String::new();
    for event in EventReader::new(body) {
        match event.map_err(|e| format!("ECCC capabilities: {e}"))? {
            XmlEvent::StartElement {
                name, attributes, ..
            } => {
                let layer_child = elements.last().is_some_and(|parent| parent == "Layer");
                match name.local_name.as_str() {
                    "Layer" => layer_names.push(String::new()),
                    "Name" => in_name = layer_child,
                    "Dimension" => {
                        in_time = layer_child
                            && layer_names.last().is_some_and(|n| n.trim() == LAYER)
                            && attributes
                                .iter()
                                .any(|a| a.name.local_name == "name" && a.value == "time");
                    }
                    _ => {}
                }
                elements.push(name.local_name);
            }
            XmlEvent::Characters(s) | XmlEvent::CData(s) => {
                if in_name && let Some(n) = layer_names.last_mut() {
                    n.push_str(&s);
                }
                if in_time {
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

fn map_url(stamp: DateTime<Utc>) -> String {
    let b = crate::envelope::ECCC;
    // WMS 1.1.1 fixes EPSG:4326 axis order as longitude, latitude.
    format!(
        "{HOST}?SERVICE=WMS&VERSION=1.1.1&REQUEST=GetMap&LAYERS={LAYER}&STYLES={STYLE}&SRS=EPSG:4326&BBOX={},{},{},{}&WIDTH={WIDTH}&HEIGHT={HEIGHT}&FORMAT=image/png&TRANSPARENT=TRUE&TIME={}",
        b.west,
        b.south,
        b.east,
        b.north,
        stamp.to_rfc3339_opts(SecondsFormat::Secs, true)
    )
}

fn decode(body: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut decoder = png::Decoder::new(Cursor::new(body));
    decoder.set_transformations(png::Transformations::EXPAND);
    decoder.set_limits(png::Limits { bytes: BODY_MAX });
    let mut reader = decoder.read_info().map_err(|e| format!("ECCC PNG: {e}"))?;
    let info = reader.info();
    if info.width != width || info.height != height || info.bit_depth != png::BitDepth::Eight {
        return Err("ECCC unexpected PNG dimensions or bit depth".into());
    }
    let size = reader
        .output_buffer_size()
        .filter(|n| *n <= BODY_MAX)
        .ok_or("ECCC PNG too large")?;
    let mut pixels = vec![0; size];
    let output = reader
        .next_frame(&mut pixels)
        .map_err(|e| format!("ECCC PNG: {e}"))?;
    if output.color_type != png::ColorType::Rgba {
        return Err("ECCC PNG must include transparency".into());
    }
    for pixel in pixels[..output.buffer_size()].as_chunks_mut::<4>().0 {
        if pixel[3] == 0 {
            // Transparent means no reported value, not a measured zero.
            pixel.copy_from_slice(&[0, 1, 0, 255]);
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
    sweep::png(width, height, &pixels[..output.buffer_size()])
        .map_err(|e| format!("ECCC texture: {e}"))
}

async fn get(url: String, cap: usize) -> Result<Vec<u8>, String> {
    let response = live_index::http_client()
        .get(url)
        .header(reqwest::header::USER_AGENT, "omastorm/Canada-radar")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    live_index::take_body(response, cap)
        .await
        .map_err(|e| e.to_string())
}
async fn load(stamp: DateTime<Utc>) -> Result<(MosaicFrame, Vec<u8>), String> {
    let body = get(map_url(stamp), BODY_MAX).await?;
    let texture = tokio::task::spawn_blocking(move || decode(&body, WIDTH, HEIGHT))
        .await
        .map_err(|e| e.to_string())??;
    let mut frame = frame_template();
    frame.id = frame_id(stamp);
    frame.scan_time = stamp.to_rfc3339_opts(SecondsFormat::Secs, true);
    frame.status = FrameStatus::Complete;
    Ok((frame, texture))
}
async fn poll_loop(events: Sender<GridEvent>, mut known: HashSet<String>) {
    loop {
        let listing = get(
            format!("{HOST}?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetCapabilities&LAYERS={LAYER}"),
            1 << 20,
        )
        .await;
        let stamps = match listing.and_then(|b| timestamps(&b)) {
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
        if !refresh_frames(&events, &mut known, stamps, load).await {
            return;
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// One refresh, with an injectable loader for failure/retry/cancellation tests.
/// Newest-frame failure reports offline; a missing historical frame must not
/// prevent other history from loading. Only published frames become known.
/// Returns false when the event receiver has closed.
async fn refresh_frames<L, F>(
    events: &Sender<GridEvent>,
    known: &mut HashSet<String>,
    stamps: Vec<DateTime<Utc>>,
    mut load: L,
) -> bool
where
    L: FnMut(DateTime<Utc>) -> F,
    F: Future<Output = Result<(MosaicFrame, Vec<u8>), String>>,
{
    let ids: HashSet<_> = stamps.iter().copied().map(frame_id).collect();
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
        let id = frame_id(stamp);
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
mod tests {
    use super::*;
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
        assert_eq!(&rgba[56..], &[0, 1, 0, 255]);
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
        let b = crate::envelope::ECCC;
        assert!((f.geotransform[0] + WIDTH as f64 * f.geotransform[1] - b.east).abs() < 1e-9);
        assert!((f.geotransform[3] + HEIGHT as f64 * f.geotransform[5] - b.south).abs() < 1e-9);
        let stamp = DateTime::parse_from_rfc3339("2026-09-25T17:24:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let url = map_url(stamp);
        assert!(url.contains("VERSION=1.1.1"));
        assert!(url.contains("BBOX=-170.32,16.93,-50,67.19"));
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
                let (frame, texture) = load(stamps[0]).await.unwrap();
                assert_eq!(frame.status, FrameStatus::Complete);
                assert!(!texture.is_empty());
            });
    }
}
