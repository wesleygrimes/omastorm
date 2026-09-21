//! GPU rendering check for the polar radar path (DESIGN.md, technical foundation). Renders
//! `ui/RadarMap.qml` alone through Quickshell at fixed sizes and cameras, in
//! every treatment, and compares each pixel against the shader's lookup rule
//! replayed in Rust: once over the golden fixture codes and once over
//! hand-built sweeps that carry the statuses the fixture lacks. A second test
//! grabs the whole map
//! with tiles and overlay, pans the camera by a known number of pixels, and
//! asserts every layer moved by exactly that much.
//!
//! Needs Quickshell and a desktop OpenGL context, so it is ignored by default:
//! `bash scripts/cargo.sh test --offline --locked -- --ignored rendering`.
//! Captures and the report land under `review/`.

use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    f64::consts::PI,
    fs,
    io::{BufRead, BufReader},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../golden/ktlx-20130520/");
/// The shader's sphere for longitude and latitude to ground distance, and
/// its 4/3 effective-radius earth for the beam, in metres (`ui/shaders/radar.frag`).
const R_M: f32 = 6_371_000.0;
const EARTH_M: f32 = R_M * 4.0 / 3.0;
/// Web Mercator's latitude limit (`ui/RadarMap.qml`).
const MAX_LATITUDE: f64 = 85.051_128_78;
/// The glyph density mask; slot = row * 3 + column inside a 3 px cell.
const DENSITY: [i32; 9] = [0, 7, 3, 6, 4, 8, 2, 5, 1];
const TREATMENTS: [&str; 3] = ["PIXELS", "GLYPHS", "STIPPLE"];
/// Status bits in the texture's G channel (`docs/protocol.md`).
const FOLDED: u8 = 1;
const BELOW_THRESHOLD: u8 = 2;
const OUTSIDE: u8 = 4;
/// How close to a gate or row boundary a sample may sit before the GPU's
/// transcendental precision, not the rule, decides the neighbour. In gates
/// (250 m each) and in lookup entries (a tenth of a degree).
const GATE_EPSILON: f32 = 0.03;
const ENTRY_EPSILON: f32 = 0.005;

/// A sweep as the shader samples it: R (class + 1), G (status), and B (raw
/// code) per gate, row-major rays × gates, with the row named by each
/// tenth-degree entry.
struct Sweep {
    gates: usize,
    cells: Vec<[u8; 3]>,
    lut: Vec<u16>,
}
impl Sweep {
    fn cell(&self, row: usize, gate: usize) -> [u8; 3] {
        self.cells[row * self.gates + gate]
    }
    /// The protocol's nearest-ray lookup for `azimuths` in ascending order.
    fn lut(azimuths: &[f32]) -> Vec<u16> {
        let n = azimuths.len();
        (0..3600u32)
            .map(|entry| {
                let center = (entry as f32 + 0.5) / 10.0;
                let after = azimuths.partition_point(|&az| az <= center) % n;
                let before = (after + n - 1) % n;
                let distance = |row: usize| {
                    let d = (azimuths[row] - center).abs();
                    d.min(360.0 - d)
                };
                (if distance(before) <= distance(after) {
                    before
                } else {
                    after
                }) as u16
            })
            .collect()
    }
    fn texture_png(&self, rays: usize) -> Vec<u8> {
        let pixels: Vec<u8> = self
            .cells
            .iter()
            .flat_map(|&[class, status, code]| [class, status, code, 255])
            .collect();
        png(self.gates as u32, rays as u32, &pixels)
    }
    fn lut_png(&self) -> Vec<u8> {
        let pixels: Vec<u8> = self
            .lut
            .iter()
            .flat_map(|row| [(row & 0xff) as u8, (row >> 8) as u8, 0, 255])
            .collect();
        png(3600, 1, &pixels)
    }
}

/// The frame's shader uniforms and palette, read from protocol `frame` JSON.
struct Geometry {
    rays: usize,
    gates: usize,
    first_gate_m: f32,
    gate_spacing_m: f32,
    elevation_deg: f32,
    /// The site's longitude and latitude in degrees.
    site: (f64, f64),
    palette: Vec<[u8; 3]>,
    /// The weak-return floor in code units (`RadarMap.qml`'s `weakBelow`):
    /// measured codes below it draw nothing; 0 is no floor.
    weak_below: u32,
}
impl Geometry {
    fn of(frame: &Value) -> Self {
        let palette = frame["palette"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hex| {
                let hex = hex.as_str().unwrap().trim_start_matches('#');
                let channel = |i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap();
                [channel(0), channel(2), channel(4)]
            })
            .collect();
        Self {
            rays: frame["rays"].as_u64().unwrap() as usize,
            gates: frame["gates"].as_u64().unwrap() as usize,
            first_gate_m: frame["firstGateM"].as_f64().unwrap() as f32,
            gate_spacing_m: frame["gateSpacingM"].as_f64().unwrap() as f32,
            elevation_deg: frame["elevationDeg"].as_f64().unwrap() as f32,
            site: (
                frame["site"]["lon"].as_f64().unwrap(),
                frame["site"]["lat"].as_f64().unwrap(),
            ),
            palette,
            weak_below: 0,
        }
    }
    /// The palette class of a measured code, as `sweep.rs` assigns it.
    fn class(code: u8, scale: f32, offset: f32, bounds: &[i64], classes: usize) -> u8 {
        let value = (f32::from(code) - offset) / scale;
        let above = bounds.partition_point(|&b| b as f32 <= value);
        above.saturating_sub(1).min(classes - 1) as u8
    }
}

/// `ui/RadarMap.qml`'s Web Mercator frame: the unit square is the world.
fn mercator(lon: f64, lat: f64) -> (f64, f64) {
    let phi = lat.clamp(-MAX_LATITUDE, MAX_LATITUDE).to_radians();
    ((lon + 180.0) / 360.0, (1.0 - phi.tan().asinh() / PI) / 2.0)
}
fn lon_lat(mx: f64, my: f64) -> (f64, f64) {
    (
        mx * 360.0 - 180.0,
        (PI * (1.0 - 2.0 * my)).sinh().atan().to_degrees(),
    )
}
/// A camera, in `RadarMap`'s terms, with the centre as kilometres east and
/// north of the site (the frame the views were chosen in).
struct View {
    name: &'static str,
    width: u32,
    height: u32,
    offset_km: (f64, f64),
    span: f64,
}
/// The camera as the shader receives it.
struct Uniforms {
    center_offset: [f32; 2],
    units_per_pixel: f32,
    site_lat_deg: f32,
}
impl View {
    /// Ground kilometres per Mercator unit at a longitude/latitude point.
    fn km_per_unit(site: (f64, f64)) -> f64 {
        2.0 * PI * 6371.0 * site.1.to_radians().cos()
    }
    /// The longitude and latitude the harness hands to `center`.
    fn center(&self, site: (f64, f64)) -> (f64, f64) {
        let (mx, my) = mercator(site.0, site.1);
        let k = Self::km_per_unit(site);
        lon_lat(mx + self.offset_km.0 / k, my - self.offset_km.1 / k)
    }
    /// `ui/RadarMap.qml`'s camera as the shader receives it: the view centre,
    /// clamped inside the tile pyramid, relative to the site in Mercator units,
    /// the scale, and the site's latitude, computed in double and uploaded as
    /// floats.
    fn uniforms(&self, site: (f64, f64)) -> Uniforms {
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        let (lon, lat) = self.center(site);
        // RadarMap scales an explicit camera center at the center latitude,
        // while siteLatDeg remains the dish latitude for radar geometry.
        let k = Self::km_per_unit((lon, lat));
        let pixels_per_km = (w.min(h) / self.span.max(25.0)).max(w.max(h) / k);
        let half_x = w / (2.0 * pixels_per_km * k);
        let half_y = h / (2.0 * pixels_per_km * k);
        let (sx, sy) = mercator(site.0, site.1);
        let (wx, wy) = mercator(lon, lat);
        Uniforms {
            center_offset: [
                (wx.clamp(half_x, 1.0 - half_x) - sx) as f32,
                (wy.clamp(half_y, 1.0 - half_y) - sy) as f32,
            ],
            units_per_pixel: (1.0 / (pixels_per_km * k)) as f32,
            site_lat_deg: site.1 as f32,
        }
    }
}

/// The shader's reduced atan2 series, checked separately against libm below.
fn bearing_atan(y: f32, x: f32) -> f32 {
    let (ax, ay) = (x.abs(), y.abs());
    let mut t = ax.min(ay) / ax.max(ay).max(1e-30);
    let reduce = t > 0.414_213_57;
    if reduce {
        t = (t - 1.0) / (t + 1.0);
    }
    let t2 = t * t;
    let mut a = t
        * (1.0
            - t2 * (1.0 / 3.0
                - t2 * (1.0 / 5.0
                    - t2 * (1.0 / 7.0
                        - t2 * (1.0 / 9.0 - t2 * (1.0 / 11.0 - t2 * (1.0 / 13.0 - t2 / 15.0)))))));
    if reduce {
        a += std::f32::consts::FRAC_PI_4;
    }
    if ay > ax {
        a = std::f32::consts::FRAC_PI_2 - a;
    }
    if x < 0.0 {
        a = std::f32::consts::PI - a;
    }
    if y < 0.0 { -a } else { a }
}

#[test]
fn bearing_matches_independent_atan2_in_every_quadrant() {
    for i in -36000..=36000 {
        let a = f64::from(i) * std::f64::consts::PI / 36000.0;
        let (y, x) = (a.sin() as f32, a.cos() as f32);
        assert!((f64::from(bearing_atan(y, x)) - f64::from(y).atan2(f64::from(x))).abs() < 4e-7);
    }
}

/// Replay the shader's Mercator-to-gate and azimuth lookup in single precision.
fn sample(x: u32, y: u32, view: &View, geometry: &Geometry) -> (f32, f32) {
    let u = view.uniforms(geometry.site);
    let cell_center = |p: u32| ((p / 3) * 3) as f32 + 1.5;
    let dx = u.center_offset[0] + (cell_center(x) - view.width as f32 * 0.5) * u.units_per_pixel;
    let dy = u.center_offset[1] + (cell_center(y) - view.height as f32 * 0.5) * u.units_per_pixel;
    let cosh = |v: f32| 0.5 * (v.exp() + (-v).exp());
    // The shader's series for small arguments (see `sinh_` and `sin_` there).
    let sinh = |v: f32| {
        if v.abs() >= 1.0 {
            return 0.5 * (v.exp() - (-v).exp());
        }
        let v2 = v * v;
        v * (1.0 + v2 * (1.0 / 6.0 + v2 * (1.0 / 120.0 + v2 * (1.0 / 5040.0 + v2 / 362880.0))))
    };
    let sin = |v: f32| {
        if v.abs() >= 1.0 {
            return v.sin();
        }
        let v2 = v * v;
        v * (1.0 - v2 * (1.0 / 6.0 - v2 * (1.0 / 120.0 - v2 * (1.0 / 5040.0 - v2 / 362880.0))))
    };
    let d_lon = dx * 2.0 * std::f32::consts::PI;
    let d_psi = -dy * 2.0 * std::f32::consts::PI;
    let lat0 = u.site_lat_deg.to_radians();
    let psi0 = (lat0.tan() + 1.0 / lat0.cos()).ln();
    let numerator = 2.0 * cosh(psi0 + d_psi * 0.5) * sinh(d_psi * 0.5);
    let denominator = 1.0 + lat0.tan() * sinh(psi0 + d_psi);
    let d_lat = if d_psi.abs() < 1.0 {
        bearing_atan(numerator, denominator)
    } else {
        sinh(psi0 + d_psi).atan() - lat0
    };
    let lat = lat0 + d_lat;
    let (sd_lat, sd_lon) = (sin(d_lat * 0.5), sin(d_lon * 0.5));
    let h = (sd_lat * sd_lat + lat0.cos() * lat.cos() * sd_lon * sd_lon).clamp(0.0, 1.0);
    let ground_m = 2.0 * R_M * h.sqrt().atan2((1.0 - h).max(0.0).sqrt());
    let mut azimuth = bearing_atan(
        sin(d_lon) * lat.cos(),
        sin(d_lat) + lat0.sin() * lat.cos() * 2.0 * sd_lon * sd_lon,
    )
    .to_degrees();
    if azimuth < 0.0 {
        azimuth += 360.0;
    }
    let arc = ground_m / EARTH_M;
    if geometry.elevation_deg.to_radians() + arc >= std::f32::consts::FRAC_PI_2 {
        return (-1.0, azimuth * 10.0);
    }
    let slant_m = EARTH_M * arc.sin() / (geometry.elevation_deg.to_radians() + arc).cos();
    let gate = (slant_m - geometry.first_gate_m) / geometry.gate_spacing_m;
    (gate, azimuth * 10.0)
}

/// What one pixel must show: its alpha and, when it paints, its color.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Expect {
    alpha: u8,
    coverage: f32,
    color: Option<[u8; 3]>,
}
const BLANK: Expect = Expect {
    alpha: 0,
    coverage: 0.0,
    color: None,
};
/// The shader's rule for the gate at `cell`, for pixel (x, y) in `treatment`.
fn expect(cell: Option<[u8; 3]>, x: u32, y: u32, treatment: &str, geometry: &Geometry) -> Expect {
    let Some([value, status, raw]) = cell else {
        return BLANK;
    };
    // Under the weak-return floor a measured code draws nothing; folded and
    // below-threshold codes (0, 1) are never weak.
    if geometry.weak_below > 0 && raw >= 2 && u32::from(raw) < geometry.weak_below {
        return BLANK;
    }
    let (px, py) = (x % 3, y % 3);
    if value == 0 {
        if status & FOLDED != 0 {
            let cross = px == py || px + py == 2;
            let tone = if cross { 245 } else { 24 };
            return Expect {
                alpha: 255,
                coverage: 1.0,
                color: Some([tone; 3]),
            };
        }
        return BLANK;
    }
    let bands = geometry.palette.len();
    let b = usize::from(value - 1);
    if b >= bands {
        return BLANK;
    }
    let group = b * 4 / bands;
    let alpha = match treatment {
        "PIXELS" => 1.0,
        "GLYPHS" => {
            let count = [2, 4, 7, 9][group];
            let slot = (py * 3 + px) as usize;
            if DENSITY[slot] < count { 1.0 } else { 0.0 }
        }
        "STIPPLE" => {
            let side = [1.75f32, 2.0, 2.25, 2.5][group];
            let coverage =
                |phase: u32| (side * 0.5 + 0.5 - (phase as f32 + 0.5 - 1.5).abs()).clamp(0.0, 1.0);
            coverage(px) * coverage(py)
        }
        other => panic!("unknown treatment {other}"),
    };
    Expect {
        alpha: (alpha * 255.0).round() as u8,
        coverage: alpha,
        color: (alpha > 0.0).then_some(geometry.palette[b]),
    }
}
/// Alpha within 1/255 everywhere; opaque pixels carry the swatch exactly, and
/// partially covered stipple pixels match in premultiplied space. Both alpha
/// and color were quantized independently by the GPU before Qt unpremultiplied
/// the PNG; using rounded alpha to predict color loses that distinction.
fn matches(actual: [u8; 4], expected: Expect) -> bool {
    if (i32::from(actual[3]) - i32::from(expected.alpha)).abs() > 1 {
        return false;
    }
    match expected.color {
        None => true,
        Some(color) => {
            if expected.alpha == 255 {
                return actual[..3] == color;
            }
            let alpha = f32::from(actual[3]) / 255.0;
            // Half a stored color byte, then half a straight PNG byte
            // mapped back to premultiplied space; epsilon for f32 arithmetic.
            let tolerance = 0.5 + 0.5 * alpha + 0.001;
            (0..3).all(|i| {
                (f32::from(actual[i]) * alpha - f32::from(color[i]) * expected.coverage).abs()
                    <= tolerance
            })
        }
    }
}

#[test]
fn stipple_quantization_keeps_independent_color_and_alpha_rounding() {
    let expected = Expect {
        alpha: 143,
        coverage: 0.5625,
        color: Some([216, 76, 100]),
    };
    assert!(matches([218, 77, 100, 143], expected));
    assert!(!matches([220, 77, 100, 143], expected));
}

#[derive(serde::Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
struct Report {
    size: [u32; 2],
    pixels: u64,
    painted: u64,
    /// Pixels explained only by the gate or row on the other side of a
    /// boundary the sample sits within epsilon of.
    boundary: u64,
    mismatches: u64,
}
fn check(
    capture: &Path,
    view: &View,
    geometry: &Geometry,
    sweep: &Sweep,
    treatment: &str,
) -> Report {
    let (width, height, rgba) = read_png(capture);
    assert_eq!(
        (width, height),
        (view.width, view.height),
        "{}",
        capture.display()
    );
    let mut report = Report {
        size: [width, height],
        pixels: u64::from(width) * u64::from(height),
        ..Report::default()
    };
    let mut examples = Vec::new();
    let cell_at = |gate: f32, entry: f32| -> Option<[u8; 3]> {
        if gate < -0.5 || gate >= geometry.gates as f32 - 0.5 {
            return None;
        }
        let entry = entry.floor().clamp(0.0, 3599.0) as usize;
        let row = usize::from(sweep.lut[entry]);
        Some(sweep.cell(row, (gate + 0.5).floor() as usize))
    };
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let actual: [u8; 4] = rgba[i..i + 4].try_into().unwrap();
            if actual[3] > 0 {
                report.painted += 1;
            }
            let (gate, entry) = sample(x, y, view, geometry);
            let expected = expect(cell_at(gate, entry), x, y, treatment, geometry);
            if matches(actual, expected) {
                continue;
            }
            // Near a boundary the GPU may have rounded the other way: accept
            // the neighbouring gate or row, never anything else.
            let mut gates = vec![gate];
            if ((gate + 0.5) - (gate + 0.5).round()).abs() < GATE_EPSILON {
                gates.extend([gate - 0.5, gate + 0.5]);
            }
            let mut entries = vec![entry];
            if (entry - entry.round()).abs() < ENTRY_EPSILON {
                entries.extend([entry - 0.5, entry + 0.5]);
            }
            let explained = gates.iter().any(|&g| {
                entries.iter().any(|&e| {
                    let e = if e < 0.0 {
                        e + 3600.0
                    } else if e >= 3600.0 {
                        e - 3600.0
                    } else {
                        e
                    };
                    matches(actual, expect(cell_at(g, e), x, y, treatment, geometry))
                })
            });
            if explained {
                report.boundary += 1;
            } else {
                report.mismatches += 1;
                if examples.len() < 8 {
                    examples.push(format!(
                        "({x},{y}) actual {actual:?} expected {expected:?} gate {gate:.4} entry {entry:.4}"
                    ));
                }
            }
        }
    }
    assert_eq!(
        report.mismatches,
        0,
        "{} {} {treatment}: {report:?}\n{}",
        view.name,
        capture.display(),
        examples.join("\n")
    );
    report
}

fn png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(pixels).unwrap();
    writer.finish().unwrap();
    out
}
/// Any 8-bit PNG as RGBA.
fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(fs::read(path).unwrap()));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    buffer.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        other => panic!("{}: unexpected color type {other:?}", path.display()),
    };
    (info.width, info.height, rgba)
}

/// A daemon in a private runtime directory, killed and removed with the test.
struct Engine {
    child: std::process::Child,
    root: PathBuf,
}
impl Engine {
    fn start(root: &Path) -> Self {
        // A private cache and a tile URL that refuses at once: the pan pair
        // asks for tiles, and nothing in the suite may reach the network.
        let child = Command::new(env!("CARGO_BIN_EXE_omastorm-engine"))
            .env("XDG_RUNTIME_DIR", root)
            .env(
                "OMASTORM_ARCHIVE",
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../data/raw/KTLX20130520_201643_V06.gz"
                ),
            )
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("OMASTORM_TILES_URL", "http://127.0.0.1:9/")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut engine = Self {
            child,
            root: root.to_owned(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !engine.root.join("omastorm/engine.sock").exists() {
            assert!(engine.child.try_wait().unwrap().is_none(), "engine exited");
            assert!(Instant::now() < deadline, "engine startup timed out");
            thread::sleep(Duration::from_millis(10));
        }
        engine
    }
    /// The initial `state` message.
    fn state(&self) -> Value {
        let stream = UnixStream::connect(self.root.join("omastorm/engine.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut client = BufReader::new(stream);
        let mut line = String::new();
        client.read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["type"],
            "hello"
        );
        line.clear();
        client.read_line(&mut line).unwrap();
        let state: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(state["type"], "state");
        state
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `ui/RadarMap.qml` with absolute shader paths and its shader item exposed
/// for the grab; nothing else changes.
fn install_harness(dir: &Path) {
    let component = fs::read_to_string(format!("{ROOT}/ui/RadarMap.qml")).unwrap();
    let shader = |name: &str| json!(format!("{ROOT}/ui/shaders/{name}.frag.qsb")).to_string();
    assert!(component.contains("\"shaders/radar.frag.qsb\"") && component.contains("id: map\n"));
    assert!(component.contains("\"shaders/tile.frag.qsb\""));
    let component = component
        .replace("\"shaders/radar.frag.qsb\"", &shader("radar"))
        .replace("\"shaders/tile.frag.qsb\"", &shader("tile"))
        .replace("\"shaders/grid.frag.qsb\"", &shader("grid"))
        .replacen(
            "id: map\n",
            "id: map\n    property alias shaderItem: radarEffect\n",
            1,
        );
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("RadarMap.qml"), component).unwrap();
    // The pan pair drives the real socket client for tiles; unchanged.
    fs::copy(format!("{ROOT}/ui/Engine.qml"), dir.join("Engine.qml")).unwrap();
}
/// What a capture shows: the camera, the frame, and the two texture files.
struct Scene<'a> {
    view: &'a View,
    frame: &'a Value,
    texture: &'a Path,
    lut: &'a Path,
    /// The weak-return floor in dBZ handed to `RadarMap.weakFloor`; None
    /// draws every measured return.
    weak_floor: Option<f64>,
}
/// Render the map component alone and grab its shader item to `out`.
fn render(dir: &Path, name: &str, scene: &Scene, treatment: &str, out: &Path) {
    let Scene {
        view,
        frame,
        texture,
        lut,
        weak_floor,
    } = scene;
    let (cx, cy) = view.center(Geometry::of(frame).site);
    let qml = format!(
        r##"import QtQuick
import Quickshell
ShellRoot {{
    FloatingWindow {{
        visible: true
        implicitWidth: {width}
        implicitHeight: {height}
        color: "#000000"
        RadarMap {{
            id: map
            anchors.fill: parent
            scan: ({frame})
            texture: {texture}
            azimuthLut: {lut}
            siteId: "TEST"
            theme: ({{ background: "#1a1b26", foreground: "#a9b1d6", accent: "#7aa2f7", font: "monospace", baseSize: 12 }})
            treatment: {treatment}
            weakFloor: {weak_floor}
            center: Qt.point({cx}, {cy})
            span: {span}
        }}
        Timer {{
            interval: 250; repeat: true; running: true
            property int ticks: 0
            onTriggered: {{
                ticks++;
                var ready = map.shaderItem.sweep.status === Image.Ready && map.shaderItem.azimuthLut.status === Image.Ready;
                if (ready && ticks >= 6) {{
                    stop();
                    map.shaderItem.grabToImage(result => {{ result.saveToFile({out}); Qt.quit(); }});
                }} else if (ticks >= 80) {{
                    console.log("HARNESS_TIMEOUT sweep", map.shaderItem.sweep.status, "lut", map.shaderItem.azimuthLut.status, map.error);
                    Qt.quit();
                }}
            }}
        }}
    }}
}}
"##,
        width = view.width,
        height = view.height,
        frame = frame,
        texture = json!(format!("file://{}", texture.display())),
        lut = json!(format!("file://{}", lut.display())),
        treatment = json!(treatment),
        weak_floor = weak_floor.map_or("null".to_string(), |floor| floor.to_string()),
        cx = cx,
        cy = cy,
        span = view.span,
        out = json!(out.display().to_string()),
    );
    let shell = dir.join(format!("{name}.qml"));
    fs::write(&shell, qml).unwrap();
    // Quickshell 0.3.1 logs to stdout when it is not a terminal, so both
    // streams go to the log (found 2026-09-06; the file was empty before).
    let log = fs::File::create(dir.join(format!("{name}.log"))).unwrap();
    let mut child = Command::new("quickshell")
        .arg("-p")
        .arg(&shell)
        .env("XDG_RUNTIME_DIR", dir)
        .env("QT_QPA_PLATFORM", "offscreen")
        .env("QT_QPA_PLATFORMTHEME", "basic")
        .env("QT_QUICK_BACKEND", "rhi")
        .env("QSG_RHI_BACKEND", "opengl")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .expect("quickshell on PATH");
    let deadline = Instant::now() + Duration::from_secs(40);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "{name}: quickshell did not finish; see {}",
                dir.join(format!("{name}.log")).display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    };
    let log = fs::read_to_string(dir.join(format!("{name}.log"))).unwrap_or_default();
    assert!(
        status.success() && out.exists(),
        "{name}: quickshell {status}\n{log}"
    );
    assert!(
        !log.contains("HARNESS_TIMEOUT"),
        "{name}: textures never loaded\n{log}"
    );
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Golden {
    rays: usize,
    gates: usize,
    scale: f32,
    offset: f32,
}

/// The fixture's expectations: classes from the golden codes and the frame's
/// bounds, rows from the azimuth lookup the daemon published (its nearest-ray
/// rule is proven in `sweep.rs`; four-decimal golden angles could tie).
fn golden_sweep(frame: &Value, runtime: &Path) -> Sweep {
    let golden: Golden =
        serde_json::from_slice(&fs::read(format!("{GOLDEN}sweep0.json")).unwrap()).unwrap();
    let codes = fs::read(format!("{GOLDEN}sweep0.u8")).unwrap();
    assert_eq!(codes.len(), golden.rays * golden.gates);
    let bounds: Vec<i64> = frame["bounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_i64().unwrap())
        .collect();
    let classes = frame["palette"].as_array().unwrap().len();
    let cells = codes
        .iter()
        .map(|&code| match code {
            0 => [0, BELOW_THRESHOLD, 0],
            1 => [0, FOLDED, 1],
            _ => [
                Geometry::class(code, golden.scale, golden.offset, &bounds, classes) + 1,
                0,
                code,
            ],
        })
        .collect();
    let (width, height, lut_rgba) = read_png(&runtime.join(frame["azimuthLut"].as_str().unwrap()));
    assert_eq!((width, height), (3600, 1));
    let lut = lut_rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| u16::from_le_bytes([px[0], px[1]]))
        .collect();
    let sweep = Sweep {
        gates: golden.gates,
        cells,
        lut,
    };
    // The texture the daemon serves is this same answer key, channel for channel.
    let (width, height, texture) = read_png(&runtime.join(frame["texture"].as_str().unwrap()));
    assert_eq!(
        (width as usize, height as usize),
        (golden.gates, golden.rays)
    );
    for (i, px) in texture.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!([px[0], px[1], px[2]], sweep.cells[i], "texel {i}");
    }
    sweep
}

/// Eight 45° sectors with the statuses the fixture lacks: class 1 and class
/// 12 (the palette's ends), folded, below threshold, two middle groups, a
/// per-gate class ramp, a class past the palette, and an outside-coverage
/// status, with the view reaching past the last gate.
fn synthetic_sweep(gates: usize) -> (Sweep, Vec<f32>) {
    let azimuths: Vec<f32> = (0..8).map(|k| 22.5 + 45.0 * k as f32).collect();
    let mut cells = Vec::with_capacity(8 * gates);
    for sector in 0..8 {
        for gate in 0..gates {
            cells.push(match sector {
                0 => [1, 0, 2],
                1 => [12, 0, 200],
                2 => [0, FOLDED, 1],
                3 => [0, BELOW_THRESHOLD, 0],
                4 => [4, 0, 100],
                5 => [7, 0, 120],
                6 => [(gate % 12) as u8 + 1, 0, 100],
                _ if gate < gates / 2 => [13, 0, 255],
                _ => [0, OUTSIDE, 0],
            });
        }
    }
    let lut = Sweep::lut(&azimuths);
    (Sweep { gates, cells, lut }, azimuths)
}

#[test]
#[ignore = "needs Quickshell and a desktop OpenGL context"]
fn rendering_matches_nearest_gate_expectations_in_every_treatment() {
    let root = PathBuf::from(ROOT).join(format!("target/t-render-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let harness = root.join("harness");
    install_harness(&harness);
    let review = PathBuf::from(ROOT).join("review");
    fs::create_dir_all(&review).unwrap();
    let mut reports = serde_json::Map::new();

    // The fixture, served by a private daemon, at the default view and close
    // over Moore where 250 m gates span several cells.
    let engine = Engine::start(&root);
    let state = engine.state();
    let frame = &state["frame"];
    let runtime = root.join("omastorm");
    let geometry = Geometry::of(frame);
    assert_eq!((geometry.rays, geometry.gates), (720, 1832));
    let sweep = golden_sweep(frame, &runtime);
    let texture = runtime.join(frame["texture"].as_str().unwrap());
    let lut = runtime.join(frame["azimuthLut"].as_str().unwrap());
    let mut views = vec![
        View {
            name: "default",
            width: 900,
            height: 420,
            offset_km: (-5.0, 15.0),
            span: 210.0,
        },
        View {
            name: "moore",
            width: 900,
            height: 420,
            offset_km: (-19.0, 0.6),
            span: 30.0,
        },
    ];
    // These views must be entirely empty: they are thousands of kilometres
    // from KTLX. This assertion is independent of the replayed shader rule,
    // and catches wrong latitude quadrants or slant-range branches.
    let (site_x, site_y) = mercator(geometry.site.0, geometry.site.1);
    for (name, lon, lat) in [
        ("alaska", -165.0, 65.0),
        ("date-line", 180.0, 52.0),
        ("south", -97.0, -70.0),
        ("antipode", 82.7, -35.3),
    ] {
        let (x, y) = mercator(lon, lat);
        let k = View::km_per_unit(geometry.site);
        views.push(View {
            name,
            width: 900,
            height: 420,
            offset_km: ((x - site_x) * k, (site_y - y) * k),
            span: 1000.0,
        });
    }
    for view in &views {
        for treatment in TREATMENTS {
            let name = format!("render-{}-{}", view.name, treatment.to_lowercase());
            let out = review.join(format!("{name}.png"));
            let scene = Scene {
                view,
                frame,
                texture: &texture,
                lut: &lut,
                weak_floor: None,
            };
            render(&harness, &name, &scene, treatment, &out);
            let report = check(&out, view, &geometry, &sweep, treatment);
            if !["default", "moore"].contains(&view.name) {
                assert_eq!(report.painted, 0, "Radar leaked into {}", view.name);
            }
            eprintln!("{name}: {report:?}");
            reports.insert(name, serde_json::to_value(&report).unwrap());
        }
    }
    // The weak-return floor (DESIGN.md, weak-return floor): the default view
    // in Glyphs with measured returns under 5 dBZ hidden. The code threshold
    // comes from the frame's scale and offset as `RadarMap.qml` derives it.
    {
        let floor = 5.0;
        let scale = frame["scale"].as_f64().unwrap();
        let offset = frame["offset"].as_f64().unwrap();
        let mut floored = Geometry::of(frame);
        floored.weak_below = (floor * scale + offset).ceil() as u32;
        assert_eq!(floored.weak_below, 76, "5 dBZ at scale 2, offset 66");
        let view = &views[0];
        let name = "render-default-glyphs-floor5";
        let out = review.join(format!("{name}.png"));
        let scene = Scene {
            view,
            frame,
            texture: &texture,
            lut: &lut,
            weak_floor: Some(floor),
        };
        render(&harness, name, &scene, "GLYPHS", &out);
        let report = check(&out, view, &floored, &sweep, "GLYPHS");
        let unfloored = reports["render-default-glyphs"]["painted"]
            .as_u64()
            .unwrap();
        assert!(
            report.painted > 0 && report.painted < unfloored,
            "{name}: {} painted, {unfloored} without the floor",
            report.painted
        );
        eprintln!("{name}: {report:?}");
        reports.insert(name.to_string(), serde_json::to_value(&report).unwrap());
    }
    drop(engine);

    // Hand-built statuses through a harness-only frame: same palette and
    // bounds, eight rays, forty gates, the site just off the view center so
    // no cell center sits exactly on a sector edge.
    let gates = 40;
    let (sweep, _) = synthetic_sweep(gates);
    let texture = root.join("synthetic-sweep.png");
    let lut = root.join("synthetic-azlut.png");
    fs::write(&texture, sweep.texture_png(8)).unwrap();
    fs::write(&lut, sweep.lut_png()).unwrap();
    let mut frame = frame.clone();
    frame["rays"] = json!(8);
    frame["gates"] = json!(gates);
    frame["texture"] = json!("tex/synthetic-sweep.png");
    frame["azimuthLut"] = json!("tex/synthetic-azlut.png");
    let geometry = Geometry::of(&frame);
    let view = View {
        name: "synthetic",
        width: 900,
        height: 420,
        offset_km: (0.37, -0.23),
        span: 30.0,
    };
    for treatment in TREATMENTS {
        let name = format!("folded-{}", treatment.to_lowercase());
        let out = review.join(format!("{name}.png"));
        let scene = Scene {
            view: &view,
            frame: &frame,
            texture: &texture,
            lut: &lut,
            weak_floor: None,
        };
        render(&harness, &name, &scene, treatment, &out);
        let report = check(&out, &view, &geometry, &sweep, treatment);
        assert!(report.painted > 0, "{name}: nothing painted");
        eprintln!("{name}: {report:?}");
        reports.insert(name, serde_json::to_value(&report).unwrap());
    }

    let report = json!({
        "check": "engine/tests/rendering.rs",
        "rule": "3 px cell center in Web Mercator to lon/lat, great-circle distance and bearing from the site on the 6371 km sphere, ground to slant range on the 4/3 earth, nearest gate, row from the azimuth lookup, class from bounds, nothing under the weak-return floor",
        "tolerance": {"alpha": "1/255", "opaqueColor": "exact", "partialColor": "premultiplied rounding", "boundary": {"gates": GATE_EPSILON, "entries": ENTRY_EPSILON}},
        "captures": reports,
    });
    fs::write(
        review.join("render-validation.json"),
        format!("{}\n", serde_json::to_string_pretty(&report).unwrap()),
    )
    .unwrap();
    let _ = fs::remove_dir_all(&root);
}

/// The pan pair's camera: the review size, a span whose tiles come from
/// Natural Earth (z6, so the tile path fetches nothing), and a shift that is
/// a whole number of 3 px cells on both axes, so the shader's screen-anchored
/// cell grid samples the same gates before and after.
const PAN_WIDTH: u32 = 960;
const PAN_HEIGHT: u32 = 680;
const PAN_SPAN: f64 = 560.0;
const PAN_DX: i32 = 48;
const PAN_DY: i32 = -27;
/// Labels near an edge appear or vanish with the viewport's own clipping
/// rule; compare well inside the overlap so only motion is measured.
const PAN_MARGIN: u32 = 128;

/// Render the whole map (radar, tiles, overlay) over the daemon's socket,
/// grab it, pan by (`PAN_DX`, `PAN_DY`) pixels, and grab again. Returns the
/// harness log.
fn render_pan_pair(dir: &Path, runtime_root: &Path, site: (f64, f64), out: [&Path; 2]) -> String {
    let view = View {
        name: "pan",
        width: PAN_WIDTH,
        height: PAN_HEIGHT,
        offset_km: (-5.0, 15.0),
        span: PAN_SPAN,
    };
    let (cx, cy) = view.center(site);
    let qml = format!(
        r##"import QtQuick
import Quickshell
ShellRoot {{
    Engine {{ id: engine }}
    FloatingWindow {{
        visible: true
        implicitWidth: {width}
        implicitHeight: {height}
        color: "#000000"
        RadarMap {{
            id: map
            anchors.fill: parent
            scan: engine.state ? engine.state.frame : null
            texture: engine.texture
            azimuthLut: engine.azimuthLut
            siteId: engine.selectedSiteId
            sites: engine.sites
            tileRoot: "file://" + engine.runtime
            theme: ({{ background: "#1a1b26", foreground: "#a9b1d6", accent: "#7aa2f7", font: "monospace", baseSize: 12 }})
            treatment: "GLYPHS"
            center: Qt.point({cx}, {cy})
            span: {span}
            onTilesNeeded: (z, x0, y0, x1, y1) => engine.send({{type: "tiles_needed", z: z, x0: x0, y0: y0, x1: x1, y1: y1}})
        }}
        Connections {{ target: engine; function onTileReady(tile) {{ map.tileReady(tile); }} }}
    }}
    // Every tile of the requested rectangle drawn at its level, and both
    // radar textures uploaded.
    function ready() {{
        if (!map.scan || !map.request || map.displayedLevel !== map.request.z) return false;
        for (var y = map.request.y0; y <= map.request.y1; y++)
            for (var x = map.request.x0; x <= map.request.x1; x++) {{
                var tile = map.tiles[map.request.z + "/" + x + "/" + y];
                if (!tile || !tile.ready) return false;
            }}
        return map.shaderItem.sweep.status === Image.Ready && map.shaderItem.azimuthLut.status === Image.Ready;
    }}
    property int stage: 0
    property int settled: 0
    property var heldLabels
    property var heldRequest
    Timer {{
        interval: 250; repeat: true; running: true
        property int ticks: 0
        onTriggered: {{
            ticks++;
            if (ticks >= 120) {{ console.log("HARNESS_TIMEOUT stage", stage, "request", JSON.stringify(map.request), "displayed", map.displayedLevel, map.error, engine.error); Qt.quit(); return; }}
            if (stage < 0 || !ready()) {{ settled = 0; return; }}
            // A few quiet ticks let asynchronous tile images settle.
            if (++settled < 4) return;
            if (stage === 0) {{
                stage = -1;
                heldLabels = map.labels;
                heldRequest = JSON.stringify(map.request);
                console.log("PAN_CAMERA", map.viewCenterX, map.viewCenterY, map.unitsPerPixel, map.tileZoom, heldRequest);
                map.grabToImage(result => {{
                    result.saveToFile({out_a});
                    map.look(map.viewCenterX + {dx} * map.unitsPerPixel, map.viewCenterY + {dy} * map.unitsPerPixel);
                    settled = 0; stage = 1;
                }});
            }} else if (stage === 1) {{
                stage = -1;
                if (map.labels !== heldLabels) console.log("PAN_RELAYOUT");
                if (JSON.stringify(map.request) !== heldRequest) console.log("PAN_REQUEST_CHANGED", JSON.stringify(map.request));
                map.grabToImage(result => {{ result.saveToFile({out_b}); console.log("PAN_DONE"); Qt.quit(); }});
            }}
        }}
    }}
}}
"##,
        width = PAN_WIDTH,
        height = PAN_HEIGHT,
        cx = cx,
        cy = cy,
        span = PAN_SPAN,
        dx = PAN_DX,
        dy = PAN_DY,
        out_a = json!(out[0].display().to_string()),
        out_b = json!(out[1].display().to_string()),
    );
    let shell = dir.join("pan.qml");
    fs::write(&shell, qml).unwrap();
    let log_path = dir.join("pan.log");
    let log = fs::File::create(&log_path).unwrap();
    let mut child = Command::new("quickshell")
        .arg("-p")
        .arg(&shell)
        .env("XDG_RUNTIME_DIR", runtime_root)
        .env("QT_QPA_PLATFORM", "offscreen")
        .env("QT_QPA_PLATFORMTHEME", "basic")
        .env("QT_QUICK_BACKEND", "rhi")
        .env("QSG_RHI_BACKEND", "opengl")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .expect("quickshell on PATH");
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pan: quickshell did not finish; see {}", log_path.display());
        }
        thread::sleep(Duration::from_millis(50));
    };
    let log = fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        status.success() && out[0].exists() && out[1].exists() && log.contains("PAN_DONE"),
        "pan: quickshell {status}\n{log}"
    );
    assert!(
        !log.contains("HARNESS_TIMEOUT"),
        "pan: map never settled\n{log}"
    );
    log
}

#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct PanReport {
    size: [u32; 2],
    shift: [i32; 2],
    /// The compared rectangle of the second capture, `[x, y, width, height]`.
    region: [u32; 4],
    compared: u64,
    /// Pixels at the same coordinates that differ between the captures: the
    /// content moved.
    moved: u64,
    /// Pixels carrying a palette swatch exactly (the radar) and pixels that
    /// are neither background nor swatch (tiles and overlay) in the region.
    radar: u64,
    other: u64,
    /// Pixels of the second capture that differ from the first shifted by the
    /// pan within rounding: one unit of alpha, and of color one unit plus
    /// what one premultiplied unit is worth at that alpha (`256 / alpha`),
    /// the GPU's blend rounding at a new offset. A fraction of a pixel of
    /// run-to-run noise, never whole rows or columns.
    noise: u64,
    /// Pixels that differ by more. Must be zero: the radar, the tiles, and
    /// the overlay share one camera, so a pan moves every layer by the same
    /// whole pixels, and a layer that lagged would show as whole rows or
    /// columns of misplaced content.
    mismatches: u64,
}

#[test]
#[ignore = "needs Quickshell and a desktop OpenGL context"]
fn rendering_pans_radar_tiles_and_overlay_together() {
    let root = PathBuf::from(ROOT).join(format!("target/t-pan-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let harness = root.join("harness");
    install_harness(&harness);
    let review = PathBuf::from(ROOT).join("review");
    fs::create_dir_all(&review).unwrap();

    let engine = Engine::start(&root);
    let state = engine.state();
    let geometry = Geometry::of(&state["frame"]);
    let before = review.join("pan-before.png");
    let after = review.join("pan-after.png");
    let log = render_pan_pair(&harness, &root, geometry.site, [&before, &after]);
    assert!(
        !log.contains("PAN_RELAYOUT"),
        "a pan inside the padded overlay re-laid out the labels\n{log}"
    );
    assert!(
        !log.contains("PAN_REQUEST_CHANGED"),
        "the pan crossed a tile edge; move the view so both captures share one tile rectangle\n{log}"
    );
    drop(engine);

    let (w, h, a) = read_png(&before);
    let (w2, h2, b) = read_png(&after);
    assert_eq!((w, h), (PAN_WIDTH, PAN_HEIGHT));
    assert_eq!((w2, h2), (w, h));
    let pixel = |rgba: &[u8], x: u32, y: u32| -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        rgba[i..i + 4].try_into().unwrap()
    };
    // Pixel (x, y) of the second capture shows what (x + dx, y + dy) showed
    // in the first, since the camera moved by (dx, dy). Compare the overlap
    // shrunk by the margin on every side.
    let (dx, dy) = (PAN_DX, PAN_DY);
    let x0 = PAN_MARGIN as i32 + (-dx).max(0);
    let y0 = PAN_MARGIN as i32 + (-dy).max(0);
    let x1 = w as i32 - PAN_MARGIN as i32 - dx.max(0);
    let y1 = h as i32 - PAN_MARGIN as i32 - dy.max(0);
    assert!(x1 > x0 + 200 && y1 > y0 + 200, "compared region too small");
    let mut report = PanReport {
        size: [w, h],
        shift: [dx, dy],
        region: [x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32],
        compared: 0,
        moved: 0,
        radar: 0,
        other: 0,
        noise: 0,
        mismatches: 0,
    };
    let mut examples = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let (x, y) = (x as u32, y as u32);
            let actual = pixel(&b, x, y);
            let expected = pixel(&a, (x as i32 + dx) as u32, (y as i32 + dy) as u32);
            report.compared += 1;
            if actual != pixel(&a, x, y) {
                report.moved += 1;
            }
            let rgb = [actual[0], actual[1], actual[2]];
            if geometry.palette.contains(&rgb) {
                report.radar += 1;
            } else if rgb != [0, 0, 0] {
                report.other += 1;
            }
            let color_tolerance = 2 + 256 / u32::from(expected[3].max(1));
            let within_rounding = actual[3].abs_diff(expected[3]) <= 1
                && (0..3).all(|i| u32::from(actual[i].abs_diff(expected[i])) <= color_tolerance);
            if actual != expected && within_rounding {
                report.noise += 1;
            } else if actual != expected {
                report.mismatches += 1;
                if examples.len() < 8 {
                    examples.push(format!(
                        "({x},{y}) after {actual:?} before-shifted {expected:?}"
                    ));
                }
            }
        }
    }
    eprintln!("pan: {report:?}");
    // Something of every layer must be in the region, and it must have moved.
    assert!(
        report.radar > 1000,
        "too little radar in the compared region: {report:?}"
    );
    assert!(
        report.other > 1000,
        "too little basemap or overlay in the compared region: {report:?}"
    );
    assert!(
        report.moved > report.compared / 20,
        "the pan moved almost nothing: {report:?}"
    );
    assert_eq!(
        report.mismatches,
        0,
        "layers did not move together: {report:?}\n{}",
        examples.join("\n")
    );
    fs::write(
        review.join("pan-validation.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "check": "engine/tests/rendering.rs (pan pair)",
                "rule": "the second capture equals the first shifted by the pan within premultiplied rounding, well inside the overlap, so radar, tiles, and overlay moved by the same whole pixels",
                "capture": report,
            }))
            .unwrap()
        ),
    )
    .unwrap();
    let _ = fs::remove_dir_all(&root);
}
