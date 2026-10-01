//! Count native cell centres from the existing classified PNG, one row at a
//! time. No full-frame validity mask or intensity arrays are retained.
use crate::{
    grid_view::{Bounds, RADIUS, latitude},
    protocol::{Crs, MosaicFrame},
};
use serde::Serialize;
use std::{fs::File, io::BufReader, path::Path};

#[derive(Default, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub measured: u64,
    pub no_echo: u64,
    pub missing: u64,
    pub outside: u64,
    pub unknown: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub v: u32,
    pub source_id: String,
    pub frame_id: String,
    pub view_id: String,
    pub unfetched: bool,
    pub counts: Counts,
}

// OPERA's ellipsoidal LAEA inverse (EPSG method 9820). Authalic latitude is
// inverted with a bounded Newton iteration. Other compiled grids here use
// geographic or spherical Mercator; unsupported inputs remain unknown.
fn inverse(crs: &Crs, x: f64, y: f64) -> Option<(f64, f64)> {
    match crs {
        Crs::Geographic {
            datum_transform: None,
            ..
        } => Some((x, y)),
        Crs::Mercator {
            ellipsoid,
            lon0_deg,
            scale,
            false_easting_m,
            false_northing_m,
            datum_transform: None,
        } if ellipsoid.inverse_flattening == 0. => {
            let a = ellipsoid.semi_major_m * scale;
            Some((
                (x - false_easting_m) / a * 180. / std::f64::consts::PI + lon0_deg,
                latitude((y - false_northing_m) * RADIUS / a),
            ))
        }
        Crs::LambertAzimuthalEqualArea {
            ellipsoid,
            lat0_deg,
            lon0_deg,
            false_easting_m,
            false_northing_m,
            datum_transform: None,
        } => {
            let a = ellipsoid.semi_major_m;
            let f = if ellipsoid.inverse_flattening == 0. {
                0.
            } else {
                1. / ellipsoid.inverse_flattening
            };
            let e2 = f * (2. - f);
            let e = e2.sqrt();
            let q = |phi: f64| {
                let s = phi.sin();
                if e < 1e-8 {
                    2. * s
                } else {
                    (1. - e2)
                        * (s / (1. - e2 * s * s) - ((1. - e * s) / (1. + e * s)).ln() / (2. * e))
                }
            };
            let qp = q(std::f64::consts::FRAC_PI_2);
            let phi0 = lat0_deg.to_radians();
            let beta0 = (q(phi0) / qp).asin();
            let rq = a * (qp / 2.).sqrt();
            let d = a * phi0.cos() / ((1. - e2 * phi0.sin().powi(2)).sqrt() * rq * beta0.cos());
            let xx = (x - false_easting_m) / d;
            let yy = (y - false_northing_m) * d;
            let rho = xx.hypot(yy);
            if rho > 2. * rq {
                return None;
            }
            let c = 2. * (rho / (2. * rq)).asin();
            let beta = if rho < 1e-8 {
                beta0
            } else {
                (c.cos() * beta0.sin() + yy * c.sin() * beta0.cos() / rho)
                    .clamp(-1., 1.)
                    .asin()
            };
            let lon = lon0_deg.to_radians()
                + (xx * c.sin()).atan2(rho * beta0.cos() * c.cos() - yy * beta0.sin() * c.sin());
            let target = qp * beta.sin();
            let mut phi = beta;
            for _ in 0..6 {
                let derivative = 2. * (1. - e2) * phi.cos() / (1. - e2 * phi.sin().powi(2)).powi(2);
                if derivative.abs() < 1e-12 {
                    break;
                }
                phi -= (q(phi) - target) / derivative;
            }
            Some((lon.to_degrees(), phi.to_degrees()))
        }
        _ => None,
    }
}

pub fn count(path: &Path, frame: &MosaicFrame, bounds: Bounds) -> Result<(Counts, bool), String> {
    if frame.scan_time.is_empty() {
        return Ok((
            Counts {
                unknown: 1,
                ..Counts::default()
            },
            true,
        ));
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_limits(png::Limits { bytes: 2 << 20 });
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    if info.width != frame.width
        || info.height != frame.height
        || info.color_type != png::ColorType::Rgba
        || info.bit_depth != png::BitDepth::Eight
        || info.interlaced
        || u64::from(info.width) * u64::from(info.height) > 20_000_000
    {
        return Err("unexpected classified grid PNG".into());
    }
    let mut counts = Counts::default();
    let [x0, dx, rx, y0, ry, dy] = frame.geotransform;
    let mut y = 0;
    while let Some(row) = reader.next_row().map_err(|e| e.to_string())? {
        for (x, p) in row.data().as_chunks::<4>().0.iter().enumerate() {
            let col = x as f64 + 0.5;
            let line = y as f64 + 0.5;
            let Some((lon, lat)) = inverse(
                &frame.crs,
                x0 + col * dx + line * rx,
                y0 + col * ry + line * dy,
            ) else {
                continue;
            };
            if lon < bounds.west || lon > bounds.east || lat < bounds.south || lat > bounds.north {
                continue;
            }
            match (p[0], p[1]) {
                (r, 0) if r > 0 => counts.measured += 1,
                (0, 2) => counts.no_echo += 1,
                (0, 1) => counts.missing += 1,
                (0, 4) => counts.outside += 1,
                _ => counts.unknown += 1,
            }
        }
        y += 1;
    }
    // For supported geographic/Mercator frames, coverage is axis aligned.
    // OPERA's curved projected view edges are tested at bounded intervals;
    // a conservative unfetched result is preferable to a false coverage claim.
    let mut unfetched = false;
    for i in 0..=64 {
        let t = i as f64 / 64.;
        for (lon, lat) in [
            (bounds.west + (bounds.east - bounds.west) * t, bounds.south),
            (bounds.west + (bounds.east - bounds.west) * t, bounds.north),
            (
                bounds.west,
                bounds.south + (bounds.north - bounds.south) * t,
            ),
            (
                bounds.east,
                bounds.south + (bounds.north - bounds.south) * t,
            ),
        ] {
            if !inside(frame, lon, lat) {
                unfetched = true;
            }
        }
    }
    Ok((counts, unfetched))
}

fn inside(frame: &MosaicFrame, lon: f64, lat: f64) -> bool {
    // Invert the inverse projection with bounded Newton iterations; this is
    // only the small perimeter test, not the per-cell hot path.
    let mut x = frame.geotransform[0] + frame.width as f64 * frame.geotransform[1] / 2.;
    let mut y = frame.geotransform[3] + frame.height as f64 * frame.geotransform[5] / 2.;
    for _ in 0..12 {
        let Some((l, p)) = inverse(&frame.crs, x, y) else {
            return false;
        };
        if (l - lon).abs() + (p - lat).abs() < 1e-8 {
            break;
        }
        let Some((lx, px)) = inverse(&frame.crs, x + 1., y) else {
            return false;
        };
        let Some((ly, py)) = inverse(&frame.crs, x, y + 1.) else {
            return false;
        };
        let det = (lx - l) * (py - p) - (ly - l) * (px - p);
        if det.abs() < 1e-18 {
            return false;
        }
        x -= ((py - p) * (l - lon) - (ly - l) * (p - lat)) / det;
        y -= (-(px - p) * (l - lon) + (lx - l) * (p - lat)) / det;
    }
    crate::source::inverse_affine(frame.geotransform, x, y).is_some_and(|(c, r)| {
        c >= -1e-6
            && r >= -1e-6
            && c <= frame.width as f64 + 1e-6
            && r <= frame.height as f64 + 1e-6
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "release-profile full-size OPERA row-stream resource measurement"]
    fn full_opera_row_stream_resource_measurement() {
        use std::io::Write;
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/grid-validity-bench");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("opera.png");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = png::Encoder::new(file, 3800, 4400);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let mut stream = writer.stream_writer().unwrap();
            let row: Vec<_> = (0..3800)
                .flat_map(|x| {
                    if x % 31 == 0 {
                        [0, 1, 0, 255]
                    } else if x % 29 == 0 {
                        [3, 0, 0, 255]
                    } else {
                        [0, 2, 0, 255]
                    }
                })
                .collect();
            for _ in 0..4400 {
                stream.write_all(&row).unwrap();
            }
            stream.finish().unwrap();
        }
        let (mut frame, _, _) = crate::grid_fixture::FixtureMosaic::new().frames().remove(0);
        frame.width = 3800;
        frame.height = 4400;
        frame.crs = crate::opera::Opera::crs();
        frame.geotransform = [-500., 1000., 0., 500., 0., -1000.];
        let resource = || {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            let pss = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap();
            let number = |text: &str, name: &str| {
                text.lines()
                    .find(|l| l.starts_with(name))
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
            };
            serde_json::json!({"rssKiB":number(&status,"VmRSS:"),"peakRssKiB":number(&status,"VmHWM:"),"pssKiB":number(&pss,"Pss:")})
        };
        let baseline = resource();
        let mut runs = Vec::new();
        for _ in 0..3 {
            let started = std::time::Instant::now();
            let (counts, unfetched) = count(
                &path,
                &frame,
                Bounds {
                    west: -30.,
                    south: 32.,
                    east: 50.,
                    north: 70.,
                },
            )
            .unwrap();
            assert!(counts.measured > 0 && counts.no_echo > 0 && counts.missing > 0);
            assert_eq!(counts.outside, 0);
            runs.push(serde_json::json!({"ms":started.elapsed().as_secs_f64()*1000.,"resources":resource(),"counts":counts,"unfetched":unfetched}));
        }
        use sha2::{Digest, Sha256};
        let report = serde_json::json!({"scope":"release-profile validity worker only; source raster decode/client/GPU excluded",
            "dimensions":[3800,4400],"inputSha256":format!("{:x}",Sha256::digest(std::fs::read(&path).unwrap())),
            "baseline":baseline,"runs":runs});
        std::fs::write(dir.join("report.json"), format!("{report:#}\n")).unwrap();
        std::fs::remove_file(path).unwrap();
        println!("{report:#}");
    }
    #[test]
    fn row_stream_counts_flags_holes_and_unfetched_without_a_second_mask() {
        let (mut frame, _, _) = crate::grid_fixture::FixtureMosaic::new().frames().remove(0);
        let mut pixels = [0, 2, 0, 255].repeat(256);
        for (i, p) in [
            [1, 0, 0, 255],
            [0, 1, 0, 255],
            [0, 4, 0, 255],
            [0, 8, 0, 255],
            [0, 0, 0, 255],
        ]
        .iter()
        .enumerate()
        {
            pixels[4 * (100 + i)..][..4].copy_from_slice(p);
        }
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../target/validity-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("frame.png");
        std::fs::write(&path, crate::sweep::png(16, 16, &pixels).unwrap()).unwrap();
        let bounds = Bounds {
            west: 0.,
            south: 0.,
            east: 1.,
            north: 1.,
        };
        let (counts, unfetched) = count(&path, &frame, bounds).unwrap();
        assert_eq!(
            (
                counts.measured,
                counts.no_echo,
                counts.missing,
                counts.outside,
                counts.unknown
            ),
            (1, 251, 1, 1, 2)
        );
        assert!(!unfetched);
        let (_, unfetched) = count(
            &path,
            &frame,
            Bounds {
                west: -1.,
                ..bounds
            },
        )
        .unwrap();
        assert!(unfetched);
        frame.scan_time.clear();
        let (counts, unfetched) = count(&path, &frame, bounds).unwrap();
        assert_eq!(counts.outside, 0);
        assert_eq!(counts.unknown, 1);
        assert!(unfetched);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn opera_inverse_preserves_projection_origin_and_declared_ellipsoid() {
        let crs = crate::opera::Opera::crs();
        let (lon, lat) = inverse(
            &crs,
            crate::opera::FALSE_EASTING_M,
            crate::opera::FALSE_NORTHING_M,
        )
        .unwrap();
        assert!((lon - 10.).abs() < 1e-9);
        assert!((lat - 55.).abs() < 1e-9);
    }
}
