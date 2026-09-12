//! Level II decoding for the polar radar path (`docs/protocol.md`):
//! the lowest sweep of an Archive II volume as sorted rays of raw moment
//! codes, the RGBA sweep texture, and the azimuth lookup table.
//!
//! Only the first elevation cut is decoded. Records are decompressed one at a
//! time and decoding stops at the first radial of the next cut, which is the
//! shape live chunks take (`live.rs` assembles the same `Sweep` from the
//! radials of each chunk as it arrives) and keeps startup quick.
//!
//! A sweep whose rays leave a gap (a live sweep still being filled, or a
//! dropout) carries one extra blank row after the sorted rays, and the
//! lookup names it for every tenth-degree entry farther than `GAP_DEG` from
//! any ray, so unscanned azimuths draw nothing instead of smearing the
//! nearest ray around the circle (`docs/protocol.md`, sweep texture).

use crate::product::Product;
use nexrad_data::volume::{File, Record};
use nexrad_model::data::{DataMoment, MomentData, Radial};

/// An azimuth farther than this from every ray has not been scanned. Half a
/// degree of spacing puts every entry within 0.25° of a ray; a 1° cut within
/// 0.5°; a single missing 0.5° ray still shows its neighbours.
pub const GAP_DEG: f32 = 0.75;

/// One radial of the sweep. `codes` are the raw Level II moment bytes:
/// 0 below threshold, 1 range folded, 2..255 measured.
#[allow(
    dead_code,
    reason = "times are checked against the golden files; the timeline session reads them"
)]
pub struct Ray {
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    /// Collection time, milliseconds since the Unix epoch.
    pub time_ms: i64,
    pub codes: Vec<u8>,
}

/// The lowest sweep of one product with its rays in ascending azimuth order
/// (a stable sort of decoded order, as the golden files were produced).
pub struct Sweep {
    pub rays: Vec<Ray>,
    /// Collection time of the first and last radial in decoded order,
    /// milliseconds since the Unix epoch; the cut's start and end.
    pub start_ms: i64,
    pub end_ms: i64,
    pub gates: u16,
    pub first_gate_m: u32,
    pub gate_spacing_m: u32,
    /// Measured value = (code - offset) / scale.
    pub scale: f32,
    pub offset: f32,
}

fn moment_of(product: Product, radial: &Radial) -> Option<&MomentData> {
    match product {
        Product::Reflectivity => radial.reflectivity(),
        Product::Velocity => radial.velocity(),
    }
}

/// Decode the given moment of the lowest elevation that carries it. Super-res
/// VCPs split the lowest angle: cut 1 is reflectivity-only, cut 2 is Doppler.
pub fn lowest(archive: &[u8], product: Product) -> Result<Sweep, String> {
    let file = File::new(archive.to_vec())
        .decompress()
        .map_err(|e| format!("inflating volume: {e}"))?;
    let mut cut: Vec<Radial> = Vec::new();
    let mut elev: Option<u8> = None;
    let mut last_err = format!("volume holds no {}", product.name().to_ascii_lowercase());
    for record in file
        .records()
        .map_err(|e| format!("splitting records: {e}"))?
    {
        let record = if record.compressed() {
            record
                .decompress()
                .map_err(|e| format!("decompressing record: {e}"))?
        } else {
            Record::new(record.data().to_vec())
        };
        for radial in record
            .radials()
            .map_err(|e| format!("decoding record: {e}"))?
        {
            if elev.is_some_and(|e| e != radial.elevation_number()) {
                match Sweep::from_radials(&cut, product) {
                    Ok(sweep) => return Ok(sweep),
                    Err(e) => {
                        last_err = e;
                        if product == Product::Reflectivity {
                            return Err(last_err);
                        }
                    }
                }
                cut.clear();
            }
            elev = Some(radial.elevation_number());
            cut.push(radial);
        }
    }
    Sweep::from_radials(&cut, product).map_err(|_| last_err)
}

/// Decode the reflectivity of the first elevation cut in `archive`.
pub fn lowest_reflectivity(archive: &[u8]) -> Result<Sweep, String> {
    lowest(archive, Product::Reflectivity)
}

impl Sweep {
    /// One moment of `radials`, one cut in decoded order, as a sweep.
    /// Radials that do not carry the moment are skipped so a velocity cut
    /// can still draw when only some rays have VEL. The first radial that
    /// carries the moment sets gate geometry; later radials that change it
    /// are an error.
    pub fn from_radials(radials: &[Radial], product: Product) -> Result<Sweep, String> {
        let Some(first) = radials
            .iter()
            .find(|radial| moment_of(product, radial).is_some())
        else {
            return Err(format!(
                "cut carries no {}",
                product.name().to_ascii_lowercase()
            ));
        };
        let moment = moment_of(product, first).expect("checked");
        if moment.data_word_size() != 8 {
            return Err(format!(
                "{} uses {}-bit words; 8 expected",
                product.name().to_ascii_lowercase(),
                moment.data_word_size()
            ));
        }
        let gates = moment.gate_count();
        let first_gate_m = meters(moment.first_gate_range_km());
        let gate_spacing_m = meters(moment.gate_interval_km());
        let (scale, offset) = (moment.scale(), moment.offset());
        let start_ms = first.collection_timestamp();
        let end_ms = radials
            .last()
            .map_or(start_ms, Radial::collection_timestamp);
        let mut rays = Vec::with_capacity(radials.len());
        for radial in radials {
            let Some(moment) = moment_of(product, radial) else {
                continue;
            };
            if moment.data_word_size() != 8 {
                continue;
            }
            let same_geometry = moment.gate_count() == gates
                && meters(moment.first_gate_range_km()) == first_gate_m
                && meters(moment.gate_interval_km()) == gate_spacing_m
                && moment.scale() == scale
                && moment.offset() == offset
                && moment.raw_values().len() == usize::from(gates);
            if !same_geometry {
                return Err(format!(
                    "radial {} changes {} gate geometry within the cut",
                    radial.azimuth_number(),
                    product.name().to_ascii_lowercase()
                ));
            }
            rays.push(Ray {
                azimuth_deg: radial.azimuth_angle_degrees(),
                elevation_deg: radial.elevation_angle_degrees(),
                time_ms: radial.collection_timestamp(),
                codes: moment.raw_values().to_vec(),
            });
        }
        if rays.is_empty() {
            return Err(format!(
                "cut carries no {}",
                product.name().to_ascii_lowercase()
            ));
        }
        rays.sort_by(|a, b| a.azimuth_deg.total_cmp(&b.azimuth_deg));
        Ok(Sweep {
            rays,
            start_ms,
            end_ms,
            gates,
            first_gate_m,
            gate_spacing_m,
            scale,
            offset,
        })
    }

    /// The mean elevation of the rays, the cut's nominal angle.
    pub fn elevation_deg(&self) -> f64 {
        if self.rays.is_empty() {
            return 0.0;
        }
        let sum: f64 = self
            .rays
            .iter()
            .map(|ray| f64::from(ray.elevation_deg))
            .sum();
        sum / self.rays.len() as f64
    }

    /// The texture's height: the rays, plus the blank row when the lookup
    /// needs one.
    pub fn rows(&self) -> u32 {
        self.rays.len() as u32 + u32::from(self.lut_rows().1)
    }

    /// The row each tenth-degree entry names: the nearest ray, or the blank
    /// row past the last ray when no ray is within `GAP_DEG`. The flag says
    /// whether any entry needed the blank row.
    fn lut_rows(&self) -> (Vec<u16>, bool) {
        let azimuths: Vec<f32> = self.rays.iter().map(|ray| ray.azimuth_deg).collect();
        let n = azimuths.len();
        let mut blank = false;
        let rows = (0..3600u32)
            .map(|entry| {
                if n == 0 {
                    blank = true;
                    return 0;
                }
                let center = (entry as f32 + 0.5) / 10.0;
                let after = azimuths.partition_point(|&az| az <= center) % n;
                let before = (after + n - 1) % n;
                let distance = |row: usize| {
                    let d = (azimuths[row] - center).abs();
                    d.min(360.0 - d)
                };
                let (row, d) = if distance(before) <= distance(after) {
                    (before, distance(before))
                } else {
                    (after, distance(after))
                };
                if d > GAP_DEG {
                    blank = true;
                    n as u16
                } else {
                    row as u16
                }
            })
            .collect();
        (rows, blank)
    }
}

/// The model reports gate geometry in kilometers derived from whole meters;
/// undo that without trusting the float.
fn meters(km: f64) -> u32 {
    (km * 1000.0).round() as u32
}

/// Status bits in the texture's G channel (`docs/protocol.md`).
pub const FOLDED: u8 = 1;
pub const BELOW_THRESHOLD: u8 = 2;

impl Sweep {
    /// The palette class of a measured code: the band of `bounds` it falls
    /// in, clamped to the palette like the legacy grid did.
    fn class(&self, code: u8, bounds: &[i32], classes: usize) -> u8 {
        let value = (f32::from(code) - self.offset) / self.scale;
        let above = bounds.partition_point(|&b| b as f32 <= value);
        above.saturating_sub(1).min(classes - 1) as u8
    }

    /// RGBA pixels, width = gates, height = `rows()`: R class + 1 (0 draws
    /// nothing), G status bits, B raw code, A 255; the blank row, when the
    /// lookup needs one, is all zeros but A.
    pub fn texture(&self, bounds: &[i32], classes: usize) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(self.rows() as usize * usize::from(self.gates) * 4);
        for ray in &self.rays {
            for &code in &ray.codes {
                let (class, status) = match code {
                    0 => (0, BELOW_THRESHOLD),
                    1 => (0, FOLDED),
                    _ => (self.class(code, bounds, classes) + 1, 0),
                };
                pixels.extend_from_slice(&[class, status, code, 255]);
            }
        }
        if self.lut_rows().1 {
            for _ in 0..self.gates {
                pixels.extend_from_slice(&[0, 0, 0, 255]);
            }
        }
        pixels
    }

    /// RGBA pixels, 3600 × 1: entry `i` covers azimuth `i / 10` degrees and
    /// names the ray nearest its center as a little-endian 16-bit row in R, G,
    /// or the blank row when no ray is within `GAP_DEG`.
    pub fn azimuth_lut(&self) -> Vec<u8> {
        self.lut_rows()
            .0
            .into_iter()
            .flat_map(|row| [(row & 0xff) as u8, (row >> 8) as u8, 0, 255])
            .collect()
    }
}

/// Encode RGBA pixels as a PNG.
pub fn png(width: u32, height: u32, pixels: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    writer
        .write_image_data(pixels)
        .map_err(std::io::Error::other)?;
    writer.finish().map_err(std::io::Error::other)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use sha2::{Digest, Sha256};
    use std::{fs, time::Instant};

    const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../golden/ktlx-20130520/");
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../data/raw/KTLX20130520_201643_V06.gz"
    );

    /// `golden/<fixture>/sweep0.json` (`docs/protocol.md`, golden files).
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Golden {
        rays: usize,
        gates: u16,
        scale: f32,
        offset: f32,
        first_gate_m: f64,
        gate_spacing_m: f64,
        azimuth_deg: Vec<f64>,
        elevation_deg: Vec<f64>,
        ray_time_base: String,
        ray_time_ms: Vec<i64>,
        source_sha256: String,
        source_bytes: usize,
    }

    /// The golden angles were written with four decimals; compare ours the
    /// same way rather than through a tolerance.
    fn four(value: f64) -> String {
        format!("{value:.4}")
    }

    #[test]
    fn lowest_sweep_matches_the_pyart_golden_files_exactly() {
        let golden: Golden =
            serde_json::from_slice(&fs::read(format!("{GOLDEN}sweep0.json")).unwrap()).unwrap();
        let codes = fs::read(format!("{GOLDEN}sweep0.u8")).unwrap();
        let archive = fs::read(FIXTURE).unwrap();
        assert_eq!(archive.len(), golden.source_bytes);
        assert_eq!(
            format!("{:x}", Sha256::digest(&archive)),
            golden.source_sha256
        );
        let started = Instant::now();
        let sweep = lowest_reflectivity(&archive).unwrap();
        eprintln!("decoded lowest sweep in {:?}", started.elapsed());
        let velocity = lowest(&archive, Product::Velocity).expect("KTLX fixture velocity");
        assert!(!velocity.rays.is_empty());

        assert_eq!(sweep.rays.len(), golden.rays);
        assert_eq!(sweep.gates, golden.gates);
        assert_eq!(sweep.scale, golden.scale);
        assert_eq!(sweep.offset, golden.offset);
        assert_eq!(f64::from(sweep.first_gate_m), golden.first_gate_m);
        assert_eq!(f64::from(sweep.gate_spacing_m), golden.gate_spacing_m);
        // pyart counts ray times from the first decoded radial, and the golden
        // file names that instant to the second.
        assert_eq!(golden.ray_time_base, "2013-05-20T20:16:43Z");
        assert_eq!(sweep.start_ms.div_euclid(1000), 1_369_081_003);
        assert!(sweep.end_ms > sweep.start_ms);
        let base_ms = sweep.start_ms;
        for (row, ray) in sweep.rays.iter().enumerate() {
            assert_eq!(
                four(f64::from(ray.azimuth_deg)),
                four(golden.azimuth_deg[row]),
                "azimuth of row {row}"
            );
            assert_eq!(
                four(f64::from(ray.elevation_deg)),
                four(golden.elevation_deg[row]),
                "elevation of row {row}"
            );
            assert_eq!(
                ray.time_ms - base_ms,
                golden.ray_time_ms[row],
                "time of row {row}"
            );
            let gates = usize::from(sweep.gates);
            assert!(
                ray.codes == codes[row * gates..(row + 1) * gates],
                "moment bytes of row {row}"
            );
        }
        assert_eq!(codes.len(), golden.rays * usize::from(golden.gates));
    }

    #[test]
    fn texture_and_lut_follow_the_protocol() {
        let archive = fs::read(FIXTURE).unwrap();
        let sweep = lowest_reflectivity(&archive).unwrap();
        let bounds = [-32, 0, 10, 20, 30, 40, 45, 50, 55, 60, 65, 70, 96];
        let pixels = sweep.texture(&bounds, 12);
        let gates = usize::from(sweep.gates);
        assert_eq!(pixels.len(), sweep.rays.len() * gates * 4);
        let mut measured = 0;
        for (i, px) in pixels.as_chunks::<4>().0.iter().enumerate() {
            let code = sweep.rays[i / gates].codes[i % gates];
            assert_eq!(px[2], code);
            assert_eq!(px[3], 255);
            match code {
                0 => assert_eq!(px[..2], [0, BELOW_THRESHOLD]),
                1 => assert_eq!(px[..2], [0, FOLDED]),
                _ => {
                    measured += 1;
                    assert_eq!(px[1], 0);
                    let value = (f32::from(code) - 66.0) / 2.0;
                    let class = usize::from(px[0] - 1);
                    assert!(bounds[class] as f32 <= value && value < bounds[class + 1] as f32);
                }
            }
        }
        // `docs/protocol.md`: 195,199 measured gates, none range folded.
        assert_eq!(measured, 195_199);
        assert!(pixels.as_chunks::<4>().0.iter().all(|px| px[1] != FOLDED));

        let lut = sweep.azimuth_lut();
        assert_eq!(lut.len(), 3600 * 4);
        for (entry, px) in lut.as_chunks::<4>().0.iter().enumerate() {
            let row = usize::from(u16::from_le_bytes([px[0], px[1]]));
            assert!(row < sweep.rays.len(), "entry {entry}");
            let center = (entry as f32 + 0.5) / 10.0;
            let d = (sweep.rays[row].azimuth_deg - center).abs();
            assert!(d.min(360.0 - d) <= 0.5, "entry {entry} maps to row {row}");
        }
        // The half-degree cut fills every tenth-degree entry: each ray owns some.
        let mut used = vec![false; sweep.rays.len()];
        for px in lut.as_chunks::<4>().0 {
            used[usize::from(u16::from_le_bytes([px[0], px[1]]))] = true;
        }
        assert!(used.iter().all(|&u| u));

        assert_eq!(sweep.rows(), 720, "a complete cut needs no blank row");

        let encoded = png(u32::from(sweep.gates), sweep.rays.len() as u32, &pixels).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(&encoded))
            .read_info()
            .unwrap();
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut decoded).unwrap();
        assert_eq!((info.width, info.height), (1832, 720));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(decoded[..info.buffer_size()], pixels);
    }

    /// A sweep with a gap (a live sweep still being filled) gets one blank
    /// row that the lookup names for every unscanned azimuth; an empty sweep
    /// is one blank row named by every entry.
    #[test]
    fn a_partial_sweep_sends_unscanned_azimuths_to_a_blank_row() {
        let archive = fs::read(FIXTURE).unwrap();
        let mut sweep = lowest_reflectivity(&archive).unwrap();
        // Keep the rays between 30° and 120°, as arrival order would leave them.
        sweep
            .rays
            .retain(|ray| (30.0..120.0).contains(&ray.azimuth_deg));
        let kept = sweep.rays.len();
        assert!((170..190).contains(&kept), "{kept} rays kept");
        assert_eq!(sweep.rows(), kept as u32 + 1);
        let gates = usize::from(sweep.gates);
        let bounds = [-32, 0, 10, 20, 30, 40, 45, 50, 55, 60, 65, 70, 96];
        let pixels = sweep.texture(&bounds, 12);
        assert_eq!(pixels.len(), (kept + 1) * gates * 4);
        let blank = &pixels[kept * gates * 4..];
        assert!(
            blank
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [0, 0, 0, 255])
        );
        let lut = sweep.azimuth_lut();
        let mut blanks = 0;
        for (entry, px) in lut.as_chunks::<4>().0.iter().enumerate() {
            let row = usize::from(u16::from_le_bytes([px[0], px[1]]));
            let center = (entry as f32 + 0.5) / 10.0;
            let nearest = sweep
                .rays
                .iter()
                .map(|ray| {
                    let d = (ray.azimuth_deg - center).abs();
                    d.min(360.0 - d)
                })
                .fold(f32::INFINITY, f32::min);
            if nearest > GAP_DEG {
                assert_eq!(row, kept, "entry {entry} ({center}°) should be blank");
                blanks += 1;
            } else {
                assert!(row < kept, "entry {entry} ({center}°) should name a ray");
                let d = (sweep.rays[row].azimuth_deg - center).abs();
                assert!(d.min(360.0 - d) <= GAP_DEG);
            }
        }
        // 270° of the circle is unscanned, less the margin past each end.
        assert!((2680..2720).contains(&blanks), "{blanks} blank entries");

        sweep.rays.clear();
        assert_eq!(sweep.rows(), 1);
        assert_eq!(sweep.texture(&bounds, 12).len(), gates * 4);
        assert!(
            sweep
                .azimuth_lut()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| px[..2] == [0, 0])
        );
    }
}
