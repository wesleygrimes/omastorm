//! DWD DX single-site product (`docs/protocol.md`, polar radar path):
//! decode the German Weather Service DX sweep and poll it live, without
//! new dependencies.
//!
//! DX is a 5-minute single-sweep product per radar (`sweep 0.8°`, 360 rays of
//! 128 1 km gates, RLE-packed uint16 words after an ASCII header), fetched
//! over plain HTTPS from `opendata.dwd.de`. It maps onto `Sweep` by
//! converting dBZ into NEXRAD-style moment codes (`scale` 2.0, `offset`
//! 66.0): undetect (`-32.5` dBZ, raw data 0) becomes code 0
//! (below threshold), measured gates become 2..=255. DX carries no
//! range-folding flag, so code 1 never appears.
//!
//! Unlike the NEXRAD chunk stream, each DX file is one complete sweep, so
//! the poller only ever reports complete frames: a backfill of the newest
//! catalogued-missing files on join, then the `-latest-` file every minute.

use crate::live::Event;
use crate::sweep::{Ray, Sweep};

/// DX geometry: one ray per degree, 128 one-kilometre gates.
pub const RAYS: usize = 360;
pub const GATES: usize = 128;
/// Gate geometry the shader consumes as uniforms (`docs/protocol.md`).
pub const FIRST_GATE_M: u32 = 0;
pub const GATE_SPACING_M: u32 = 1000;
/// NEXRAD-style code mapping: dBZ = (code - offset) / scale.
pub const SCALE: f32 = 2.0;
pub const OFFSET: f32 = 66.0;
/// dBZ value the format assigns to undetect (raw data part 0).
pub const UNDETECT_DBZ: f32 = -32.5;
/// Ray marker word (wradlib's bit 14) and zero-run flag (wradlib's bit 13;
/// both counted from 1, so `1 << 13` and `1 << 12`) in the body.
const RAY_MARK: u16 = 1 << 13;
const ZERO_FLAG: u16 = 1 << 12;
const DATA_MASK: u16 = (1 << 13) - 1;
/// A zero run carries its count in bits 0..11 (wradlib: `data = 4095`).
const RUN_MASK: u16 = (1 << 12) - 1;
const CLUTTER_FLAG: u16 = 1 << 15;

/// One decoded DX file: the volume scan time plus a polar grid of raw gate
/// words (clutter flag in bit 15, data in bits 0..12).
#[allow(dead_code, reason = "spike: clutter overlay not wired yet")]
pub struct DxSweep {
    pub radar_id: String,
    /// Volume scan time, milliseconds since the Unix epoch (UTC).
    pub time_ms: i64,
    pub azimuths_deg: Vec<f32>,
    pub elevations_deg: Vec<f32>,
    pub words: Vec<Vec<u16>>,
    pub clutter: Vec<Vec<bool>>,
}

/// dBZ of one raw gate word: `(word & 0x1FFF) * 0.5 - 32.5`.
pub fn dbz_of(word: u16) -> f32 {
    f32::from(word & DATA_MASK) * 0.5 - 32.5
}

/// NEXRAD-style moment code for a gate: 0 when undetect, otherwise
/// `round(dbz * 2 + 66)` clamped to 2..=255.
pub fn code_of(word: u16) -> u8 {
    if dbz_of(word) == UNDETECT_DBZ {
        return 0;
    }
    (dbz_of(word) * SCALE + OFFSET).round().clamp(2.0, 255.0) as u8
}

/// Decode one DX product file (`read_dx` in wradlib, same word layout).
pub fn decode_dx(bytes: &[u8]) -> Result<DxSweep, String> {
    let header_end = header_len(bytes)?;
    let header = std::str::from_utf8(&bytes[..header_end])
        .map_err(|e| format!("DX header is not ASCII: {e}"))?;
    let header_text = header.trim_end_matches('\x03');
    let (radar_id, time_ms) = parse_header_fixed(header_text)?;
    let mut body = &bytes[header_end..];
    // The declared length is occasionally off by one (`bytes` in wradlib's
    // attributes); drop a trailing byte rather than fail the whole sweep.
    if !body.len().is_multiple_of(2) {
        body = &body[..body.len() - 1];
    }
    let words: Vec<u16> = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|w| u16::from_le_bytes(*w))
        .collect();
    let mut azimuths_deg = Vec::new();
    let mut elevations_deg = Vec::new();
    let mut rays: Vec<Vec<u16>> = Vec::new();
    let mut clutter: Vec<Vec<bool>> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if words[i] != RAY_MARK {
            return Err(format!(
                "DX ray {} starts with {:04X}, not the ray marker",
                rays.len(),
                words[i]
            ));
        }
        if i + 2 >= words.len() {
            return Err("DX ray header is truncated".into());
        }
        azimuths_deg.push(f32::from(words[i + 1] & DATA_MASK) / 10.0);
        elevations_deg.push(f32::from(words[i + 2] & DATA_MASK) / 10.0);
        i += 3;
        let mut ray = Vec::new();
        let mut mask = Vec::new();
        while i < words.len() && words[i] != RAY_MARK {
            let word = words[i];
            if word & ZERO_FLAG != 0 {
                let run = usize::from(word & RUN_MASK);
                ray.resize(ray.len() + run, 0);
                mask.resize(mask.len() + run, false);
            } else {
                ray.push(word);
                mask.push(word & CLUTTER_FLAG != 0);
            }
            i += 1;
        }
        rays.push(ray);
        clutter.push(mask);
    }
    if rays.is_empty() {
        return Err("DX file holds no rays".into());
    }
    // Real files are 360 rays of 128 gates, but the format does not
    // guarantee it (wradlib notes 361-beam files in the wild), so require
    // only a rectangular grid; the azimuth lookup handles any ray count.
    let gates = rays[0].len();
    if let Some((n, len)) = rays
        .iter()
        .enumerate()
        .map(|(n, ray)| (n, ray.len()))
        .find(|&(_, len)| len != gates)
    {
        return Err(format!(
            "DX rays disagree on gate count: ray 0 has {gates} gates, ray {n} has {len}"
        ));
    }
    Ok(DxSweep {
        radar_id,
        time_ms,
        azimuths_deg,
        elevations_deg,
        words: rays,
        clutter,
    })
}

/// Length of the ASCII header including its 0x03 terminator(s).
fn header_len(bytes: &[u8]) -> Result<usize, String> {
    let end = bytes
        .iter()
        .position(|&b| b == 0x03)
        .ok_or("DX file has no header terminator")?;
    Ok(if bytes.get(end + 1) == Some(&0x03) {
        end + 2
    } else {
        end + 1
    })
}

/// Radar id (`header[8:13]`) and UTC scan time from `DDHHMM` + `MMyy`.
fn parse_header_fixed(header: &str) -> Result<(String, i64), String> {
    if !header.starts_with("DX") || header.len() < 17 {
        return Err("DX header is too short for the fixed fields".into());
    }
    let digits = |range: std::ops::Range<usize>, what: &str| {
        header[range]
            .parse::<u32>()
            .map_err(|_| format!("DX header {what} is not numeric"))
    };
    let day = digits(2..4, "day")?;
    let hour = digits(4..6, "hour")?;
    let minute = digits(6..8, "minute")?;
    let radar_id = header[8..13].to_owned();
    let month = digits(13..15, "month")?;
    let year = 2000 + digits(15..17, "year")?;
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || !(1..=12).contains(&month) {
        return Err("DX header time fields are out of range".into());
    }
    Ok((radar_id, unix_ms(year, month, day, hour, minute)))
}

/// Milliseconds since the Unix epoch for a UTC calendar time (Howard
/// Hinnant's `days_from_civil`, no date library needed here).
fn unix_ms(year: u32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
    let y = year as i64 - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    ((days * 24 + i64::from(hour)) * 60 + i64::from(minute)) * 60_000
}

impl DxSweep {
    /// The sweep in the engine's polar texture encoding: rays in ascending
    /// azimuth order (a stable sort, as `Sweep::from_radials` does), one
    /// volume time for every ray since DX names no per-ray times.
    pub fn to_sweep(&self) -> Result<Sweep, String> {
        let mut order: Vec<usize> = (0..self.words.len()).collect();
        order.sort_by(|&a, &b| {
            self.azimuths_deg[a]
                .total_cmp(&self.azimuths_deg[b])
                .then_with(|| a.cmp(&b))
        });
        let rays = order
            .into_iter()
            .map(|i| Ray {
                azimuth_deg: self.azimuths_deg[i],
                elevation_deg: self.elevations_deg[i],
                time_ms: self.time_ms,
                codes: self.words[i].iter().map(|&w| code_of(w)).collect(),
            })
            .collect();
        let gates = self.words[0].len();
        if gates > u16::MAX as usize {
            return Err("DX ray is wider than u16 gates".into());
        }
        Ok(Sweep {
            rays,
            start_ms: self.time_ms,
            end_ms: self.time_ms,
            gates: gates as u16,
            first_gate_m: FIRST_GATE_M,
            gate_spacing_m: GATE_SPACING_M,
            scale: SCALE,
            offset: OFFSET,
        })
    }
}

/// The 17 DWD radar sites: uppercase id to WMO number, mirroring
/// `engine/data/dwd_sites.json` (a unit test keeps the two in step).
pub const SITES: [(&str, u32); 17] = [
    ("ASB", 10103),
    ("BOO", 10132),
    ("DRS", 10488),
    ("EIS", 10780),
    ("ESS", 10410),
    ("FBG", 10908),
    ("FLD", 10440),
    ("HNR", 10339),
    ("ISN", 10873),
    ("MEM", 10950),
    ("NEU", 10557),
    ("NHB", 10605),
    ("OFT", 10629),
    ("PRO", 10392),
    ("ROS", 10169),
    ("TUR", 10832),
    ("UMD", 10356),
];

/// WMO number for a DWD station id (`None` for NEXRAD ids).
pub fn wmo_of(id: &str) -> Option<u32> {
    SITES
        .iter()
        .find(|&&(code, _)| code == id)
        .map(|&(_, wmo)| wmo)
}

/// Directory of one site's DX files; DWD spells the site lowercase.
pub fn site_url(id: &str) -> String {
    format!("{BASE}/{}", id.to_lowercase())
}

/// The rolling `-latest-` file: `raa00-dx_<wmo>-latest-<site>---bin`.
pub fn latest_name(id: &str, wmo: u32) -> String {
    format!("raa00-dx_{wmo}-latest-{}---bin", id.to_lowercase())
}

/// Dated files sort chronologically by name (`yyMMddHHmm` is fixed width).
pub fn parse_listing(html: &str) -> Vec<String> {
    let mut names: Vec<String> = html
        .split('"')
        .filter(|token| {
            token.starts_with("raa00-dx_") && token.ends_with("---bin") && !token.contains("latest")
        })
        .map(str::to_owned)
        .collect();
    names.sort();
    names.dedup();
    names
}

const BASE: &str = "https://opendata.dwd.de/weather/radar/sites/dx";
/// DX files land every 5 minutes; poll the `-latest-` file this often.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Wait before the backfill so a hand-off passed while panning costs
/// nothing (`live.rs` gives its backfill the same delay).
const BACKFILL_DELAY: std::time::Duration = std::time::Duration::from_secs(3);
/// Earlier files fetched on joining a station, newest first area covered
/// by the same count as the NEXRAD backfill.
const BACKFILL_FILES: usize = 12;
/// Report `offline` from the second consecutive failure, like `live.rs`.
const OFFLINE_AFTER: u32 = 2;
const BACK_OFF: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_BACK_OFF: std::time::Duration = std::time::Duration::from_secs(60);

fn dwd_log(site: &str, message: impl std::fmt::Display) {
    eprintln!(
        "{} Dwd {site}: {message}",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    );
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|body| body.to_vec())
        .map_err(|e| format!("reading {url}: {e}"))
}

fn decode_named(site: &str, name: &str, bytes: &[u8]) -> Result<Sweep, String> {
    let dx = decode_dx(bytes).map_err(|e| format!("{name}: {e}"))?;
    if dx.words.len() != RAYS || dx.words[0].len() != GATES {
        return Err(format!(
            "{name}: {}x{} grid, {RAYS}x{GATES} expected",
            dx.words.len(),
            dx.words[0].len()
        ));
    }
    dwd_log(site, format_args!("decoded {name}"));
    dx.to_sweep()
}

/// Fetch the newest `BACKFILL_FILES` dated files missing from `known`
/// and report each as `Event::Backfill`, newest first. Delivered start
/// times join `known` so the live loop never republishes them.
async fn backfill(
    client: &reqwest::Client,
    site: &str,
    events: &tokio::sync::mpsc::Sender<Event>,
    known: &mut Vec<i64>,
) -> bool {
    let listing = match fetch(client, &site_url(site)).await {
        Ok(html) => String::from_utf8_lossy(&html).into_owned(),
        Err(e) => {
            dwd_log(site, format_args!("backfill listing: {e}"));
            return true;
        }
    };
    let names = parse_listing(&listing);
    let window = &names[names.len().saturating_sub(BACKFILL_FILES)..];
    for name in window.iter().rev() {
        let url = format!("{}/{name}", site_url(site));
        let bytes = match fetch(client, &url).await {
            Ok(bytes) => bytes,
            Err(e) => {
                dwd_log(site, format_args!("backfill {name}: {e}"));
                continue;
            }
        };
        let sweep = match decode_named(site, name, &bytes) {
            Ok(sweep) => sweep,
            Err(e) => {
                dwd_log(site, format_args!("{e}"));
                continue;
            }
        };
        if known.contains(&sweep.start_ms) {
            continue;
        }
        known.push(sweep.start_ms);
        let provenance = format!("dwd-dx/{site}/{name}");
        if events
            .send(Event::Backfill {
                site: site.to_owned(),
                sweep,
                provenance,
            })
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}

/// Poll a DWD station until the task is aborted or the event channel
/// closes. `cached` holds the start times already catalogued, so neither
/// the backfill nor a respawned poller republishes them.
pub async fn poll(
    site: String,
    wmo: u32,
    events: tokio::sync::mpsc::Sender<Event>,
    cached: Vec<i64>,
) {
    let client = match reqwest::Client::builder().timeout(CALL_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => {
            dwd_log(&site, format_args!("HTTP client: {e}"));
            let _ = events
                .send(Event::Offline {
                    site,
                    reason: "HTTP client failed to build".into(),
                })
                .await;
            return;
        }
    };
    tokio::time::sleep(BACKFILL_DELAY).await;
    let mut known = cached;
    if !backfill(&client, &site, &events, &mut known).await {
        return;
    }
    let latest = format!("{}/{}", site_url(&site), latest_name(&site, wmo));
    let mut failures = 0;
    let mut back_off = BACK_OFF;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let bytes = match fetch(&client, &latest).await {
            Ok(bytes) => bytes,
            Err(e) => {
                failures += 1;
                if failures >= OFFLINE_AFTER
                    && events
                        .send(Event::Offline {
                            site: site.clone(),
                            reason: e.clone(),
                        })
                        .await
                        .is_err()
                {
                    return;
                }
                dwd_log(&site, format_args!("{e}; retrying"));
                tokio::time::sleep(back_off).await;
                back_off = (back_off * 2).min(MAX_BACK_OFF);
                continue;
            }
        };
        failures = 0;
        back_off = BACK_OFF;
        let sweep = match decode_named(&site, "latest", &bytes) {
            Ok(sweep) => sweep,
            Err(e) => {
                dwd_log(&site, format_args!("{e}"));
                continue;
            }
        };
        if known.contains(&sweep.start_ms) {
            continue;
        }
        known.push(sweep.start_ms);
        let provenance = format!("dwd-dx/{site}/latest");
        if events
            .send(Event::Sweep {
                site: site.clone(),
                sweep,
                complete: true,
                provenance,
            })
            .await
            .is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal synthetic DX file: header plus two rays (azimuths 1.0° and
    /// 0.0° to prove the ascending sort), each four gates: a zero run, a
    /// measured gate, and a clutter-flagged gate.
    fn synthetic() -> Vec<u8> {
        let mut bytes =
            b"DX121040101320926BY0000VS 2CO0CD4CS0EP0.80.80.80.80.80.80.80.8MS  0\x03\x03".to_vec();
        let mut words: Vec<u16> = Vec::new();
        // Ray 1.0°: az 10, el 8 (0.8°); run of 2 zeros, data 65 (-0.0 dBZ),
        // clutter-flagged data 65.
        words.extend([RAY_MARK, 10, 8, 0x1000 | 2, 65, CLUTTER_FLAG | 65]);
        // Ray 0.0°: az 0, el 8; data 100 (17.5 dBZ), then a run of 3 zeros.
        words.extend([RAY_MARK, 0, 8, 100, 0x1000 | 3]);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn header_time_and_rays_decode() {
        let sweep = decode_dx(&synthetic()).unwrap();
        assert_eq!(sweep.radar_id, "10132");
        // 2026-09-12T10:40:00Z.
        assert_eq!(sweep.time_ms, 1_789_209_600_000);
        assert_eq!(sweep.azimuths_deg, [1.0, 0.0]);
        assert_eq!(sweep.elevations_deg, [0.8, 0.8]);
        assert_eq!(sweep.words.len(), 2);
        assert_eq!(sweep.words[0].len(), 4);
        assert_eq!(sweep.words[1].len(), 4);
    }

    #[test]
    fn codes_follow_the_nexrad_mapping() {
        assert_eq!(code_of(0), 0, "undetect is below threshold");
        assert_eq!(dbz_of(0), UNDETECT_DBZ);
        // 17.5 dBZ -> round(17.5 * 2 + 66) = 101.
        assert_eq!(code_of(100), 101);
        // Clutter flag does not change the value bits.
        assert_eq!(code_of(CLUTTER_FLAG | 100), 101);
        assert_eq!(code_of(0x1FFF), 255, "clamp the top");
    }

    #[test]
    fn sweep_sorts_rays_and_shares_the_palette() {
        let sweep = decode_dx(&synthetic()).unwrap().to_sweep().unwrap();
        assert_eq!(sweep.gates, 4);
        assert_eq!(sweep.first_gate_m, FIRST_GATE_M);
        assert_eq!(sweep.gate_spacing_m, GATE_SPACING_M);
        assert_eq!(sweep.scale, SCALE);
        assert_eq!(sweep.offset, OFFSET);
        // Ascending azimuth: the 0.0° ray first.
        assert_eq!(sweep.rays[0].azimuth_deg, 0.0);
        assert_eq!(sweep.rays[1].azimuth_deg, 1.0);
        assert_eq!(sweep.rays[0].codes, [code_of(100), 0, 0, 0]);
        assert_eq!(sweep.rays[1].codes, [0, 0, code_of(65), code_of(65)]);
        assert!((sweep.elevation_deg() - 0.8).abs() < 1e-6);
    }

    #[test]
    fn urls_name_the_site_and_wmo() {
        assert_eq!(
            site_url("BOO"),
            "https://opendata.dwd.de/weather/radar/sites/dx/boo"
        );
        assert_eq!(latest_name("BOO", 10132), "raa00-dx_10132-latest-boo---bin");
        assert_eq!(wmo_of("BOO"), Some(10132));
        assert_eq!(wmo_of("UMD"), Some(10356));
        assert_eq!(wmo_of("KTLX"), None);
    }

    #[test]
    fn listing_keeps_dated_files_newest_sortable() {
        let html = r#"<a href="raa00-dx_10132-2609121040-boo---bin">x</a>
<a href="raa00-dx_10132-latest-boo---bin">latest</a>
<a href="raa00-dx_10132-2609121035-boo---bin">x</a>"#;
        assert_eq!(
            parse_listing(html),
            [
                "raa00-dx_10132-2609121035-boo---bin",
                "raa00-dx_10132-2609121040-boo---bin",
            ]
        );
    }

    /// The `SITES` map and `dwd_sites.json` name the same 17 stations, all
    /// inside Germany, with no id shared with the NEXRAD table.
    #[test]
    fn station_table_and_wmo_map_agree() {
        use crate::protocol::SiteTable;
        let table: SiteTable =
            serde_json::from_str(include_str!("../data/dwd_sites.json")).unwrap();
        assert_eq!(table.sites.len(), SITES.len());
        let nexrad: SiteTable = serde_json::from_str(include_str!("../data/sites.json")).unwrap();
        for site in &table.sites {
            assert!(
                SITES.iter().any(|&(code, _)| code == site.id),
                "SITES has no WMO for {}",
                site.id
            );
            assert!(
                nexrad.sites.iter().all(|entry| entry.id != site.id),
                "{} collides with a NEXRAD id",
                site.id
            );
            assert!(
                (47.0..=55.5).contains(&site.lat) && (5.5..=15.5).contains(&site.lon),
                "{} is outside Germany",
                site.id
            );
            assert!(site.alt_m > 0.0);
        }
    }

    /// Real-file spike, ignored by default: `DWD_DX_SAMPLE` names a DX file
    /// (see /tmp/opencode/dwd/dx_sample.bin). Decodes it, checks it against
    /// wradlib's reading, and writes the polar texture PNG plus the raw code
    /// grid for cross-checking in Python.
    #[test]
    #[ignore]
    fn real_file_matches_wradlib_and_renders() {
        let path = std::env::var("DWD_DX_SAMPLE").expect("DWD_DX_SAMPLE names a DX file");
        let bytes = std::fs::read(path).unwrap();
        let sweep = decode_dx(&bytes).unwrap();
        assert_eq!(sweep.radar_id, "10132");
        assert_eq!(sweep.words.len(), 360, "one ray per degree");
        assert!(sweep.words.iter().all(|ray| ray.len() == GATES));
        let polar = sweep.to_sweep().unwrap();
        assert_eq!(polar.rows(), 360, "a complete 1° cut needs no blank row");
        let bounds = [-32, 0, 10, 20, 30, 40, 45, 50, 55, 60, 65, 70, 96];
        let pixels = polar.texture(&bounds, 12);
        let out = std::env::var("DWD_DX_OUTDIR").unwrap_or_else(|_| "/tmp/opencode/dwd".into());
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            format!("{out}/dx_render.png"),
            crate::sweep::png(u32::from(polar.gates), polar.rows(), &pixels).unwrap(),
        )
        .unwrap();
        let grid: Vec<u8> = polar
            .rays
            .iter()
            .flat_map(|ray| ray.codes.iter().copied())
            .collect();
        std::fs::write(format!("{out}/dx_grid_codes.bin"), grid).unwrap();
        let measured = polar
            .rays
            .iter()
            .flat_map(|ray| ray.codes.iter())
            .filter(|&&c| c >= 2)
            .count();
        eprintln!(
            "DX {}x{} el {:.1}° measured {measured} gates",
            polar.rays.len(),
            polar.gates,
            polar.elevation_deg()
        );
    }
}
