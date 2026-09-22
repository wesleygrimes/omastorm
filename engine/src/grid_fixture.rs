//! Synthetic GridFamily mosaic: no network, tiny classified raster
//! (`docs/grid-adapters.md`).

use crate::{
    protocol::{Coverage, Crs, Family, FrameStatus, Kind, MosaicFrame, ProductClass},
    source::{MosaicMeta, SourceMetadataBorrowed},
    sweep,
};

pub const ID: &str = "fixture-mosaic";
pub const NAME: &str = "Fixture mosaic";
pub const ATTRIBUTION: &str = "Omastorm fixture";
pub const SELECTION_PRIORITY: i32 = 100;
pub const WIDTH: u32 = 16;
pub const HEIGHT: u32 = 16;

/// Pixel-corner affine covering the hello coverage box `[0,1]×[0,1]`
/// (lon east, lat south in raster rows). The spec's example affine is
/// exercised in `source::tests`; this one keeps the raster tiny and visible
/// wherever that box is selected.
pub const GEOTRANSFORM: [f64; 6] = [0.0, 1.0 / 16.0, 0.0, 1.0, 0.0, -1.0 / 16.0];

pub struct FixtureMosaic {
    pub id: &'static str,
    coverage: Coverage,
    frames: Vec<(MosaicFrame, Vec<u8>)>,
}

impl FixtureMosaic {
    pub fn new() -> Self {
        let coverage = Coverage::Box {
            north: 1.0,
            south: 0.0,
            east: 1.0,
            west: 0.0,
        };
        assert!(
            crate::source::inverse_affine(GEOTRANSFORM, 0.5, 0.5).is_some_and(|(c, r)| c >= 0.0
                && c < f64::from(WIDTH)
                && r >= 0.0
                && r < f64::from(HEIGHT))
        );
        let template: crate::protocol::Frame =
            serde_json::from_str(include_str!("../data/product.json")).unwrap();
        let palette = template.palette.clone();
        let bounds: Vec<f64> = template.bounds.iter().map(|&b| b as f64).collect();
        let frames = ["2026-01-01T00:00:00Z", "2026-01-01T00:05:00Z"]
            .into_iter()
            .enumerate()
            .map(|(i, scan_time)| {
                let compact = scan_time.replace(['-', ':'], "");
                let frame = MosaicFrame {
                    id: format!("{ID}-{compact}"),
                    product: template.product.clone(),
                    product_name: template.product_name.clone(),
                    units: template.units.clone(),
                    scan_time: scan_time.into(),
                    sweep_end: None,
                    status: FrameStatus::Complete,
                    texture: String::new(),
                    width: WIDTH,
                    height: HEIGHT,
                    crs: Crs::wgs84_geographic(),
                    geotransform: GEOTRANSFORM,
                    palette: palette.clone(),
                    bounds: bounds.clone(),
                };
                let pixels = raster(i, palette.len());
                let png = sweep::png(WIDTH, HEIGHT, &pixels).expect("fixture mosaic png");
                (frame, png)
            })
            .collect();
        Self {
            id: ID,
            coverage,
            frames,
        }
    }

    pub fn metadata(&self) -> SourceMetadataBorrowed<'_> {
        SourceMetadataBorrowed {
            id: self.id,
            family: Family::Grid,
            kind: Kind::Mosaic,
            default_product_class: ProductClass::Reflectivity,
            name: NAME,
            attribution: ATTRIBUTION,
            mosaic: Some(MosaicMeta {
                coverage: &self.coverage,
                selection_priority: SELECTION_PRIORITY,
                covering: false,
            }),
        }
    }

    /// Complete synthetic frames, oldest first. No network. Scan times sit
    /// a few minutes behind now so live age judging is not involved and the
    /// timeline looks current.
    pub fn frames(&self) -> Vec<(MosaicFrame, Vec<u8>, i64)> {
        let newest = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let last = self.frames.len().saturating_sub(1) as i64;
        self.frames
            .iter()
            .enumerate()
            .map(|(i, (frame, png))| {
                let start_ms = newest - (last - i as i64) * 300_000;
                let mut frame = frame.clone();
                if let Some(t) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(start_ms) {
                    frame.scan_time = t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
                    frame.id = format!("{ID}-{}", t.format("%Y%m%dT%H%M%SZ"));
                }
                (frame, png.clone(), start_ms)
            })
            .collect()
    }
}

/// Classified RGBA: R = class+1, G bit 0 missing / bit 1 undetect, B 0, A 255.
fn raster(seed: usize, classes: usize) -> Vec<u8> {
    let classes = classes.max(1);
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let n = (row + col + seed as u32) % 8;
            let (r, g) = match n {
                0 => (0, 1), // missing
                1 => (0, 2), // undetect
                _ => {
                    let class = ((row * WIDTH + col + seed as u32) as usize) % classes;
                    ((class as u8) + 1, 0)
                }
            };
            pixels.extend_from_slice(&[r, g, 0, 255]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Crs;

    #[test]
    fn fixture_mosaic_decodes_a_classified_texture_on_geographic_crs() {
        let mosaic = FixtureMosaic::new();
        let frames = mosaic.frames();
        assert_eq!(frames.len(), 2);
        let (frame, png, _) = &frames[0];
        assert_eq!(frame.width, WIDTH);
        assert_eq!(frame.height, HEIGHT);
        assert_eq!(frame.status, FrameStatus::Complete);
        assert!(matches!(frame.crs, Crs::Geographic { .. }));
        assert_eq!(frame.crs.ellipsoid().semi_major_m, 6_378_137.0);
        assert_eq!(frame.geotransform, GEOTRANSFORM);
        let info = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap()
            .info()
            .clone();
        assert_eq!((info.width, info.height), (WIDTH, HEIGHT));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        // Centre of the coverage box lands inside the raster.
        let (col, row) = crate::source::inverse_affine(GEOTRANSFORM, 0.5, 0.5).unwrap();
        assert!(col >= 0.0 && col < f64::from(WIDTH));
        assert!(row >= 0.0 && row < f64::from(HEIGHT));
        let (col0, row0) = crate::source::inverse_affine(GEOTRANSFORM, 0.0, 1.0).unwrap();
        assert!((col0 - 0.0).abs() < 1e-12 && (row0 - 0.0).abs() < 1e-12);
        let pixels = raster(0, 12);
        assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);
        assert_eq!(pixels[3], 255);
        assert_eq!(pixels[2], 0);
    }
}
