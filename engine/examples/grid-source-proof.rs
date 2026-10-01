//! Offline source evidence for #128. No provider data is embedded or fetched.
//! Run with `mise exec -- cargo run --offline --locked -p omastorm-engine
//! --example grid-source-proof -- <png|pair|compare|cog> ...`.
#[path = "../src/cog.rs"]
mod cog;

use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Cursor};

const RAIN: [[u8; 3]; 14] = [
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

struct Image {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    bytes: usize,
    chunks: Vec<String>,
    source_type: String,
}

fn load(path: &str) -> Image {
    let bytes = fs::read(path).expect("read PNG");
    assert!(bytes.len() <= 8 << 20, "proof PNG body cap");
    let mut decoder = png::Decoder::new(Cursor::new(&bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().expect("PNG header");
    let info = reader.info();
    assert!(info.width <= 1024 && info.height <= 1024, "proof pixel cap");
    assert_eq!(info.bit_depth, png::BitDepth::Eight, "8-bit proof only");
    let source_type = format!("{:?}", info.color_type);
    let mut buf = vec![0; reader.output_buffer_size().expect("buffer size")];
    let out = reader.next_frame(&mut buf).expect("decode PNG");
    let rgba = match out.color_type {
        png::ColorType::Rgba => buf[..out.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..out.buffer_size()]
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        _ => panic!("proof supports RGB/RGBA/indexed RGB only"),
    };
    let mut chunks = Vec::new();
    let mut at = 8;
    while at + 12 <= bytes.len() {
        let n = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let name = std::str::from_utf8(&bytes[at + 4..at + 8]).unwrap();
        if !chunks.iter().any(|v| v == name) {
            chunks.push(name.to_string());
        }
        at = at.checked_add(12 + n).expect("chunk length");
    }
    Image {
        width: out.width,
        height: out.height,
        rgba,
        bytes: bytes.len(),
        chunks,
        source_type,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("png") | Some("pair") => {
            let image = load(&args[2]);
            let mask = if args[1] == "pair" {
                Some(load(&args[3]))
            } else {
                None
            };
            if let Some(m) = &mask {
                assert_eq!((image.width, image.height), (m.width, m.height));
                assert!(
                    m.rgba.as_chunks::<4>().0.iter().all(|p| {
                        p[3] <= 128
                            && (p[3] == 0 || (p[0] == p[1] && p[1] == p[2]))
                            && (p[3] != 128 || p[..3] == [181, 181, 181])
                    }),
                    "verified inverse-mask style"
                );
            }
            let mut colors = BTreeMap::<String, usize>::new();
            let mut validity = BTreeMap::<String, usize>::new();
            let mut samples = BTreeMap::new();
            let mut measured_outside_mask = Vec::new();
            let mut measured_outside_boundary_band = 0;
            let mut classified = Vec::with_capacity(image.rgba.len());
            for (i, p) in image.rgba.as_chunks::<4>().0.iter().enumerate() {
                let class = RAIN.iter().position(|c| p[..3] == c[..]);
                let endpoint = mask.as_ref().and_then(|m| {
                    let x = i % m.width as usize;
                    let y = i / m.width as usize;
                    let alpha = m.rgba[i * 4 + 3];
                    if ![0, 128].contains(&alpha)
                        || x < 2
                        || y < 2
                        || x + 2 >= m.width as usize
                        || y + 2 >= m.height as usize
                    {
                        return None;
                    }
                    (y - 2..=y + 2)
                        .all(|yy| {
                            (x - 2..=x + 2)
                                .all(|xx| m.rgba[(yy * m.width as usize + xx) * 4 + 3] == alpha)
                        })
                        .then_some(alpha)
                });
                if let Some(m) = &mask
                    && p[3] == 255
                    && class.is_some()
                    && m.rgba[i * 4 + 3] == 128
                {
                    measured_outside_mask
                        .push(json!({"x":i % image.width as usize,"y":i / image.width as usize}));
                    if endpoint == Some(128) {
                        measured_outside_boundary_band += 1;
                    }
                }
                let label = if p[3] == 0 {
                    "transparent".to_string()
                } else if p[3] != 255 {
                    "partial-alpha".to_string()
                } else if let Some(c) = class {
                    format!("class-{c}")
                } else {
                    format!("unknown-{:02x}{:02x}{:02x}", p[0], p[1], p[2])
                };
                *colors.entry(label.clone()).or_default() += 1;
                let state = if p[3] == 255 && class.is_some() {
                    "measured"
                } else if p[3] != 0 {
                    "unknown"
                } else if mask.is_some() {
                    match endpoint {
                        Some(0) => "covered-no-visible-echo",
                        Some(128) => "outside-observed-coverage",
                        _ => "ambiguous-mask-edge",
                    }
                } else {
                    "unknown"
                };
                *validity.entry(state.to_string()).or_default() += 1;
                let x = i as u32 % image.width;
                let y = i as u32 / image.width;
                samples
                    .entry(label)
                    .or_insert(json!({"x":x,"y":y,"rgba":p}));
                samples
                    .entry(state.to_string())
                    .or_insert(json!({"x":x,"y":y,"rgba":p}));
                let (r, g) = match state {
                    "measured" => (class.unwrap() as u8 + 1, 0),
                    "covered-no-visible-echo" => (0, 2),
                    "outside-observed-coverage" => (0, 4),
                    _ => (0, 8),
                };
                classified.extend_from_slice(&[r, g, 0, 255]);
            }
            let mut encoded = Vec::new();
            let mut encoder = png::Encoder::new(&mut encoded, image.width, image.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&classified)
                .unwrap();
            let mut holes = Vec::new();
            if let Some(m) = &mask {
                let mut seen = vec![false; (m.width * m.height) as usize];
                for start in 0..seen.len() {
                    if seen[start] || m.rgba[start * 4 + 3] != 128 {
                        continue;
                    }
                    let mut stack = vec![start];
                    seen[start] = true;
                    let mut size = 0;
                    let mut edge = false;
                    while let Some(i) = stack.pop() {
                        size += 1;
                        let x = i % m.width as usize;
                        let y = i / m.width as usize;
                        if x == 0
                            || y == 0
                            || x + 1 == m.width as usize
                            || y + 1 == m.height as usize
                        {
                            edge = true;
                        }
                        for (xx, yy) in [
                            (x.wrapping_sub(1), y),
                            (x + 1, y),
                            (x, y.wrapping_sub(1)),
                            (x, y + 1),
                        ] {
                            if xx >= m.width as usize || yy >= m.height as usize {
                                continue;
                            }
                            let j = yy * m.width as usize + xx;
                            if !seen[j] && m.rgba[j * 4 + 3] == 128 {
                                seen[j] = true;
                                stack.push(j);
                            }
                        }
                    }
                    if !edge && size >= 4 {
                        holes.push(json!({"cells":size,"x":start % m.width as usize,"y":start / m.width as usize}));
                    }
                }
            }
            json!({"width":image.width,"height":image.height,"sourceBytes":image.bytes,
                "sourceType":image.source_type,"chunks":image.chunks,
                "rgbaSha256":format!("{:x}",Sha256::digest(&image.rgba)),
                "colors":colors,"validity":validity,"samples":samples,
                "classifiedBytes":encoded.len(),"enclosedUncoveredComponents":holes,
                "measuredOutsideMask":measured_outside_mask,
                "measuredOutsideBoundaryBand":measured_outside_boundary_band,
                "maskBoundaryRadiusPixels":2})
        }
        Some("compare") => {
            let a = load(&args[2]);
            let b = load(&args[3]);
            let dx: u32 = args[4].parse().unwrap();
            let dy: u32 = args[5].parse().unwrap();
            assert!(dx < a.width && dy < a.height);
            let mut mismatches = 0;
            let mut wet = 0;
            let mut cells = 0;
            let mut examples = Vec::new();
            for y in 0..b.height.min(a.height - dy) {
                for x in 0..b.width.min(a.width - dx) {
                    let ai = ((y + dy) * a.width + x + dx) as usize * 4;
                    let bi = (y * b.width + x) as usize * 4;
                    let ap = &a.rgba[ai..ai + 4];
                    let bp = &b.rgba[bi..bi + 4];
                    cells += 1;
                    if ap[3] != 0 || bp[3] != 0 {
                        wet += 1;
                    }
                    if ap != bp {
                        mismatches += 1;
                        if examples.len() < 8 {
                            examples.push(json!({"aX":x+dx,"aY":y+dy,"a":ap,"b":bp}));
                        }
                    }
                }
            }
            json!({"overlapCells":cells,"nontransparentCells":wet,"mismatches":mismatches,"examples":examples})
        }
        Some("stress") => {
            let mut seed = 128u32;
            let mut rgba = Vec::with_capacity(1024 * 1024 * 4);
            for _ in 0..1024 * 1024 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let class = (seed % 18) as u8;
                let (r, g) = if class < 14 {
                    (class + 1, 0)
                } else {
                    (0, 1 << (class - 14))
                };
                rgba.extend_from_slice(&[r, g, 0, 255]);
            }
            let mut bytes = Vec::new();
            let mut encoder = png::Encoder::new(&mut bytes, 1024, 1024);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&rgba)
                .unwrap();
            json!({"width":1024,"height":1024,"rgbaBytes":rgba.len(),"encodedBytes":bytes.len(),"rgbaSha256":format!("{:x}",Sha256::digest(&rgba))})
        }
        Some("footprint") => {
            let data: serde_json::Value =
                serde_json::from_slice(&fs::read(&args[2]).unwrap()).unwrap();
            let lat: f64 = args[3].parse().unwrap();
            let lon: f64 = args[4].parse().unwrap();
            let contains = data["features"][0]["geometry"]["coordinates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|polygon| {
                    let rings = polygon.as_array().unwrap();
                    let in_ring = |ring: &serde_json::Value| {
                        let vertices = ring.as_array().unwrap();
                        let mut inside = false;
                        for pair in vertices.windows(2) {
                            let (x0, y0) =
                                (pair[0][0].as_f64().unwrap(), pair[0][1].as_f64().unwrap());
                            let (x1, y1) =
                                (pair[1][0].as_f64().unwrap(), pair[1][1].as_f64().unwrap());
                            if (y0 > lat) != (y1 > lat)
                                && lon < x0 + (lat - y0) * (x1 - x0) / (y1 - y0)
                            {
                                inside = !inside;
                            }
                        }
                        inside
                    };
                    in_ring(&rings[0]) && !rings[1..].iter().any(in_ring)
                });
            json!({"lat":lat,"lon":lon,"covered":contains})
        }
        Some("band") => {
            let image = load(&args[2]);
            let mask = load(&args[9]);
            assert_eq!((image.width, image.height), (mask.width, mask.height));
            let west: f64 = args[4].parse().unwrap();
            let north: f64 = args[5].parse().unwrap();
            let step: f64 = args[6].parse().unwrap();
            let sites: Vec<_> = fs::read_to_string(&args[3])
                .unwrap()
                .lines()
                .filter_map(|line| {
                    let words: Vec<_> = line.split_whitespace().collect();
                    let id = words
                        .iter()
                        .find(|s| s.len() == 5 && s.starts_with("CAS"))?;
                    let numbers: Vec<f64> = words.iter().filter_map(|s| s.parse().ok()).collect();
                    if numbers.len() < 2 {
                        return None;
                    }
                    Some((
                        id.to_string(),
                        numbers[0].to_radians(),
                        numbers[1].to_radians(),
                    ))
                })
                .filter(|(id, _, _)| id == &args[7] || id == &args[8])
                .collect();
            assert_eq!(sites.len(), 2);
            let wet: Vec<bool> = image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[3] == 255 && RAIN.iter().any(|c| p[..3] == c[..]))
                .collect();
            let mut zones = vec![0u8; wet.len()];
            for (i, &is_wet) in wet.iter().enumerate() {
                if !is_wet {
                    continue;
                }
                let x = i % image.width as usize;
                let y = i / image.width as usize;
                let lon = (west + (x as f64 + 0.5) * step) / 6378137.0;
                let lat = 2.0 * ((north - (y as f64 + 0.5) * step) / 6378137.0).exp().atan()
                    - std::f64::consts::FRAC_PI_2;
                for (n, (_, slat, slon)) in sites.iter().enumerate() {
                    let h = ((lat - slat) / 2.0).sin().powi(2)
                        + lat.cos() * slat.cos() * ((lon - slon) / 2.0).sin().powi(2);
                    if 2.0 * 6371.0 * h.sqrt().asin() <= 240.0 {
                        zones[i] |= 1 << n;
                    }
                }
            }
            let mut seen = vec![false; wet.len()];
            let mut bands = Vec::new();
            for start in 0..wet.len() {
                if zones[start] == 0 || seen[start] {
                    continue;
                }
                let mut stack = vec![start];
                seen[start] = true;
                let mut cells = Vec::new();
                while let Some(i) = stack.pop() {
                    let x = i % image.width as usize;
                    let y = i / image.width as usize;
                    let lon = (west + (x as f64 + 0.5) * step) / 6378137.0;
                    let lat = 2.0 * ((north - (y as f64 + 0.5) * step) / 6378137.0).exp().atan()
                        - std::f64::consts::FRAC_PI_2;
                    cells.push((x, y, zones[i], lat.to_degrees(), lon.to_degrees()));
                    for dy in -1isize..=1 {
                        for dx in -1isize..=1 {
                            if dx.abs() + dy.abs() != 1 {
                                continue;
                            }
                            let xx = x as isize + dx;
                            let yy = y as isize + dy;
                            if xx < 0
                                || yy < 0
                                || xx >= image.width as isize
                                || yy >= image.height as isize
                            {
                                continue;
                            }
                            let j = yy as usize * image.width as usize + xx as usize;
                            if zones[j] != 0 && !seen[j] {
                                seen[j] = true;
                                stack.push(j);
                            }
                        }
                    }
                }
                if cells.len() < 100 {
                    continue;
                }
                let mut counts = [0usize; 3];
                let mut mask_alpha = BTreeMap::<u8, usize>::new();
                let mut samples = [None, None, None];
                for &(x, y, c, lat, lon) in &cells {
                    let i = (y * image.width as usize + x) * 4;
                    *mask_alpha.entry(mask.rgba[i + 3]).or_default() += 1;
                    let z = match c {
                        1 => 0,
                        3 => 1,
                        2 => 2,
                        _ => unreachable!(),
                    };
                    counts[z] += 1;
                    if samples[z].is_none() {
                        samples[z].get_or_insert(json!({"x":x,"y":y,"lat":lat,"lon":lon}));
                    }
                }
                if counts.iter().all(|&n| n >= 10) {
                    bands.push(json!({"a":sites[0].0,"b":sites[1].0,"componentCells":cells.len(),"aOnly":counts[0],"overlap":counts[1],"bOnly":counts[2],"maskAlpha":mask_alpha,"samples":samples}));
                }
            }
            json!({"radiusKm":240,"connectivity":4,"restrictedToFootprintUnion":true,"sites":sites.len(),"bands":bands})
        }
        Some("cog") => {
            let bytes = fs::read(&args[2]).unwrap();
            let raster = cog::decode_float_cog(&bytes).expect("OPERA COG");
            let mut counts = BTreeMap::<String, usize>::new();
            let odim = args
                .get(3)
                .map(|p| fs::read(p).expect("h5dump f64 LE output"));
            if let Some(o) = &odim {
                assert_eq!(o.len(), raster.values.len() * 8);
            }
            let mut disagreements = 0;
            for (i, &v) in raster.values.iter().enumerate() {
                let state = if v.is_nan() {
                    "undetect"
                } else if Some(v) == raster.nodata {
                    "missing"
                } else {
                    "measured"
                };
                *counts.entry(state.to_string()).or_default() += 1;
                if let Some(o) = &odim {
                    let h = f64::from_le_bytes(o[i * 8..i * 8 + 8].try_into().unwrap());
                    // Values verified in the same object's ODIM what attributes.
                    let hs = if h == -8_888_000.0 {
                        "undetect"
                    } else if h == -9_999_000.0 {
                        "missing"
                    } else {
                        "measured"
                    };
                    if state != hs || (state == "measured" && (h as f32) != v) {
                        disagreements += 1;
                    }
                }
            }
            json!({"width":raster.width,"height":raster.height,"nodata":raster.nodata,
                "geotransform":raster.geotransform,"projection":raster.geo_double_params,
                "sourceBytes":bytes.len(),"counts":counts,"odimCompared":odim.is_some(),
                "odimDisagreements":disagreements})
        }
        _ => panic!(
            "png IMAGE | pair RAIN INVERSE | compare A B DX DY | cog TIFF [ODIM_F64_LE] | stress | footprint GEOJSON LAT LON | band RAIN SITES_TEXT WEST NORTH STEP SITE_A SITE_B INVERSE"
        ),
    };
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}
