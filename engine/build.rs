//! Converts the Natural Earth GeoJSON that `scripts/extract-fixtures.sh`
//! extracts into the geography the binary embeds (DESIGN.md, basemap tiles,
//! shipped geography): one polyline blob holding the 1:50m world set and the
//! 1:10m set clipped to the NEXRAD network envelope, and the populated places
//! for low-zoom labels. GeoNames cities with population ≥ 5000, clipped to
//! the same envelope, become the location-picker gazetteer. Reruns only when
//! an input changes.
//!
//! Blob layout (`ne.bin`, read by `src/tiles.rs`): the magic `OMNE\x01`, then
//! for each of the two sets (1:50m, 1:10m) and each of its two layers
//! (boundaries: country and state lines; coast: coastlines and lake shores) a
//! varint polyline count, and per polyline a varint vertex count followed by
//! that many `(lon, lat)` pairs quantized to 1e-5° as zigzag varint deltas,
//! the first pair relative to the origin.

use serde_json::Value;
use std::{env, fs, path::Path, process::exit};

const RAW: &str = "../data/raw";
const THEMES: [(&str, usize); 4] = [
    ("admin_0_boundary_lines_land", 0),
    ("admin_1_states_provinces_lines", 0),
    ("coastline", 1),
    ("lakes", 1),
];
/// Everything the site table reaches (DESIGN.md): 5–75° N, west of 20° W or
/// east of 120° E, holding Lajes, Guam, Kunsan, and Kadena with their range.
fn in_envelope(lon: f64, lat: f64) -> bool {
    (5.0..=75.0).contains(&lat) && (lon <= -20.0 || lon >= 120.0)
}
const SCALE: f64 = 1e5;

fn main() {
    let out = env::var_os("OUT_DIR").expect("OUT_DIR");
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let raw = Path::new(&manifest).join(RAW);
    let mut inputs = vec![
        "places.geojson".to_owned(),
        "cities5000.txt".to_owned(),
        "admin1CodesASCII.txt".to_owned(),
    ];
    for scale in ["50m", "10m"] {
        for (theme, _) in THEMES {
            inputs.push(format!("ne_{scale}_{theme}.geojson"));
        }
    }
    for input in &inputs {
        let path = raw.join(input);
        println!("cargo:rerun-if-changed={}", path.display());
        if !path.is_file() {
            eprintln!(
                "\nMissing {}.\nThe engine embeds the Natural Earth geography and the GeoNames gazetteer; run `bash scripts/extract-fixtures.sh` once to extract and verify \
                 them (see data/README.md).\n",
                path.display()
            );
            exit(1);
        }
    }
    let mut blob = b"OMNE\x01".to_vec();
    for (scale, clip) in [("50m", false), ("10m", true)] {
        let mut layers: [Vec<Vec<(i32, i32)>>; 2] = [Vec::new(), Vec::new()];
        for (theme, layer) in THEMES {
            let text = fs::read_to_string(raw.join(format!("ne_{scale}_{theme}.geojson")))
                .expect("read Natural Earth file");
            let collection: Value = serde_json::from_str(&text).expect("parse GeoJSON");
            for feature in collection["features"].as_array().expect("features") {
                let Some(geometry) = feature["geometry"].as_object() else {
                    continue;
                };
                for line in lines(geometry) {
                    for piece in clipped(&line, clip) {
                        if piece.len() >= 2 {
                            layers[layer].push(piece);
                        }
                    }
                }
            }
        }
        for polylines in layers {
            varint(&mut blob, polylines.len() as u64);
            for polyline in polylines {
                varint(&mut blob, polyline.len() as u64);
                let (mut px, mut py) = (0i32, 0i32);
                for (x, y) in polyline {
                    zigzag(&mut blob, i64::from(x) - i64::from(px));
                    zigzag(&mut blob, i64::from(y) - i64::from(py));
                    (px, py) = (x, y);
                }
            }
        }
    }
    fs::write(Path::new(&out).join("ne.bin"), blob).expect("write ne.bin");

    // Populated places, worldwide: the tile's places become `tile_ready`
    // labels. Class and rank are a proposal for the overlay session: Natural
    // Earth's `scalerank` is the rank (lower is more important, as in
    // OpenMapTiles); `min_zoom` is the zoom the data authors first show the
    // place at; the class follows capital status and population.
    let text = fs::read_to_string(raw.join("places.geojson")).expect("read places");
    let collection: Value = serde_json::from_str(&text).expect("parse places");
    let mut places = Vec::new();
    for feature in collection["features"].as_array().expect("features") {
        let p = &feature["properties"];
        let (Some(name), Some(lat), Some(lon)) = (
            p["name"].as_str(),
            p["latitude"].as_f64(),
            p["longitude"].as_f64(),
        ) else {
            continue;
        };
        let feature_class = p["featurecla"].as_str().unwrap_or_default();
        let population = p["pop_max"].as_f64().unwrap_or_default();
        let class = if feature_class.starts_with("Admin-0 capital") {
            "capital"
        } else if population >= 100_000.0 {
            "city"
        } else if population >= 10_000.0 {
            "town"
        } else {
            "village"
        };
        let iso = p["iso_a2"].as_str().unwrap_or("");
        let country = if iso.is_empty() || iso == "-99" {
            ""
        } else {
            iso
        };
        places.push(serde_json::json!({
            "name": name,
            "lat": (lat * SCALE).round() / SCALE,
            "lon": (lon * SCALE).round() / SCALE,
            "class": class,
            "rank": p["scalerank"].as_u64().unwrap_or(10),
            "minZoom": p["min_zoom"].as_f64().unwrap_or(10.0),
            "region": p["adm1name"].as_str().unwrap_or(""),
            "country": country,
        }));
    }
    fs::write(
        Path::new(&out).join("places.json"),
        serde_json::to_vec(&places).expect("serialize places"),
    )
    .expect("write places.json");
    write_gazetteer(&raw, Path::new(&out));
}

/// GeoNames `cities5000` clipped to the NEXRAD envelope, for the location
/// picker only. Map labels stay on Natural Earth (`places.json`).
fn write_gazetteer(raw: &Path, out: &Path) {
    let mut admin1 = std::collections::HashMap::new();
    for line in fs::read_to_string(raw.join("admin1CodesASCII.txt"))
        .expect("read admin1")
        .lines()
    {
        let mut cols = line.split('\t');
        let (Some(code), Some(name)) = (cols.next(), cols.next()) else {
            continue;
        };
        if !code.is_empty() && !name.is_empty() {
            admin1.insert(code.to_owned(), name.to_owned());
        }
    }
    let mut gazetteer = Vec::new();
    for line in fs::read_to_string(raw.join("cities5000.txt"))
        .expect("read cities5000")
        .lines()
    {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 15 {
            continue;
        }
        let (name, lat, lon, fclass, fcode, country, adm1, pop) = (
            cols[1], cols[4], cols[5], cols[6], cols[7], cols[8], cols[10], cols[14],
        );
        if fclass != "P" || name.is_empty() {
            continue;
        }
        let (Ok(lat), Ok(lon), Ok(pop)) =
            (lat.parse::<f64>(), lon.parse::<f64>(), pop.parse::<f64>())
        else {
            continue;
        };
        if !in_envelope(lon, lat) {
            continue;
        }
        let class = if fcode == "PPLC" {
            "capital"
        } else if pop >= 100_000.0 {
            "city"
        } else if pop >= 10_000.0 {
            "town"
        } else {
            "village"
        };
        let rank = if pop >= 5_000_000.0 {
            1
        } else if pop >= 1_000_000.0 {
            2
        } else if pop >= 500_000.0 {
            3
        } else if pop >= 100_000.0 {
            4
        } else if pop >= 50_000.0 {
            6
        } else {
            8
        };
        let region = if adm1.is_empty() {
            String::new()
        } else {
            admin1
                .get(&format!("{country}.{adm1}"))
                .cloned()
                .unwrap_or_default()
        };
        gazetteer.push(serde_json::json!({
            "name": name,
            "lat": (lat * SCALE).round() / SCALE,
            "lon": (lon * SCALE).round() / SCALE,
            "class": class,
            "rank": rank,
            "region": region,
            "country": country,
        }));
    }
    fs::write(
        out.join("gazetteer.json"),
        serde_json::to_vec(&gazetteer).expect("serialize gazetteer"),
    )
    .expect("write gazetteer.json");
}

/// Every line in a geometry: line strings as they are, polygon rings as
/// closed lines (lakes are stroked shorelines, not fills).
fn lines(geometry: &serde_json::Map<String, Value>) -> Vec<Vec<(f64, f64)>> {
    let coordinates = &geometry["coordinates"];
    let positions = |value: &Value| -> Vec<(f64, f64)> {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|point| {
                let point = point.as_array()?;
                Some((point.first()?.as_f64()?, point.get(1)?.as_f64()?))
            })
            .collect()
    };
    let rings = |polygon: &Value| -> Vec<Vec<(f64, f64)>> {
        polygon
            .as_array()
            .into_iter()
            .flatten()
            .map(positions)
            .collect()
    };
    match geometry["type"].as_str().unwrap_or_default() {
        "LineString" => vec![positions(coordinates)],
        "MultiLineString" | "Polygon" => rings(coordinates),
        "MultiPolygon" => coordinates
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(rings)
            .collect(),
        other => panic!("unsupported geometry {other}"),
    }
}

/// Quantize a line, and when `clip` is set keep only its runs inside the
/// envelope, each extended by one vertex past the edge so the line leaves the
/// envelope instead of stopping short of it. Repeated vertices collapse.
fn clipped(line: &[(f64, f64)], clip: bool) -> Vec<Vec<(i32, i32)>> {
    let quantize =
        |(lon, lat): (f64, f64)| ((lon * SCALE).round() as i32, (lat * SCALE).round() as i32);
    let inside: Vec<bool> = line
        .iter()
        .map(|&(lon, lat)| !clip || in_envelope(lon, lat))
        .collect();
    let mut pieces = Vec::new();
    let mut current: Vec<(i32, i32)> = Vec::new();
    for (i, &point) in line.iter().enumerate() {
        let next_inside = inside.get(i + 1).copied().unwrap_or(false);
        let keep = inside[i] || (i > 0 && inside[i - 1]) || next_inside;
        if keep {
            let q = quantize(point);
            if current.last() != Some(&q) {
                current.push(q);
            }
        }
        if !inside[i] && !next_inside && !current.is_empty() {
            pieces.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
fn zigzag(out: &mut Vec<u8>, value: i64) {
    varint(out, ((value << 1) ^ (value >> 63)) as u64);
}
