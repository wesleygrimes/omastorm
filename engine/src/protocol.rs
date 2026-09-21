//! Wire types for `docs/protocol.md`, version 2. Objects serialize in
//! declaration order; clients read keys by name, so order is not significant.
#![allow(
    dead_code,
    reason = "wire fields are written by Serialize for the UI and never read here"
)]

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 2;

/// Engine to client. Borrows so a snapshot never clones the state tree.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message<'a> {
    Hello(&'a Hello),
    State(&'a State),
    Error(&'a Rejection<'a>),
    TileReady(&'a TileReady<'a>),
    Places(&'a Places<'a>),
}

/// One tile answering a client's `tiles_needed`, sent to that client alone
/// (docs/protocol.md, basemap tiles). Like `Error` it is a reply, not shared
/// state: a tile already rendered this session is answered at once.
#[derive(Serialize)]
pub struct TileReady<'a> {
    pub v: u32,
    /// `ne` (Natural Earth, embedded) or `osm` (fetched vector tile).
    pub set: &'a str,
    pub z: u32,
    pub x: u32,
    pub y: u32,
    /// `tiles/<set>/<z>/<x>/<file>`, relative to the runtime directory.
    pub path: &'a str,
    /// The tile's places for the overlay.
    pub labels: Vec<Label>,
}

/// A place label carried by `tile_ready`; `class` and `rank` follow the
/// OpenMapTiles `place` vocabulary (lower rank is more important).
/// `region` and `country` come from Natural Earth (`adm1name`, `iso_a2`)
/// so the location picker can tell two Jacksonvilles apart; empty on OSM
/// labels and omitted on the wire when empty.
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug, Default)]
pub struct Label {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub class: String,
    pub rank: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub region: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub country: String,
}

/// Places answering one client's `search_places`. A reply, not shared state:
/// only the sender hears it, and `state` does not change.
#[derive(Serialize)]
pub struct Places<'a> {
    pub v: u32,
    pub query: &'a str,
    pub results: &'a [Label],
}

/// The engine's answer to one client's command it could not carry out. Sent
/// only to that client; `state` is untouched and not broadcast, so nothing a
/// client sends can erase a condition another client is showing.
#[derive(Serialize)]
pub struct Rejection<'a> {
    pub v: u32,
    /// The command's `type`, as the client wrote it.
    pub command: &'a str,
    pub message: &'a str,
}

#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub v: u32,
    pub engine: &'static str,
    pub pid: u32,
    pub build: String,
    pub sites: Vec<Station>,
    pub sources: Vec<SourceInfo>,
    pub sites_source: String,
    pub sites_retrieved: String,
    pub sites_notes: String,
}

/// One compiled adapter listed in `hello.sources` (`docs/grid-adapters.md`).
#[derive(Serialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub id: String,
    pub family: Family,
    pub kind: Kind,
    pub default_product_class: ProductClass,
    pub name: String,
    pub attribution: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Coverage>,
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Polar,
    Grid,
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Site,
    Mosaic,
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Debug, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum ProductClass {
    Other,
    PrecipitationRate,
    Reflectivity,
}

/// A lat/lon vertex on the wire (`docs/grid-adapters.md`).
#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Debug)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

/// Adapter-declared coverage. Site circles omit lat/lon on the wire (those
/// live on `hello.sites`); mosaic boxes and polygons carry their own.
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Coverage {
    Circle {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lat: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lon: Option<f64>,
        radius_km: f64,
    },
    Box {
        north: f64,
        south: f64,
        east: f64,
        west: f64,
    },
    Polygon {
        vertices: Vec<GeoPoint>,
    },
}

/// The launcher's view of a running daemon's hello. Only the fields needed to
/// recognize a matching build, so a daemon of another build still names its PID.
#[derive(Deserialize)]
pub struct Handshake {
    #[serde(rename = "type")]
    pub kind: String,
    pub v: u32,
    pub pid: u32,
    pub build: String,
}

/// `engine/data/sites.json`.
#[derive(Deserialize)]
pub struct SiteTable {
    pub source: String,
    pub retrieved: String,
    pub notes: String,
    pub sites: Vec<Station>,
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Station {
    pub id: String,
    #[serde(default)]
    pub source_id: String,
    pub name: String,
    pub state: String,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
    #[serde(default = "nexrad_site_coverage")]
    pub coverage: Coverage,
}

fn nexrad_site_coverage() -> Coverage {
    Coverage::Circle {
        lat: None,
        lon: None,
        radius_km: 460.0,
    }
}

#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub v: u32,
    pub mode: Mode,
    pub navigation: Navigation,
    pub selection: Option<Selection>,
    pub connection: Connection,
    pub timeline: Vec<TimelineEntry>,
    pub frame: Option<FrameWire>,
    /// The tile sources (`docs/protocol.md`, `tile_ready`).
    pub basemap: Basemap,
    pub playing: bool,
}

/// Follow / lock flags; valid even with no selection (`docs/grid-adapters.md`).
#[derive(Serialize, PartialEq, Clone, Debug)]
pub struct Navigation {
    pub follow: bool,
    pub locked: bool,
}

/// The exact thing being viewed: a compiled source plus its polar site or mosaic.
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub source_id: String,
    pub target: AdapterTarget,
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AdapterTarget {
    Site { site_id: String },
    Mosaic,
}

/// `state.basemap`: what draws the tiles and, for `osm`, whether it can.
#[derive(Serialize, PartialEq, Debug)]
pub struct Basemap {
    pub ne: NaturalEarth,
    pub osm: Osm,
}

#[derive(Serialize, PartialEq, Debug)]
pub struct NaturalEarth {
    pub version: &'static str,
}

/// The `osm` tile source. `status` is the lasting condition of the fetch path,
/// which no client command can clear; `attribution` is shown verbatim by the
/// UI whenever an `osm` tile is on screen.
#[derive(Serialize, PartialEq, Clone, Debug)]
pub struct Osm {
    pub status: OsmStatus,
    pub source: String,
    /// The data version, empty until one is known.
    pub version: String,
    pub attribution: String,
}

#[derive(Serialize, PartialEq, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum OsmStatus {
    /// Fetching works, or has not been tried since a version was learnt.
    Ok,
    /// Fetching fails; cached tiles still serve.
    Offline,
    /// No data version is known and nothing is cached.
    Unavailable,
}

/// Whether `path` names a texture file the protocol allows: the literal `tex/`
/// prefix and exactly one further segment that is not empty, `.`, or `..` and
/// holds no `/`, backslash, or NUL (`docs/protocol.md`). `ui/Engine.qml`
/// applies the same rule, so a path that passes here is one the UI will load.
pub fn is_texture_path(path: &str) -> bool {
    match path.strip_prefix("tex/") {
        Some(name) => {
            !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
        }
        None => false,
    }
}

/// Whether `path` names a tile file the protocol allows: the literal `tiles/`
/// prefix, a set (`ne` or `osm`), a zoom and a column as decimal integers,
/// and one further segment under the texture rule (`docs/protocol.md`).
pub fn is_tile_path(path: &str) -> bool {
    let mut parts = path.split('/');
    let decimal = |part: Option<&str>| {
        part.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    parts.next() == Some("tiles")
        && matches!(parts.next(), Some("ne" | "osm"))
        && decimal(parts.next())
        && decimal(parts.next())
        && parts.next().is_some_and(|name| {
            !name.is_empty() && name != "." && name != ".." && !name.contains(['\\', '\0'])
        })
        && parts.next().is_none()
}

impl State {
    /// Every file under `$XDG_RUNTIME_DIR/omastorm/` a client may currently be
    /// reading. Texture cleanup retires a file 30 s after it leaves this set,
    /// so any new path field (azimuth tables, timeline frames) is added here.
    pub fn referenced_files(&self) -> impl Iterator<Item = &str> {
        match &self.frame {
            Some(FrameWire::Polar(frame)) => {
                [frame.texture.as_str(), frame.azimuth_lut.as_str()].into_iter()
            }
            Some(FrameWire::Mosaic(frame)) => [frame.texture.as_str(), ""].into_iter(),
            None => ["", ""].into_iter(),
        }
        .filter(|path| !path.is_empty())
    }
}

#[derive(Serialize, PartialEq, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Archived,
    Live,
}

#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub status: ConnectionStatus,
    /// Age of the newest complete frame; 0 while there is none.
    pub age_seconds: u64,
}

/// The lasting condition of the engine's data path (`docs/protocol.md`). In
/// live mode `Ok`, `Stale`, and `Unavailable` follow the age of the newest
/// radial received at each snapshot, so they only ever say how quiet a
/// reachable feed has been; `Loading` is set by a site switch and `Offline`
/// by the poller when the bucket cannot be reached, and the next live sweep
/// clears both. `Idle` is no active selection (`docs/grid-adapters.md`).
#[derive(Serialize, PartialEq, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionStatus {
    Ok,
    Stale,
    Unavailable,
    Offline,
    Loading,
    Idle,
}

/// One frame a client can `seek` to (`docs/protocol.md`, `state.timeline`):
/// the station's complete frames oldest first, then the sweep in progress
/// as a `partial` entry while one is painting.
#[derive(Serialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
    pub id: String,
    pub scan_time: String,
    pub status: FrameStatus,
}

/// Polar sweep or georeferenced mosaic (`docs/grid-adapters.md`). Internally
/// tagged with `kind`. Polar is today's sweep frame; mosaic omits polar-only
/// keys. The fixture file deserializes as [`Frame`], not this enum.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum FrameWire {
    Polar(Frame),
    Mosaic(MosaicFrame),
}

impl FrameWire {
    pub fn id(&self) -> &str {
        match self {
            Self::Polar(frame) => &frame.id,
            Self::Mosaic(frame) => &frame.id,
        }
    }
    pub fn product(&self) -> &str {
        match self {
            Self::Polar(frame) => &frame.product,
            Self::Mosaic(frame) => &frame.product,
        }
    }
    pub fn scan_time(&self) -> &str {
        match self {
            Self::Polar(frame) => &frame.scan_time,
            Self::Mosaic(frame) => &frame.scan_time,
        }
    }
    pub fn as_polar(&self) -> Option<&Frame> {
        match self {
            Self::Polar(frame) => Some(frame),
            Self::Mosaic(_) => None,
        }
    }
    pub fn as_polar_mut(&mut self) -> Option<&mut Frame> {
        match self {
            Self::Polar(frame) => Some(frame),
            Self::Mosaic(_) => None,
        }
    }
}

/// GridFamily frame (`docs/grid-adapters.md`). Bounds are JSON numbers and
/// may be fractional; polar frames keep integer bounds.
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MosaicFrame {
    pub id: String,
    pub product: String,
    pub product_name: String,
    pub units: String,
    pub scan_time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep_end: Option<String>,
    pub status: FrameStatus,
    #[serde(default)]
    pub texture: String,
    pub width: u32,
    pub height: u32,
    pub crs: Crs,
    pub geotransform: [f64; 6],
    pub palette: Vec<String>,
    pub bounds: Vec<f64>,
}

/// Tagged CRS: WGS84 lon/lat in, east-then-north x/y out (`docs/grid-adapters.md`).
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Crs {
    Geographic {
        ellipsoid: Ellipsoid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        datum_transform: Option<DatumTransform>,
    },
    Mercator {
        ellipsoid: Ellipsoid,
        lon0_deg: f64,
        scale: f64,
        false_easting_m: f64,
        false_northing_m: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        datum_transform: Option<DatumTransform>,
    },
    TransverseMercator {
        ellipsoid: Ellipsoid,
        lat0_deg: f64,
        lon0_deg: f64,
        scale: f64,
        false_easting_m: f64,
        false_northing_m: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        datum_transform: Option<DatumTransform>,
    },
    PolarStereographic {
        ellipsoid: Ellipsoid,
        lat0_deg: f64,
        lon0_deg: f64,
        scale: f64,
        false_easting_m: f64,
        false_northing_m: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        datum_transform: Option<DatumTransform>,
    },
    LambertConformalConic {
        ellipsoid: Ellipsoid,
        lat0_deg: f64,
        lon0_deg: f64,
        standard_parallel1_deg: f64,
        standard_parallel2_deg: f64,
        false_easting_m: f64,
        false_northing_m: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        datum_transform: Option<DatumTransform>,
    },
    LambertAzimuthalEqualArea {
        ellipsoid: Ellipsoid,
        lat0_deg: f64,
        lon0_deg: f64,
        false_easting_m: f64,
        false_northing_m: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        datum_transform: Option<DatumTransform>,
    },
}

impl Crs {
    pub fn ellipsoid(&self) -> &Ellipsoid {
        match self {
            Self::Geographic { ellipsoid, .. }
            | Self::Mercator { ellipsoid, .. }
            | Self::TransverseMercator { ellipsoid, .. }
            | Self::PolarStereographic { ellipsoid, .. }
            | Self::LambertConformalConic { ellipsoid, .. }
            | Self::LambertAzimuthalEqualArea { ellipsoid, .. } => ellipsoid,
        }
    }
    pub fn datum_transform(&self) -> Option<&DatumTransform> {
        match self {
            Self::Geographic {
                datum_transform, ..
            }
            | Self::Mercator {
                datum_transform, ..
            }
            | Self::TransverseMercator {
                datum_transform, ..
            }
            | Self::PolarStereographic {
                datum_transform, ..
            }
            | Self::LambertConformalConic {
                datum_transform, ..
            }
            | Self::LambertAzimuthalEqualArea {
                datum_transform, ..
            } => datum_transform.as_ref(),
        }
    }
    /// WGS84 geographic, as the fixture mosaic publishes.
    pub fn wgs84_geographic() -> Self {
        Self::Geographic {
            ellipsoid: Ellipsoid::WGS84,
            datum_transform: None,
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Ellipsoid {
    pub semi_major_m: f64,
    pub inverse_flattening: f64,
}

impl Ellipsoid {
    pub const WGS84: Self = Self {
        semi_major_m: 6_378_137.0,
        inverse_flattening: 298.257_223_563,
    };
}

/// EPSG position-vector seven-parameter Helmert transform, grid datum → WGS84.
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DatumTransform {
    Helmert7 {
        translation_m: [f64; 3],
        rotation_arc_seconds: [f64; 3],
        scale_ppm: f64,
    },
}
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Frame {
    pub id: String,
    pub product: String,
    /// Display name for `product`; the engine owns product vocabulary.
    pub product_name: String,
    pub units: String,
    pub elevation_deg: f64,
    pub scan_time: String,
    pub sweep_end: String,
    pub status: FrameStatus,
    /// Paths relative to `$XDG_RUNTIME_DIR/omastorm/` and the sweep geometry
    /// are decoded at startup, so the fixture file carries none of them.
    #[serde(default)]
    pub texture: String,
    #[serde(default)]
    pub azimuth_lut: String,
    #[serde(default)]
    pub rays: u32,
    #[serde(default)]
    pub gates: u32,
    #[serde(default)]
    pub first_gate_m: u32,
    #[serde(default)]
    pub gate_spacing_m: u32,
    /// Measured value = (code - offset) / scale, the moment's own encoding;
    /// the UI turns a dBZ floor into a code threshold with them. Zero while
    /// nothing is decoded (the loading placeholder), which disables the floor.
    #[serde(default)]
    pub scale: f32,
    #[serde(default)]
    pub offset: f32,
    pub site: Geometry,
    pub palette: Vec<String>,
    pub bounds: Vec<i32>,
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum FrameStatus {
    Complete,
    Partial,
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Geometry {
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
}

/// Client to engine. A known type with missing or mistyped fields fails to
/// deserialize; the engine answers the sender with a `Rejection`.
#[derive(Deserialize, PartialEq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Go live on a station from the `hello` table: the newest
    /// cached frame or an empty one shows at once, and the poller follows.
    SelectSite {
        id: String,
    },
    /// Restore a mosaic by its `hello.sources` id. Polar ids error; unknown
    /// ids error against the source table (`docs/grid-adapters.md`).
    SelectSource {
        id: String,
    },
    Follow {
        enabled: bool,
    },
    Lock {
        enabled: bool,
    },
    Play,
    Pause,
    Step {
        delta: i64,
    },
    Seek {
        id: String,
    },
    #[serde(rename_all = "camelCase")]
    SetProduct {
        product: String,
        elevation_index: u32,
    },
    /// The visible inclusive tile rectangle at one zoom, at most 64 tiles
    /// Answered tile by tile with `tile_ready` to the sender.
    TilesNeeded {
        z: u32,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
    },
    /// The map centre when a pan settles: with `follow` on and
    /// `lock` off the engine hands off to the nearest station.
    ViewCenter {
        lat: f64,
        lon: f64,
    },
    /// Rank gazetteer places for the location picker. Answered with
    /// `places` to the sender; optional `lat`/`lon` bias nearer matches.
    /// `name, where` filters by region or country.
    SearchPlaces {
        query: String,
        #[serde(default)]
        lat: Option<f64>,
        #[serde(default)]
        lon: Option<f64>,
    },
    /// Anything newer than this build.
    #[serde(other)]
    Unsupported,
}

#[cfg(test)]
mod tests {
    use super::{is_texture_path, is_tile_path};

    #[test]
    fn tile_paths_are_set_zoom_column_and_one_file() {
        for ok in [
            "tiles/ne/5/7/12-3f9a1c2e.png",
            "tiles/osm/11/470/808-3f9a1c2e.png",
            "tiles/ne/0/0/0-00000000.png",
            "tiles/ne/1/0/a",
        ] {
            assert!(is_tile_path(ok), "{ok} should be accepted");
        }
        for bad in [
            "",
            "tiles",
            "tiles/ne/5/7/",
            "tiles/ne/5/7",
            "tiles/ne/5/7/12-a.png/x",
            "tiles/ne/5/7/.",
            "tiles/ne/5/7/..",
            "tiles/ne/5/../12-a.png",
            "tiles/ne/-1/7/12-a.png",
            "tiles/ne/5/7a/12-a.png",
            "tiles/foo/5/7/12-a.png",
            "tiles/ne/5/7/12\\a.png",
            "tiles/ne/5/7/12\0.png",
            "tex/ne/5/7/12-a.png",
            "/tiles/ne/5/7/12-a.png",
        ] {
            assert!(!is_tile_path(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn texture_paths_are_one_segment_under_tex() {
        for ok in [
            "tex/sweep-KTLX-20130520T201643Z-e0-1-r2.png",
            "tex/sweep-TEST-r1.png",
            "tex/azlut-KTLX-20130520T201643Z-e0-r3.png",
            "tex/...",
            "tex/a",
        ] {
            assert!(is_texture_path(ok), "{ok} should be accepted");
        }
        for bad in [
            "",
            "tex",
            "tex/",
            "tex/.",
            "tex/..",
            "tex/../x.png",
            "tex/a/b.png",
            "tex//a.png",
            "tex/a\\b.png",
            "tex/a\0.png",
            "/tex/a.png",
            "text/a.png",
            "TEX/a.png",
            "../tex/a.png",
            "a.png",
        ] {
            assert!(!is_texture_path(bad), "{bad:?} should be rejected");
        }
    }
}
