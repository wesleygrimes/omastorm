//! Basemap tile masks (DESIGN.md, basemap tiles; `docs/protocol.md`, tiles):
//! the tile pyramid, the stroke rasterizer both sets share, the `ne` set from
//! the embedded Natural Earth geography, and the store of masks published
//! this session. A tile is the standard Web Mercator XYZ cell at 512 px: one
//! antialiased coverage `Mask` per layer, stroked with `tiny-skia`,
//! interleaved into an RGBA PNG whose R channel is boundaries, G coastlines
//! and lake shores, B minor roads, and A 255 minus major-road coverage: Qt
//! premultiplies textures by A on upload, so an empty tile must be opaque for
//! its other channels to survive (DESIGN.md, basemap tiles). Roads exist only
//! in `osm` data (`osm.rs`), so an `ne` tile has B zero and A 255 everywhere.
//! Rendering is deterministic, so the same tile always produces the same
//! bytes under the same name.

use crate::protocol::{Label, is_tile_path};
use serde::Deserialize;
use std::{
    collections::{HashMap, VecDeque},
    f64::consts::PI,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};
use tiny_skia::{FillRule, LineCap, LineJoin, Mask, PathBuilder, Stroke, Transform};

/// Tile edge in pixels; a 512 px tile covers the ground of the 256 px tile
/// with the same `z/x/y` at twice the density.
pub const SIZE: u32 = 512;
/// The most tiles one `tiles_needed` may ask for (`docs/protocol.md`).
pub const MAX_REQUEST: u32 = 64;
/// `2^22` tiles across is already 40 m per tile at the equator; deeper
/// requests are mistakes, not views.
pub const MAX_ZOOM: u32 = 22;
/// Rendered masks kept in the runtime directory, oldest announced dropped.
pub const MAX_FILES: usize = 4096;
/// The 1:50m set draws below this zoom, the 1:10m set from it (DESIGN.md).
const DETAIL_FROM: u32 = 5;
/// Stroke widths in tile pixels: a proposal for the capture review, not a
/// decision. Boundaries a touch heavier than water, both quiet.
pub(crate) const BOUNDARY_WIDTH: f32 = 1.5;
pub(crate) const COAST_WIDTH: f32 = 1.25;
/// Geometry this far outside the tile still touches it through its stroke.
const MARGIN_PX: f64 = 4.0;
/// Coordinates in the blob are degrees times this (`build.rs`).
const QUANTUM: f64 = 1e-5;
/// Web Mercator's latitude limit: the square world.
const MAX_LAT: f64 = 85.051_128_779_806_6;
/// The Natural Earth release the embedded files come from: upstream master's
/// `VERSION` file on 2026-09-06, the day `data/SHA256SUMS` pinned them.
pub const NE_VERSION: &str = "5.2.0-pre";

const BLOB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ne.bin"));
const PLACES: &str = include_str!(concat!(env!("OUT_DIR"), "/places.json"));
const GAZETTEER: &str = include_str!(concat!(env!("OUT_DIR"), "/gazetteer.json"));

/// Which data drew a tile (`docs/protocol.md`, `tile_ready.set`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Set {
    /// Natural Earth, embedded in the binary, any zoom.
    Ne,
    /// A fetched OpenMapTiles-schema vector tile (`osm.rs`).
    Osm,
}
impl Set {
    pub fn name(self) -> &'static str {
        match self {
            Set::Ne => "ne",
            Set::Osm => "osm",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileKey {
    pub z: u32,
    pub x: u32,
    pub y: u32,
}
impl TileKey {
    /// The tile at zoom `z` holding a coordinate.
    #[cfg(test)]
    pub fn containing(z: u32, lon: f64, lat: f64) -> Self {
        let (mx, my) = mercator(lon, lat);
        let n = f64::from(1u32 << z);
        let clamp = |v: f64| (v * n).floor().clamp(0.0, n - 1.0) as u32;
        Self {
            z,
            x: clamp(mx),
            y: clamp(my),
        }
    }
    /// Longitude and latitude of a point given in this tile's pixels.
    pub fn lon_lat(self, px: f64, py: f64) -> (f64, f64) {
        let n = f64::from(1u32 << self.z);
        let mx = (f64::from(self.x) + px / f64::from(SIZE)) / n;
        let my = (f64::from(self.y) + py / f64::from(SIZE)) / n;
        (mx * 360.0 - 180.0, latitude(my))
    }
}

/// A validated `tiles_needed`: an inclusive rectangle at one zoom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Request {
    pub z: u32,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}
impl Request {
    /// Check the rectangle against the pyramid and the request ceiling,
    /// wording the refusal for the client.
    pub fn validate(self) -> Result<Self, String> {
        if self.z > MAX_ZOOM {
            return Err(format!("zoom {} is beyond {MAX_ZOOM}", self.z));
        }
        let n = 1u64 << self.z;
        if self.x1 < self.x0 || self.y1 < self.y0 {
            return Err("x1 and y1 must not be less than x0 and y0".into());
        }
        if u64::from(self.x1) >= n || u64::from(self.y1) >= n {
            return Err(format!("tiles at zoom {} run 0 to {}", self.z, n - 1));
        }
        let count = u64::from(self.x1 - self.x0 + 1) * u64::from(self.y1 - self.y0 + 1);
        if count > u64::from(MAX_REQUEST) {
            return Err(format!(
                "{count} tiles requested; at most {MAX_REQUEST} at once"
            ));
        }
        Ok(self)
    }
    pub fn contains(&self, key: TileKey) -> bool {
        key.z == self.z
            && (self.x0..=self.x1).contains(&key.x)
            && (self.y0..=self.y1).contains(&key.y)
    }
    /// Every tile in the rectangle, nearest the centre first.
    pub fn centre_out(&self) -> Vec<TileKey> {
        let cx = f64::from(self.x0) + f64::from(self.x1 - self.x0) / 2.0;
        let cy = f64::from(self.y0) + f64::from(self.y1 - self.y0) / 2.0;
        let mut keys: Vec<TileKey> = (self.y0..=self.y1)
            .flat_map(|y| (self.x0..=self.x1).map(move |x| TileKey { z: self.z, x, y }))
            .collect();
        keys.sort_by(|a, b| {
            let distance =
                |k: &TileKey| (f64::from(k.x) - cx).powi(2) + (f64::from(k.y) - cy).powi(2);
            distance(a)
                .total_cmp(&distance(b))
                .then(a.y.cmp(&b.y))
                .then(a.x.cmp(&b.x))
        });
        keys
    }
}

/// Longitude and latitude to the Web Mercator unit square.
fn mercator(lon: f64, lat: f64) -> (f64, f64) {
    let phi = lat.clamp(-MAX_LAT, MAX_LAT).to_radians();
    (
        (lon + 180.0) / 360.0,
        (1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / PI) / 2.0,
    )
}
fn latitude(my: f64) -> f64 {
    (PI * (1.0 - 2.0 * my)).sinh().atan().to_degrees()
}

struct Polyline {
    points: Vec<(i32, i32)>,
    /// Quantized `lon, lat` bounds: min x, min y, max x, max y.
    bounds: [i32; 4],
}
struct Layer {
    polylines: Vec<Polyline>,
}
/// One Natural Earth scale: boundaries, then coast.
struct Scale {
    layers: [Layer; 2],
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Place {
    name: String,
    lat: f64,
    lon: f64,
    class: String,
    rank: u32,
    #[serde(default)]
    min_zoom: f64,
    #[serde(default)]
    region: String,
    #[serde(default)]
    country: String,
}
impl Place {
    fn label(&self) -> Label {
        Label {
            name: self.name.clone(),
            lat: self.lat,
            lon: self.lon,
            class: self.class.clone(),
            rank: self.rank,
            region: self.region.clone(),
            country: self.country.clone(),
        }
    }
}
/// The embedded geography, decoded once on first use.
pub struct Geography {
    /// 1:50m world, then 1:10m network envelope.
    sets: [Scale; 2],
    places: Vec<Place>,
}
impl Geography {
    pub fn embedded() -> &'static Geography {
        static GEOGRAPHY: OnceLock<Geography> = OnceLock::new();
        GEOGRAPHY.get_or_init(|| Geography::decode(BLOB, PLACES).expect("embedded geography"))
    }
    fn decode(blob: &[u8], places: &str) -> io::Result<Geography> {
        let mut reader = Varints {
            bytes: blob
                .strip_prefix(b"OMNE\x01")
                .ok_or_else(|| io::Error::other("geography blob header"))?,
        };
        let mut layer = || -> io::Result<Layer> {
            let count = reader.next()?;
            let mut polylines = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let vertices = reader.next()?;
                let mut points = Vec::with_capacity(vertices as usize);
                let (mut x, mut y) = (0i32, 0i32);
                let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
                for _ in 0..vertices {
                    x = x.wrapping_add(reader.signed()?);
                    y = y.wrapping_add(reader.signed()?);
                    points.push((x, y));
                    bounds = [
                        bounds[0].min(x),
                        bounds[1].min(y),
                        bounds[2].max(x),
                        bounds[3].max(y),
                    ];
                }
                polylines.push(Polyline { points, bounds });
            }
            Ok(Layer { polylines })
        };
        let coarse = Scale {
            layers: [layer()?, layer()?],
        };
        let fine = Scale {
            layers: [layer()?, layer()?],
        };
        if !reader.bytes.is_empty() {
            return Err(io::Error::other("geography blob has trailing bytes"));
        }
        Ok(Geography {
            sets: [coarse, fine],
            places: serde_json::from_str(places).map_err(io::Error::other)?,
        })
    }
    fn scale(&self, z: u32) -> &Scale {
        &self.sets[usize::from(z >= DETAIL_FROM)]
    }
}

/// GeoNames cities with population ≥ 5000, clipped to the station envelope.
/// Location search uses this; map labels stay on Natural Earth `places`.
fn gazetteer() -> &'static [Place] {
    static GAZETTEER_PLACES: OnceLock<Vec<Place>> = OnceLock::new();
    GAZETTEER_PLACES
        .get_or_init(|| serde_json::from_str(GAZETTEER).expect("embedded gazetteer"))
        .as_slice()
}
struct Varints<'a> {
    bytes: &'a [u8],
}
impl Varints<'_> {
    fn next(&mut self) -> io::Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self
                .bytes
                .split_first()
                .ok_or_else(|| io::Error::other("geography blob ends inside a number"))?;
            self.bytes = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(io::Error::other("geography blob number too long"))
    }
    fn signed(&mut self) -> io::Result<i32> {
        let raw = self.next()?;
        let value = ((raw >> 1) as i64) ^ -((raw & 1) as i64);
        i32::try_from(value).map_err(|_| io::Error::other("geography blob coordinate out of range"))
    }
}

/// Where a tile sits: pixel projection plus the quantized lon/lat box that a
/// polyline must touch (with the stroke margin) to be drawn.
struct Frame {
    scale: f64,
    origin_x: f64,
    origin_y: f64,
    bounds: [i32; 4],
}
impl Frame {
    fn new(key: TileKey) -> Self {
        let n = f64::from(1u32 << key.z);
        let scale = n * f64::from(SIZE);
        let margin = MARGIN_PX / scale;
        let x0 = f64::from(key.x) / n - margin;
        let x1 = f64::from(key.x + 1) / n + margin;
        let y0 = f64::from(key.y) / n - margin;
        let y1 = f64::from(key.y + 1) / n + margin;
        let q = |degrees: f64| {
            (degrees / QUANTUM)
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        };
        Self {
            scale,
            origin_x: f64::from(key.x) * f64::from(SIZE),
            origin_y: f64::from(key.y) * f64::from(SIZE),
            bounds: [
                q(x0 * 360.0 - 180.0),
                q(latitude(y1)),
                q(x1 * 360.0 - 180.0),
                q(latitude(y0)),
            ],
        }
    }
    fn touches(&self, bounds: &[i32; 4]) -> bool {
        bounds[2] >= self.bounds[0]
            && bounds[0] <= self.bounds[2]
            && bounds[3] >= self.bounds[1]
            && bounds[1] <= self.bounds[3]
    }
    fn pixel(&self, (x, y): (i32, i32)) -> (f64, f64) {
        let (mx, my) = mercator(f64::from(x) * QUANTUM, f64::from(y) * QUANTUM);
        (
            mx * self.scale - self.origin_x,
            my * self.scale - self.origin_y,
        )
    }
    fn contains_lon_lat(&self, lon: f64, lat: f64) -> bool {
        let q = |degrees: f64| (degrees / QUANTUM).round() as i32;
        (self.bounds[0]..=self.bounds[2]).contains(&q(lon))
            && (self.bounds[1]..=self.bounds[3]).contains(&q(lat))
    }
}

/// Polylines in tile pixels accumulating into one stroked coverage mask.
/// Segments that cannot reach the tile are skipped, so a continent-long
/// coastline costs only its local part; so are segments with both ends past
/// one edge, which the neighbouring tile draws and which is where a vector
/// tile's clipped polygons run along its buffer.
pub struct Strokes {
    builder: PathBuilder,
}
impl Strokes {
    pub fn new() -> Self {
        Self {
            builder: PathBuilder::new(),
        }
    }
    pub fn polyline(&mut self, points: impl IntoIterator<Item = (f64, f64)>) {
        let low = -MARGIN_PX;
        let high = f64::from(SIZE) + MARGIN_PX;
        let edge = f64::from(SIZE);
        let visible = |a: (f64, f64), b: (f64, f64)| {
            let (min_x, max_x) = (a.0.min(b.0), a.0.max(b.0));
            let (min_y, max_y) = (a.1.min(b.1), a.1.max(b.1));
            max_x >= low
                && min_x <= high
                && max_y >= low
                && min_y <= high
                && !(max_x <= 0.0 || min_x >= edge || max_y <= 0.0 || min_y >= edge)
        };
        let mut previous: Option<(f64, f64)> = None;
        let mut open = false;
        for current in points {
            if let Some(last) = previous {
                if visible(last, current) {
                    if !open {
                        self.builder.move_to(last.0 as f32, last.1 as f32);
                        open = true;
                    }
                    self.builder.line_to(current.0 as f32, current.1 as f32);
                } else {
                    open = false;
                }
            }
            previous = Some(current);
        }
    }
    /// Stroke everything added so far, `width` pixels wide with round caps
    /// and joins, into an antialiased coverage mask.
    pub fn mask(self, width: f32) -> Mask {
        let mut mask = Mask::new(SIZE, SIZE).expect("tile mask");
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        if let Some(path) = self
            .builder
            .finish()
            .and_then(|path| path.stroke(&stroke, 1.0))
        {
            mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        }
        mask
    }
}

/// Stroke one Natural Earth layer into a coverage mask.
fn rasterize(layer: &Layer, frame: &Frame, width: f32) -> Mask {
    let mut strokes = Strokes::new();
    for polyline in &layer.polylines {
        if frame.touches(&polyline.bounds) {
            strokes.polyline(polyline.points.iter().map(|&point| frame.pixel(point)));
        }
    }
    strokes.mask(width)
}

/// Interleave the layer masks into the protocol's RGBA pixels: R boundaries,
/// G water, B minor roads, A 255 minus major roads. `roads` is `(minor,
/// major)`; without it (an `ne` tile) B is zero and A 255 everywhere.
pub fn compose(boundaries: &Mask, water: &Mask, roads: Option<(&Mask, &Mask)>) -> Vec<u8> {
    let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];
    for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        pixel[0] = boundaries.data()[i];
        pixel[1] = water.data()[i];
        pixel[3] = 255;
        if let Some((minor, major)) = roads {
            pixel[2] = minor.data()[i];
            pixel[3] = 255 - major.data()[i];
        }
    }
    pixels
}

/// Render an `ne` tile to PNG bytes: R boundaries, G coast, B zero, and A
/// 255 (no major roads to subtract).
pub fn render(geography: &Geography, key: TileKey) -> io::Result<Vec<u8>> {
    let frame = Frame::new(key);
    let scale = geography.scale(key.z);
    let boundaries = rasterize(&scale.layers[0], &frame, BOUNDARY_WIDTH);
    let coast = rasterize(&scale.layers[1], &frame, COAST_WIDTH);
    crate::sweep::png(SIZE, SIZE, &compose(&boundaries, &coast, None))
}

/// Ranked gazetteer places matching `query` for the location picker
/// (GeoNames ≥ 5000 people in the network envelope). Word-start matches
/// beat substrings; nearer the optional origin, then lower rank, win
/// within a tier. Empty or blank queries return nothing.
pub fn search_places(query: &str, origin: Option<(f64, f64)>, limit: usize) -> Vec<Label> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(u32, f64, u32, &Place)> = Vec::new();
    for place in gazetteer() {
        let name = place.name.to_lowercase();
        let tier = if word_start(&name, &needle) {
            0
        } else if name.contains(&needle) {
            1
        } else {
            continue;
        };
        let distance = origin.map_or(0.0, |(lat, lon)| {
            great_circle_km(lat, lon, place.lat, place.lon)
        });
        scored.push((tier, distance, place.rank, place));
    }
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.total_cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.name.cmp(&b.3.name))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, _, place)| place.label())
        .collect()
}

fn word_start(text: &str, needle: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|word| !word.is_empty() && word.starts_with(needle))
}

fn great_circle_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.clamp(0.0, 1.0).sqrt().asin()
}

/// The places inside a tile that Natural Earth shows by this zoom. A 512 px
/// tile shows the ground of four 256 px tiles one level deeper, so the data's
/// `min_zoom` is read against `z + 1`.
pub fn labels(geography: &Geography, key: TileKey) -> Vec<Label> {
    let frame = Frame::new(key);
    geography
        .places
        .iter()
        .filter(|place| place.min_zoom <= f64::from(key.z + 1))
        .filter(|place| frame.contains_lon_lat(place.lon, place.lat))
        .map(Place::label)
        .collect()
}

/// FNV-1a over `text`, as eight hex digits: a stable tag for names that
/// outlive a build (cache directories, generation tags), which
/// `DefaultHasher` does not promise.
pub fn tag(text: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:08x}", (hash >> 32) ^ (hash & 0xffff_ffff))
}

/// A published tile: its protocol path and the labels it carries.
#[derive(Clone, PartialEq, Debug)]
pub struct Ready {
    pub path: String,
    pub labels: Vec<Label>,
}

/// The rendered masks under `tiles/` this session: which exist, in the order
/// they were announced, so the directory can be capped oldest-first.
pub struct Store {
    /// The `ne` generation: eight hex digits of the build fingerprint, since
    /// the geography travels inside the binary.
    generation: String,
    /// The `osm` generation once the data version is known: eight hex digits
    /// over the build fingerprint and the version, so a new version, like a
    /// new build, publishes under new names.
    osm_generation: Option<String>,
    ready: HashMap<(Set, TileKey), Ready>,
    order: VecDeque<(Set, TileKey)>,
}
impl Store {
    /// Make `tiles/` ready for this build: `ne` masks of any other generation
    /// are deleted along with leftover temporaries, and every `osm` mask,
    /// whose generation mixes in a data version this daemon has yet to learn.
    /// Same-generation `ne` leftovers from a crashed daemon are harmless
    /// (identical bytes under the same name) and are rewritten when asked for.
    pub fn open(dir: &Path, generation: &str) -> io::Result<Self> {
        let root = dir.join("tiles");
        fs::create_dir_all(&root)?;
        let suffix = format!("-{generation}.png");
        let mut pending = vec![root];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(entry.path());
                } else if !entry.file_name().to_string_lossy().ends_with(&suffix)
                    || !entry.path().starts_with(dir.join("tiles/ne"))
                {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(Self {
            generation: generation.to_owned(),
            osm_generation: None,
            ready: HashMap::new(),
            order: VecDeque::new(),
        })
    }
    /// Name `osm` masks after this data version. A change forgets the `osm`
    /// tiles announced so far: their files stay valid under their old names,
    /// and the next request redraws from the new data under new ones.
    pub fn osm_version(&mut self, version: &str) {
        let generation = tag(&format!("{}\0{version}", self.generation));
        if self.osm_generation.as_deref() != Some(&generation) {
            self.osm_generation = Some(generation);
            self.ready.retain(|(set, _), _| *set != Set::Osm);
            self.order.retain(|(set, _)| *set != Set::Osm);
        }
    }
    /// The protocol path a tile is published under. An `osm` path needs the
    /// data version first (`osm_version`).
    pub fn path(&self, set: Set, key: TileKey) -> String {
        let generation = match set {
            Set::Ne => &self.generation,
            Set::Osm => self
                .osm_generation
                .as_deref()
                .expect("osm version is set before an osm tile is published"),
        };
        format!(
            "tiles/{}/{}/{}/{}-{generation}.png",
            set.name(),
            key.z,
            key.x,
            key.y
        )
    }
    pub fn ready(&self, set: Set, key: TileKey) -> Option<&Ready> {
        self.ready.get(&(set, key))
    }
    /// Record a published tile and return the paths (relative to the runtime
    /// directory) that fell off the end of the cap and must be deleted.
    pub fn announce(&mut self, set: Set, key: TileKey, ready: Ready) -> Vec<String> {
        if self.ready.insert((set, key), ready).is_none() {
            self.order.push_back((set, key));
        }
        let mut evicted = Vec::new();
        while self.order.len() > MAX_FILES {
            let oldest = self.order.pop_front().expect("non-empty");
            if let Some(ready) = self.ready.remove(&oldest) {
                evicted.push(ready.path);
            }
        }
        evicted
    }
}

/// Write PNG bytes under the runtime directory by temp-and-rename. The name
/// is deterministic for the tile and generation, and so are the bytes, so a
/// concurrent render of the same tile replaces the file with itself.
pub fn write(dir: &Path, path: &str, png: &[u8]) -> io::Result<()> {
    if !is_tile_path(path) {
        return Err(io::Error::other(format!(
            "Refusing to publish tile path {path:?}; see docs/protocol.md"
        )));
    }
    let target = dir.join(path);
    fs::create_dir_all(target.parent().expect("tile directory"))?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = dir.join(format!("{path}.{}.{unique}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(png)?;
    file.sync_all()?;
    fs::rename(temporary, target)
}

/// Shared by the tile tests here and in `osm.rs`.
#[cfg(test)]
pub mod review {
    pub const KTLX: (f64, f64) = (-97.27748, 35.33306);
    pub const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../review");

    /// A viewable composite for the review: the protocol's masks tinted over
    /// an opaque ground in the order the UI shader draws them (water,
    /// boundaries, minor roads, major roads), since the raw masks read as a
    /// red and green blur in a viewer.
    pub fn preview(pixels: &[u8]) -> Vec<u8> {
        let ground = [0x1b_u8, 0x1f, 0x27];
        let boundary = [0xc8_u8, 0xcc, 0xd4];
        let water = [0x6c_u8, 0xa6, 0xe0];
        let minor = [0x8a_u8, 0x8f, 0x99];
        let major = [0xe6_u8, 0xe9, 0xef];
        let mut out = Vec::with_capacity(pixels.len());
        for p in pixels.as_chunks::<4>().0 {
            let mut color = ground.map(f32::from);
            for (mask, tint) in [
                (p[1], water),
                (p[0], boundary),
                (p[2], minor),
                (255 - p[3], major),
            ] {
                let a = f32::from(mask) / 255.0;
                for (c, t) in color.iter_mut().zip(tint) {
                    *c = *c * (1.0 - a) + f32::from(t) * a;
                }
            }
            out.extend(color.map(|c| c.round() as u8));
            out.push(255);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::review::{DIR as REVIEW, KTLX, preview};
    use super::*;

    struct Channels {
        r: usize,
        g: usize,
        b: usize,
        a: usize,
        partial: usize,
    }
    fn decode(png_bytes: &[u8]) -> (png::Info<'static>, Vec<u8>) {
        let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        pixels.truncate(info.buffer_size());
        (reader.info().clone(), pixels)
    }
    fn count(pixels: &[u8]) -> Channels {
        let mut c = Channels {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
            partial: 0,
        };
        for p in pixels.as_chunks::<4>().0 {
            c.r += usize::from(p[0] != 0);
            c.g += usize::from(p[1] != 0);
            c.b += usize::from(p[2] != 0);
            c.a += usize::from(p[3] == 255);
            c.partial += usize::from((p[0] > 0 && p[0] < 255) || (p[1] > 0 && p[1] < 255));
        }
        c
    }

    #[test]
    fn embedded_geography_decodes() {
        let geography = Geography::embedded();
        let vertices = |scale: &Scale| -> usize {
            scale
                .layers
                .iter()
                .flat_map(|l| &l.polylines)
                .map(|p| p.points.len())
                .sum()
        };
        // DESIGN.md measured about 116 k vertices for the 1:50m world and
        // 324 k for the 1:10m envelope.
        assert!((90_000..200_000).contains(&vertices(&geography.sets[0])));
        assert!((250_000..450_000).contains(&vertices(&geography.sets[1])));
        for scale in &geography.sets {
            for polyline in scale.layers.iter().flat_map(|l| &l.polylines) {
                assert!(polyline.points.len() >= 2);
                for &(x, y) in &polyline.points {
                    assert!((-18_000_000..=18_000_000).contains(&x), "{x}");
                    assert!((-9_000_000..=9_000_000).contains(&y), "{y}");
                }
            }
        }
        // Every 1:10m vertex sits in the station envelope or next to one
        // that does (mirrors `engine/build.rs` `in_envelope`).
        fn in_envelope(lon: f64, lat: f64) -> bool {
            ((5.0..=75.0).contains(&lat) && (lon <= -20.0 || lon >= 120.0))
                || ((47.0..=56.0).contains(&lat) && (5.0..=16.0).contains(&lon))
        }
        for polyline in geography.sets[1].layers.iter().flat_map(|l| &l.polylines) {
            let inside: Vec<bool> = polyline
                .points
                .iter()
                .map(|&(x, y)| {
                    let (lon, lat) = (f64::from(x) * QUANTUM, f64::from(y) * QUANTUM);
                    in_envelope(lon, lat)
                })
                .collect();
            for i in 0..inside.len() {
                assert!(
                    inside[i]
                        || (i > 0 && inside[i - 1])
                        || inside.get(i + 1).copied().unwrap_or(false),
                    "{:?}",
                    polyline.points[i]
                );
            }
        }
        assert!(geography.places.len() > 7_000);
        assert!(
            geography
                .places
                .iter()
                .any(|p| p.name == "Oklahoma City" && p.class == "city" && p.rank == 3)
        );
    }

    #[test]
    fn place_search_ranks_word_starts_and_nearer_matches() {
        let oklahoma = search_places("oklahoma", Some((35.47, -97.52)), 8);
        assert_eq!(oklahoma[0].name, "Oklahoma City");
        assert!(
            oklahoma
                .iter()
                .all(|p| p.name.to_lowercase().contains("oklahoma"))
        );
        let norman = search_places("norman", None, 4);
        assert_eq!(norman[0].name, "Norman");
        assert!(search_places("   ", None, 8).is_empty());
        assert_eq!(search_places("city", Some((35.47, -97.52)), 3).len(), 3);
        let jacksonville = search_places("jacksonville", None, 8);
        let regions: Vec<&str> = jacksonville.iter().map(|p| p.region.as_str()).collect();
        assert!(
            regions.contains(&"Florida") && regions.contains(&"North Carolina"),
            "Jacksonville results name their states: {regions:?}"
        );
        let stokesdale = search_places("stokesdale", None, 4);
        assert_eq!(stokesdale[0].name, "Stokesdale");
        assert_eq!(stokesdale[0].region, "North Carolina");
        let hannover = search_places("hannover", Some((52.37, 9.73)), 4);
        assert_eq!(hannover[0].name, "Hannover");
        assert_eq!(hannover[0].region, "Lower Saxony");
        assert_eq!(hannover[0].country, "DE");
        assert!(gazetteer().len() > 15_000);
        assert!(gazetteer().len() > Geography::embedded().places.len());
    }

    #[test]
    fn tile_math() {
        assert_eq!(
            TileKey::containing(0, KTLX.0, KTLX.1),
            TileKey { z: 0, x: 0, y: 0 }
        );
        assert_eq!(
            TileKey::containing(5, KTLX.0, KTLX.1),
            TileKey { z: 5, x: 7, y: 12 }
        );
        // The unit square's corners and the Mercator latitude limit.
        assert_eq!(mercator(-180.0, MAX_LAT).0, 0.0);
        assert!(mercator(-180.0, MAX_LAT).1.abs() < 1e-9);
        assert!((mercator(180.0, -MAX_LAT).1 - 1.0).abs() < 1e-9);
        assert!((latitude(0.5)).abs() < 1e-9);
        assert!((latitude(mercator(0.0, 35.0).1) - 35.0).abs() < 1e-9);
        let request = Request {
            z: 3,
            x0: 1,
            y0: 2,
            x1: 3,
            y1: 3,
        }
        .validate()
        .unwrap();
        let order = request.centre_out();
        assert_eq!(order.len(), 6);
        assert_eq!(order[0], TileKey { z: 3, x: 2, y: 2 });
        assert!(order.iter().all(|k| request.contains(*k)));
        assert!(!request.contains(TileKey { z: 3, x: 0, y: 2 }));
        assert!(!request.contains(TileKey { z: 4, x: 2, y: 2 }));
        for bad in [
            Request { z: 23, ..request },
            Request {
                z: 3,
                x0: 1,
                y0: 2,
                x1: 8,
                y1: 3,
            },
            Request {
                z: 3,
                x0: 3,
                y0: 2,
                x1: 1,
                y1: 3,
            },
            Request {
                z: 5,
                x0: 0,
                y0: 0,
                x1: 8,
                y1: 7,
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert!(
            Request {
                z: 5,
                x0: 0,
                y0: 0,
                x1: 7,
                y1: 7,
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn renders_review_tiles() {
        let geography = Geography::embedded();
        fs::create_dir_all(REVIEW).unwrap();
        let world = TileKey { z: 1, x: 0, y: 0 };
        let ktlx5 = TileKey::containing(5, KTLX.0, KTLX.1);
        let ktlx8 = TileKey::containing(8, KTLX.0, KTLX.1);
        for (name, key, expect_boundaries, expect_coast) in [
            ("world-z1", world, true, true),
            ("ktlx-z5", ktlx5, true, true),
            ("ktlx-z8", ktlx8, false, false),
        ] {
            let started = std::time::Instant::now();
            let png_bytes = render(geography, key).unwrap();
            let elapsed = started.elapsed();
            assert_eq!(
                render(geography, key).unwrap(),
                png_bytes,
                "{name} is deterministic"
            );
            let (info, pixels) = decode(&png_bytes);
            assert_eq!((info.width, info.height), (SIZE, SIZE));
            assert_eq!(info.color_type, png::ColorType::Rgba);
            let c = count(&pixels);
            eprintln!(
                "{name} {key:?}: {elapsed:.1?}, {} bytes, R {} G {} partial {}, labels {}",
                png_bytes.len(),
                c.r,
                c.g,
                c.partial,
                labels(geography, key).len()
            );
            assert_eq!(c.b, 0, "{name}: B stays zero for ne");
            assert_eq!(
                c.a,
                (SIZE * SIZE) as usize,
                "{name}: A is 255 everywhere for ne (no major roads)"
            );
            if expect_boundaries {
                assert!(c.r > 0, "{name}: boundaries cross this tile");
            }
            if expect_coast {
                assert!(c.g > 0, "{name}: coast crosses this tile");
            }
            if expect_boundaries || expect_coast {
                assert!(c.partial > 0, "{name}: antialiased edges");
            }
            fs::write(format!("{REVIEW}/tile-{name}.png"), &png_bytes).unwrap();
            fs::write(
                format!("{REVIEW}/tile-{name}-preview.png"),
                crate::sweep::png(SIZE, SIZE, &preview(&pixels)).unwrap(),
            )
            .unwrap();
        }
        let ktlx_labels = labels(geography, ktlx5);
        assert!(ktlx_labels.iter().any(|l| l.name == "Oklahoma City"));
        for label in &ktlx_labels {
            assert!(Frame::new(ktlx5).contains_lon_lat(label.lon, label.lat));
        }
        assert!(labels(geography, world).is_empty() || labels(geography, world).len() < 200);
    }

    #[test]
    fn store_caps_and_names() {
        let mut store = Store {
            generation: "0123abcd".into(),
            osm_generation: None,
            ready: HashMap::new(),
            order: VecDeque::new(),
        };
        let key = TileKey { z: 5, x: 7, y: 12 };
        let path = store.path(Set::Ne, key);
        assert_eq!(path, "tiles/ne/5/7/12-0123abcd.png");
        assert!(is_tile_path(&path));
        let ready = Ready {
            path: path.clone(),
            labels: Vec::new(),
        };
        assert!(store.announce(Set::Ne, key, ready.clone()).is_empty());
        assert!(store.announce(Set::Ne, key, ready.clone()).is_empty());
        assert_eq!(store.ready(Set::Ne, key), Some(&ready));
        // The osm generation mixes in the data version and is its own name space.
        assert_eq!(store.ready(Set::Osm, key), None);
        store.osm_version("20260830_080001_pt");
        let osm = store.path(Set::Osm, key);
        assert!(
            osm.starts_with("tiles/osm/5/7/12-") && osm.ends_with(".png"),
            "{osm}"
        );
        assert_ne!(osm, path.replace("/ne/", "/osm/"));
        assert!(is_tile_path(&osm));
        let osm_ready = Ready {
            path: osm.clone(),
            labels: Vec::new(),
        };
        assert!(store.announce(Set::Osm, key, osm_ready.clone()).is_empty());
        assert_eq!(store.ready(Set::Osm, key), Some(&osm_ready));
        store.osm_version("20260830_080001_pt");
        assert_eq!(store.ready(Set::Osm, key), Some(&osm_ready), "same version");
        store.osm_version("20260913_080001_pt");
        assert_eq!(
            store.ready(Set::Osm, key),
            None,
            "a new version starts over"
        );
        assert_ne!(store.path(Set::Osm, key), osm);
        assert_eq!(store.ready(Set::Ne, key), Some(&ready), "ne is untouched");
        for i in 0..MAX_FILES as u32 {
            let k = TileKey { z: 12, x: i, y: 0 };
            let evicted = store.announce(
                Set::Ne,
                k,
                Ready {
                    path: store.path(Set::Ne, k),
                    labels: Vec::new(),
                },
            );
            if i + 1 < MAX_FILES as u32 {
                assert!(evicted.is_empty(), "{i}");
            } else {
                assert_eq!(
                    evicted,
                    vec![path.clone()],
                    "the oldest announced goes first"
                );
            }
        }
        assert_eq!(store.ready(Set::Ne, key), None);
        assert_eq!(store.order.len(), MAX_FILES);
        assert_eq!(tag("a"), tag("a"));
        assert_ne!(tag("a"), tag("b"));
        assert_eq!(tag("").len(), 8);
    }
}
