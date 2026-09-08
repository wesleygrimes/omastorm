//! The `osm` tile set (DESIGN.md, basemap tiles): OpenMapTiles-schema vector
//! tiles, one HTTPS GET per tile from OpenFreeMap by default, cached as
//! fetched under `$XDG_CACHE_HOME/omastorm/vt/`, decoded with `mvt-reader`,
//! and stroked into the protocol's four masks by the rasterizer `ne` tiles
//! use, with roads in B (minor) and A (255 minus major). Launch fetches
//! nothing: the first network call is the first `tiles_needed` at z7 or
//! deeper, which reads TileJSON once in a daemon's life for the data version
//! and the URL template, then fetches tiles. Fetching that fails backs off
//! for 30 s, during which tiles are answered from the cache or as `ne`.

use crate::protocol::{Label, Osm as Info, OsmStatus};
use crate::tiles::{self, BOUNDARY_WIDTH, COAST_WIDTH, SIZE, Strokes, TileKey};
use geo_types::{Geometry, LineString};
use mvt_reader::{
    Reader,
    feature::{Feature, Value},
};
use serde::Deserialize;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::Semaphore, task::spawn_blocking};

/// `osm` starts here: about 500 m/px at 35° N, where Natural Earth's
/// kilometre vertex spacing begins to show (DESIGN.md).
pub const FROM_ZOOM: u32 = 7;
/// OpenFreeMap serves z0–14; deeper tiles fall back to `ne`.
pub const MAX_ZOOM: u32 = 14;
/// The TileJSON document naming the data version and the URL template.
/// `OMASTORM_TILES_URL` overrides it for development and tests.
const DEFAULT_URL: &str = "https://tiles.openfreemap.org/planet";
const USER_AGENT: &str = concat!(
    "omastorm/",
    env!("CARGO_PKG_VERSION"),
    " (https://omastorm.com)"
);
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const IN_FLIGHT: usize = 4;
/// After a 429, a 5xx, or a transport failure.
const BACK_OFF: Duration = Duration::from_secs(30);
/// Cache ceiling and the size eviction trims to (DESIGN.md, basemap tiles).
const CEILING: u64 = 512 << 20;
const TARGET: u64 = 448 << 20;
/// Data versions kept: the current one and the newest other.
const KEEP_VERSIONS: usize = 2;
/// The biggest body accepted; the largest tile measured near KTLX is 132 kB.
const MAX_BODY: usize = 4 << 20;
/// Road stroke widths in tile pixels: a proposal for the capture review.
const MAJOR_WIDTH: f32 = 1.5;
const MINOR_WIDTH: f32 = 1.0;
/// OpenMapTiles `transportation` classes drawn as major and minor roads.
const MAJOR: [&str; 3] = ["motorway", "trunk", "primary"];
const MINOR: [&str; 2] = ["secondary", "tertiary"];
/// Places carried as labels, in the protocol's vocabulary.
const PLACES: [&str; 3] = ["city", "town", "village"];
/// A `place` without an OpenMapTiles `rank`: below every ranked one.
const UNRANKED: u32 = 20;
/// Attribution before TileJSON supplies its own (openfreemap.org, terms).
const OPENFREEMAP_ATTRIBUTION: &str = "OpenFreeMap © OpenMapTiles Data from OpenStreetMap";
const GENERIC_ATTRIBUTION: &str = "© OpenMapTiles © OpenStreetMap contributors";

/// The fetch path shared by every client's tile task.
pub struct Osm {
    url: String,
    /// `vt/<source>/`, the source a tag of the configured URL.
    dir: PathBuf,
    client: reqwest::Client,
    permits: Semaphore,
    /// Serializes the TileJSON read so concurrent first requests fetch it once.
    tilejson: tokio::sync::Mutex<()>,
    back_off: Duration,
    inner: Mutex<Inner>,
}
struct Inner {
    info: Info,
    /// The URL template once TileJSON has been read this daemon's life.
    template: Option<String>,
    back_off_until: Option<Instant>,
    /// Bytes under `dir`: the startup scan plus what was written since.
    cache_bytes: u64,
    /// Written to since the last trim.
    grown: bool,
}

/// `TileJSON 3.0`, the fields used.
#[derive(Deserialize)]
struct TileJson {
    tiles: Vec<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    attribution: String,
}
/// What TileJSON told us.
/// `$XDG_CACHE_HOME/omastorm` (default `~/.cache/omastorm`), the one place
/// the engine persists anything: vector tiles here, the frame catalog in
/// `catalog.rs`. Created if missing.
pub fn cache_root() -> io::Result<PathBuf> {
    let cache = match env::var_os("XDG_CACHE_HOME") {
        Some(base) if Path::new(&base).is_absolute() => PathBuf::from(base),
        _ => {
            let home = env::var_os("HOME")
                .ok_or_else(|| io::Error::other("HOME or XDG_CACHE_HOME is required"))?;
            PathBuf::from(home).join(".cache")
        }
    };
    let root = cache.join("omastorm");
    fs::create_dir_all(&root)?;
    Ok(root)
}

#[derive(PartialEq, Debug)]
struct Metadata {
    version: String,
    template: String,
    name: String,
    attribution: String,
}

impl Osm {
    /// Open the cache and build the client. Nothing is fetched.
    pub fn open() -> io::Result<Osm> {
        let url = env::var("OMASTORM_TILES_URL").unwrap_or_else(|_| DEFAULT_URL.into());
        let dir = cache_root()?.join("vt").join(tiles::tag(&url));
        fs::create_dir_all(&dir)?;
        let (versions, cache_bytes) = scan(&dir)?;
        let host = reqwest::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        let openfreemap = host == "openfreemap.org" || host.ends_with(".openfreemap.org");
        // Offline, the newest cached version serves; until a version is known
        // there is nothing to serve.
        let version = versions.last().cloned().unwrap_or_default();
        let info = Info {
            status: if version.is_empty() {
                OsmStatus::Unavailable
            } else {
                OsmStatus::Ok
            },
            source: if openfreemap {
                "OpenFreeMap".into()
            } else {
                host
            },
            version,
            attribution: if openfreemap {
                OPENFREEMAP_ATTRIBUTION.into()
            } else {
                GENERIC_ATTRIBUTION.into()
            },
        };
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(io::Error::other)?;
        Ok(Osm {
            url,
            dir,
            client,
            permits: Semaphore::new(IN_FLIGHT),
            tilejson: tokio::sync::Mutex::new(()),
            back_off: BACK_OFF,
            inner: Mutex::new(Inner {
                info,
                template: None,
                back_off_until: None,
                cache_bytes,
                grown: false,
            }),
        })
    }
    /// The `state.basemap.osm` this source reports right now.
    pub fn info(&self) -> Info {
        self.inner.lock().unwrap().info.clone()
    }
    /// When a tile answered as `ne` for want of network is worth asking for
    /// again: the end of the back-off, or one back-off from now.
    pub fn retry_at(&self) -> Instant {
        self.inner
            .lock()
            .unwrap()
            .back_off_until
            .unwrap_or_else(|| Instant::now() + self.back_off)
    }
    fn backing_off(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .back_off_until
            .is_some_and(|until| Instant::now() < until)
    }
    fn fail(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.back_off_until = Some(Instant::now() + self.back_off);
        inner.info.status = if inner.info.version.is_empty() {
            OsmStatus::Unavailable
        } else {
            OsmStatus::Offline
        };
    }
    fn succeed(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.back_off_until = None;
        inner.info.status = OsmStatus::Ok;
    }
    fn known(&self) -> Option<(String, String)> {
        let inner = self.inner.lock().unwrap();
        inner
            .template
            .clone()
            .map(|template| (inner.info.version.clone(), template))
    }
    /// One GET under the in-flight limit: the body on 200, an empty body on
    /// 204 or 404 (an empty tile), otherwise the failure in words.
    async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let _permit = self.permits.acquire().await.map_err(|e| e.to_string())?;
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            return Err("body over the size limit".into());
        }
        // Content-Length may be absent (chunked or close-delimited bodies).
        // Enforce the cap while receiving, before retaining each chunk, rather
        // than buffering the entire response and only checking after EOF.
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            if chunk.len() > MAX_BODY - bytes.len() {
                return Err("body over the size limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    /// The data version and URL template, reading TileJSON on the first call
    /// of a daemon's life. `None` while offline.
    async fn template(&self) -> Option<(String, String)> {
        if let Some(known) = self.known() {
            return Some(known);
        }
        let _serial = self.tilejson.lock().await;
        if let Some(known) = self.known() {
            return Some(known);
        }
        if self.backing_off() {
            return None;
        }
        let metadata = match self.get(&self.url).await.and_then(|bytes| tilejson(&bytes)) {
            Ok(metadata) => metadata,
            Err(e) => {
                eprintln!("TileJSON {}: {e}", self.url);
                self.fail();
                return None;
            }
        };
        self.succeed();
        {
            let mut inner = self.inner.lock().unwrap();
            inner.info.version = metadata.version.clone();
            inner.template = Some(metadata.template.clone());
            if !metadata.name.is_empty() {
                inner.info.source = metadata.name;
            }
            if !metadata.attribution.is_empty() {
                inner.info.attribution = metadata.attribution;
            }
        }
        let (dir, keep) = (self.dir.clone(), metadata.version.clone());
        if let Err(e) = spawn_blocking(move || prune_versions(&dir, &keep))
            .await
            .map_err(io::Error::other)
            .and_then(|r| r)
        {
            eprintln!("Tile cache versions: {e}");
        }
        Some((metadata.version, metadata.template))
    }
    /// The vector tile for `key`, from the cache or fetched into it; `None`
    /// when neither is possible right now (no version known, or backing off).
    /// The bytes are the tile as served, empty for an empty tile.
    pub async fn tile(&self, key: TileKey) -> Option<Vec<u8>> {
        let (version, template) = self.template().await?;
        let path = self
            .dir
            .join(&version)
            .join(key.z.to_string())
            .join(key.x.to_string())
            .join(format!("{}.pbf", key.y));
        // A cached tile is tens of kilobytes from the page cache: read inline.
        if let Ok(bytes) = fs::read(&path) {
            touch(&path);
            return Some(bytes);
        }
        if self.backing_off() {
            return None;
        }
        let url = template
            .replace("{z}", &key.z.to_string())
            .replace("{x}", &key.x.to_string())
            .replace("{y}", &key.y.to_string());
        match self.get(&url).await {
            Ok(bytes) => {
                self.succeed();
                match write(&path, &bytes) {
                    Ok(()) => {
                        let mut inner = self.inner.lock().unwrap();
                        inner.cache_bytes += bytes.len() as u64;
                        inner.grown = true;
                    }
                    Err(e) => eprintln!("Tile cache {}: {e}", path.display()),
                }
                Some(bytes)
            }
            Err(e) => {
                eprintln!("Tile {url}: {e}");
                self.fail();
                None
            }
        }
    }
    /// After a fetch batch: when the cache has grown past its ceiling, drop
    /// the least recently read tiles until it is under the target.
    pub async fn trim(&self) {
        let due = {
            let mut inner = self.inner.lock().unwrap();
            let due = inner.grown && inner.cache_bytes > CEILING;
            inner.grown = false;
            due
        };
        if !due {
            return;
        }
        let dir = self.dir.clone();
        match spawn_blocking(move || evict(&dir, TARGET))
            .await
            .map_err(io::Error::other)
            .and_then(|r| r)
        {
            Ok(total) => self.inner.lock().unwrap().cache_bytes = total,
            Err(e) => eprintln!("Tile cache eviction: {e}"),
        }
    }
}

/// Parse TileJSON: the first template, the data version it names (the path
/// segment before `{z}`), the source's name, and its attribution as text.
fn tilejson(bytes: &[u8]) -> Result<Metadata, String> {
    let document: TileJson = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let template = document
        .tiles
        .into_iter()
        .find(|t| t.contains("{z}") && t.contains("{x}") && t.contains("{y}"))
        .ok_or("no {z}/{x}/{y} template in tiles")?;
    let head = template.split("{z}").next().unwrap_or_default();
    let segment = head
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let clean = !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && segment != "."
        && segment != ".."
        && !segment.contains(':');
    // A template whose version is not a plain directory name is versioned by
    // its own text, so a changed template still starts a new cache.
    let version = if clean {
        segment.to_owned()
    } else {
        tiles::tag(&template)
    };
    Ok(Metadata {
        version,
        template,
        name: document.name.trim().to_owned(),
        attribution: text(&document.attribution),
    })
}
/// HTML attribution to the words: tags removed, the common entities decoded,
/// whitespace collapsed.
fn text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&copy;", "©")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every file under `dir`: path, size, and last touch.
fn walk(dir: &Path) -> io::Result<Vec<(PathBuf, u64, SystemTime)>> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                files.push((
                    entry.path(),
                    metadata.len(),
                    metadata.modified().unwrap_or(UNIX_EPOCH),
                ));
            }
        }
    }
    Ok(files)
}
/// The version directories under `dir`, oldest name first, and the bytes held.
fn scan(dir: &Path) -> io::Result<(Vec<String>, u64)> {
    let mut versions = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            versions.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    versions.sort();
    let total = walk(dir)?.iter().map(|(_, len, _)| len).sum();
    Ok((versions, total))
}
/// Keep `keep` and the newest other version directory; delete the rest.
fn prune_versions(dir: &Path, keep: &str) -> io::Result<()> {
    let (mut versions, _) = scan(dir)?;
    versions.retain(|v| v != keep);
    // Newest first by name; OpenFreeMap versions are dated.
    versions.reverse();
    for old in versions.iter().skip(KEEP_VERSIONS - 1) {
        fs::remove_dir_all(dir.join(old))?;
    }
    Ok(())
}
/// Delete the least recently touched files until `dir` holds at most
/// `target` bytes; returns what it holds afterwards.
fn evict(dir: &Path, target: u64) -> io::Result<u64> {
    let mut files = walk(dir)?;
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    files.sort_by_key(|(_, _, touched)| *touched);
    for (path, len, _) in files {
        if total <= target {
            break;
        }
        match fs::remove_file(&path) {
            Ok(()) => total -= len,
            Err(e) if e.kind() == io::ErrorKind::NotFound => total -= len,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}
/// Record a read with a clock we control, so `relatime` cannot mislead eviction.
fn touch(path: &Path) {
    if let Ok(file) = fs::File::open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}
/// Write a fetched tile by temp-and-rename; a concurrent fetch of the same
/// tile replaces the file with the same bytes.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(path.parent().expect("tile directory"))?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("{}.{unique}.tmp", std::process::id()));
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

/// Render a vector tile (its bytes as served; empty for an empty tile) to
/// the protocol's PNG and its place labels. `boundary` (admin levels 2 and 4
/// on land) strokes R, `water` shorelines and `waterway` lines G,
/// `transportation` secondary and tertiary roads B, motorway, trunk, and
/// primary roads A as 255 minus coverage; `place` cities, towns, and
/// villages become labels.
pub fn render(bytes: &[u8], key: TileKey) -> io::Result<(Vec<u8>, Vec<Label>)> {
    let mut boundaries = Strokes::new();
    let mut water = Strokes::new();
    let mut minor = Strokes::new();
    let mut major = Strokes::new();
    let mut labels = Vec::new();
    if !bytes.is_empty() {
        let reader = Reader::new(bytes.to_vec()).map_err(io::Error::other)?;
        for layer in reader.get_layer_metadata().map_err(io::Error::other)? {
            let wanted = matches!(
                layer.name.as_str(),
                "boundary" | "water" | "waterway" | "transportation" | "place"
            );
            if !wanted {
                continue;
            }
            // The tile's extent (4096 by convention) maps onto 512 px.
            let scale = f64::from(SIZE) / f64::from(layer.extent.max(1));
            let features = reader
                .get_features(layer.layer_index)
                .map_err(io::Error::other)?;
            match layer.name.as_str() {
                "boundary" => {
                    for feature in features.iter().filter(|f| is_admin_boundary(f)) {
                        stroke(feature, scale, true, &mut boundaries);
                    }
                }
                "water" | "waterway" => {
                    for feature in &features {
                        stroke(feature, scale, true, &mut water);
                    }
                }
                "transportation" => {
                    for feature in &features {
                        match string(feature, "class") {
                            Some(class) if MAJOR.contains(&class) => {
                                stroke(feature, scale, false, &mut major);
                            }
                            Some(class) if MINOR.contains(&class) => {
                                stroke(feature, scale, false, &mut minor);
                            }
                            _ => {}
                        }
                    }
                }
                "place" => labels.extend(features.iter().filter_map(|f| place(f, scale, key))),
                _ => {}
            }
        }
    }
    let pixels = tiles::compose(
        &boundaries.mask(BOUNDARY_WIDTH),
        &water.mask(COAST_WIDTH),
        Some((&minor.mask(MINOR_WIDTH), &major.mask(MAJOR_WIDTH))),
    );
    Ok((crate::sweep::png(SIZE, SIZE, &pixels)?, labels))
}
fn string<'a>(feature: &'a Feature, key: &str) -> Option<&'a str> {
    match feature.properties.as_ref()?.get(key)? {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}
fn number(feature: &Feature, key: &str) -> Option<i64> {
    match feature.properties.as_ref()?.get(key)? {
        Value::Int(i) | Value::SInt(i) => Some(*i),
        Value::UInt(u) => i64::try_from(*u).ok(),
        Value::Float(f) => Some(*f as i64),
        Value::Double(d) => Some(*d as i64),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}
/// Country and state lines on land: `admin_level` 2 or 4 with `maritime` 0.
fn is_admin_boundary(feature: &Feature) -> bool {
    matches!(number(feature, "admin_level"), Some(2 | 4))
        && number(feature, "maritime").unwrap_or(0) == 0
}
/// Add a feature's lines to `strokes`, in tile pixels. Polygon rings count
/// only when `rings` is set (water bodies, whose edges are shorelines; not
/// road areas).
fn stroke(feature: &Feature, scale: f64, rings: bool, strokes: &mut Strokes) {
    let mut add = |line: &LineString<f32>| {
        strokes.polyline(
            line.0
                .iter()
                .map(|c| (f64::from(c.x) * scale, f64::from(c.y) * scale)),
        );
    };
    match &feature.geometry {
        Geometry::LineString(line) => add(line),
        Geometry::MultiLineString(lines) => lines.0.iter().for_each(&mut add),
        Geometry::Polygon(polygon) if rings => {
            add(polygon.exterior());
            polygon.interiors().iter().for_each(&mut add);
        }
        Geometry::MultiPolygon(polygons) if rings => {
            for polygon in &polygons.0 {
                add(polygon.exterior());
                polygon.interiors().iter().for_each(&mut add);
            }
        }
        _ => {}
    }
}
/// A `place` feature inside the tile as a label: `name:en` over `name`,
/// class `capital` for a national capital, else city, town, or village.
fn place(feature: &Feature, scale: f64, key: TileKey) -> Option<Label> {
    let point = match &feature.geometry {
        Geometry::Point(point) => point.0,
        Geometry::MultiPoint(points) => points.0.first()?.0,
        _ => return None,
    };
    let (px, py) = (f64::from(point.x) * scale, f64::from(point.y) * scale);
    let inside = (0.0..f64::from(SIZE)).contains(&px) && (0.0..f64::from(SIZE)).contains(&py);
    if !inside {
        return None;
    }
    let class = string(feature, "class")?;
    if !PLACES.contains(&class) {
        return None;
    }
    let name = string(feature, "name:en")
        .filter(|n| !n.is_empty())
        .or_else(|| string(feature, "name"))
        .filter(|n| !n.is_empty())?;
    let class = if number(feature, "capital") == Some(2) {
        "capital"
    } else {
        class
    };
    let rank = number(feature, "rank")
        .and_then(|r| u32::try_from(r).ok())
        .unwrap_or(UNRANKED);
    let (lon, lat) = key.lon_lat(px, py);
    Some(Label {
        name: name.to_owned(),
        lat: (lat * 1e5).round() / 1e5,
        lon: (lon * 1e5).round() / 1e5,
        class: class.to_owned(),
        rank,
        region: String::new(),
        country: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::review::{DIR as REVIEW, preview};

    const RECORDED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/vt");

    /// A loopback response, optionally held open after its last supplied byte.
    /// No public service or disk cache is involved. Holding the response open
    /// distinguishes an in-flight size check from one that waits for EOF.
    async fn fetch_response(
        headers: &str,
        body: Vec<u8>,
        hold_open: bool,
    ) -> Result<Vec<u8>, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/tile", listener.local_addr().unwrap());
        let headers = headers.to_owned();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Consume the request before replying so closing does not reset a
            // connection with unread request bytes. Bound this test reader too.
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
                assert!(request.len() < 16 * 1024);
            }
            // Early rejection may close the connection before the server has
            // written everything. That is expected, not a server test failure.
            if socket.write_all(headers.as_bytes()).await.is_ok() {
                let _ = socket.write_all(&body).await;
            }
            if hold_open {
                let _ = released.await;
            }
        });
        // Only client and permits are used by get(); avoid Osm::open(), which
        // reads environment configuration and opens the user's persistent cache.
        let source = Osm {
            url: url.clone(),
            dir: PathBuf::new(),
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(FETCH_TIMEOUT)
                .build()
                .unwrap(),
            permits: Semaphore::new(IN_FLIGHT),
            tilejson: tokio::sync::Mutex::new(()),
            back_off: BACK_OFF,
            inner: Mutex::new(Inner {
                info: Info {
                    status: OsmStatus::Unavailable,
                    source: String::new(),
                    version: String::new(),
                    attribution: String::new(),
                },
                template: None,
                back_off_until: None,
                cache_bytes: 0,
                grown: false,
            }),
        };
        let result = tokio::time::timeout(Duration::from_secs(3), source.get(&url))
            .await
            .unwrap_or_else(|_| Err("still waiting for the response to finish".into()));
        let _ = release.send(());
        server.await.unwrap();
        result
    }

    fn chunked_body(size: usize, complete: bool) -> Vec<u8> {
        let mut body = format!("{size:x}\r\n").into_bytes();
        body.resize(body.len() + size, b'x');
        body.extend_from_slice(b"\r\n");
        if complete {
            body.extend_from_slice(b"0\r\n\r\n");
        }
        body
    }

    #[test]
    fn oversized_downloads_are_rejected_before_the_response_finishes() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                // A declared oversized length must be rejected before reading
                // any body. Unknown lengths must stop after crossing MAX_BODY,
                // without waiting for a closing chunk or connection close.
                for (name, headers, body) in [
                    (
                        "declared length",
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                            MAX_BODY + 1
                        ),
                        Vec::new(),
                    ),
                    (
                        "chunked",
                        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".into(),
                        chunked_body(MAX_BODY + 1, false),
                    ),
                    (
                        "close-delimited",
                        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".into(),
                        vec![b'x'; MAX_BODY + 1],
                    ),
                ] {
                    let result = fetch_response(&headers, body, true).await;
                    eprintln!("oversized {name}: {result:?}");
                    assert_eq!(result.unwrap_err(), "body over the size limit", "{name}");
                }
            });
    }

    #[test]
    fn downloads_accept_bodies_up_to_the_limit_and_preserve_http_status_handling() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for size in [0, 17, MAX_BODY - 1, MAX_BODY] {
                    let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n\r\n");
                    assert_eq!(
                        fetch_response(&headers, vec![b'x'; size], false)
                            .await
                            .unwrap(),
                        vec![b'x'; size]
                    );
                    let headers = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
                    assert_eq!(
                        fetch_response(headers, vec![b'x'; size], false)
                            .await
                            .unwrap(),
                        vec![b'x'; size]
                    );
                    let headers = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
                    let body = if size == 0 {
                        b"0\r\n\r\n".to_vec()
                    } else {
                        chunked_body(size, true)
                    };
                    assert_eq!(
                        fetch_response(headers, body, false).await.unwrap(),
                        vec![b'x'; size]
                    );
                }
                for status in ["204 No Content", "404 Not Found"] {
                    let headers = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
                    assert!(
                        fetch_response(&headers, Vec::new(), false)
                            .await
                            .unwrap()
                            .is_empty()
                    );
                }
                assert!(
                    fetch_response(
                        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
                        Vec::new(),
                        false
                    )
                    .await
                    .unwrap_err()
                    .starts_with("HTTP 503")
                );
            });
    }

    fn decode(png_bytes: &[u8]) -> Vec<u8> {
        let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (SIZE, SIZE));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        pixels.truncate(info.buffer_size());
        pixels
    }
    struct Counts {
        r: usize,
        g: usize,
        b: usize,
        major: usize,
        /// Major-road texels with intermediate coverage: the partial-alpha
        /// case the UI shader's division has to recover.
        partial_major: usize,
        partial_minor: usize,
    }
    fn count(pixels: &[u8]) -> Counts {
        let mut c = Counts {
            r: 0,
            g: 0,
            b: 0,
            major: 0,
            partial_major: 0,
            partial_minor: 0,
        };
        for p in pixels.as_chunks::<4>().0 {
            c.r += usize::from(p[0] != 0);
            c.g += usize::from(p[1] != 0);
            c.b += usize::from(p[2] != 0);
            c.major += usize::from(p[3] != 255);
            c.partial_major += usize::from(p[3] > 0 && p[3] < 255);
            c.partial_minor += usize::from(p[2] > 0 && p[2] < 255);
        }
        c
    }

    #[test]
    fn renders_recorded_tiles() {
        // `engine/data/vt/tiles.json` records where these came from.
        for (name, file, key, expect_labels, expect_minor) in [
            (
                "ktlx-z7-osm",
                "7-29-50.pbf",
                TileKey { z: 7, x: 29, y: 50 },
                true,
                false,
            ),
            (
                "ktlx-z11-osm",
                "11-470-808.pbf",
                TileKey {
                    z: 11,
                    x: 470,
                    y: 808,
                },
                false,
                true,
            ),
        ] {
            let bytes = fs::read(format!("{RECORDED}/{file}")).unwrap();
            let started = Instant::now();
            let (png_bytes, labels) = render(&bytes, key).unwrap();
            let elapsed = started.elapsed();
            assert_eq!(
                render(&bytes, key).unwrap().0,
                png_bytes,
                "{name} is deterministic"
            );
            let pixels = decode(&png_bytes);
            let c = count(&pixels);
            eprintln!(
                "{name} {key:?}: {elapsed:.1?}, {} bytes in, {} bytes out, R {} G {} B {} major {} (partial {}), minor partial {}, labels {:?}",
                bytes.len(),
                png_bytes.len(),
                c.r,
                c.g,
                c.b,
                c.major,
                c.partial_major,
                c.partial_minor,
                labels
                    .iter()
                    .map(|l| format!("{} ({}, {})", l.name, l.class, l.rank))
                    .collect::<Vec<_>>()
            );
            // Both tiles lie inside Oklahoma, away from any state line; the
            // `boundary` layer here holds only `aboriginal_lands` polygons,
            // which are not admin boundaries and must stay out of R.
            assert_eq!(c.r, 0, "{name}: no admin boundary crosses this tile");
            assert!(c.g > 0, "{name}: rivers and lakes cross this tile");
            assert!(c.major > 0, "{name}: an interstate crosses this tile");
            assert!(
                c.partial_major > 0,
                "{name}: antialiased road edges give partial A"
            );
            if expect_minor {
                assert!(
                    c.b > 0 && c.partial_minor > 0,
                    "{name}: secondary roads in B"
                );
            }
            // The z11 tile holds KTLX and Tinker AFB; its towns' points sit in
            // the tiles around it.
            assert_eq!(!labels.is_empty(), expect_labels, "{name}: {labels:?}");
            for label in &labels {
                assert!(["capital", "city", "town", "village"].contains(&label.class.as_str()));
                let (lon0, lat1) = key.lon_lat(0.0, 0.0);
                let (lon1, lat0) = key.lon_lat(f64::from(SIZE), f64::from(SIZE));
                assert!(
                    (lon0..=lon1).contains(&label.lon) && (lat0..=lat1).contains(&label.lat),
                    "{label:?}"
                );
            }
            fs::create_dir_all(REVIEW).unwrap();
            fs::write(format!("{REVIEW}/tile-{name}.png"), &png_bytes).unwrap();
            fs::write(
                format!("{REVIEW}/tile-{name}-preview.png"),
                crate::sweep::png(SIZE, SIZE, &preview(&pixels)).unwrap(),
            )
            .unwrap();
        }
        let (_, labels) = render(
            &fs::read(format!("{RECORDED}/7-29-50.pbf")).unwrap(),
            TileKey { z: 7, x: 29, y: 50 },
        )
        .unwrap();
        let city = labels.iter().find(|l| l.name == "Oklahoma City").unwrap();
        assert_eq!((city.class.as_str(), city.rank), ("city", 4));
        assert!((city.lat - 35.47).abs() < 0.05 && (city.lon + 97.52).abs() < 0.05);
        assert!(
            labels
                .iter()
                .any(|l| l.name == "Moore" && l.class == "town")
        );
    }

    #[test]
    fn an_empty_tile_is_opaque_and_blank() {
        let key = TileKey { z: 7, x: 0, y: 0 };
        let (png_bytes, labels) = render(&[], key).unwrap();
        assert!(labels.is_empty());
        let pixels = decode(&png_bytes);
        for p in pixels.as_chunks::<4>().0 {
            assert_eq!(p, &[0, 0, 0, 255]);
        }
        // Not a vector tile at all.
        assert!(render(b"\xff\xff\xff\xff", key).is_err());
    }

    #[test]
    fn tilejson_names_the_version_template_and_attribution() {
        let live = br#"{"tilejson":"3.0.0","tiles":["https://tiles.openfreemap.org/planet/20260830_080001_pt/{z}/{x}/{y}.pbf"],
            "attribution":"<a href=\"https://openfreemap.org\" target=\"_blank\">OpenFreeMap</a> <a href=\"https://www.openmaptiles.org/\" target=\"_blank\">&copy; OpenMapTiles</a> Data from <a href=\"https://www.openstreetmap.org/copyright\" target=\"_blank\">OpenStreetMap</a>",
            "name":"OpenFreeMap","version":"3.16.0","maxzoom":14}"#;
        assert_eq!(
            tilejson(live).unwrap(),
            Metadata {
                version: "20260830_080001_pt".into(),
                template: "https://tiles.openfreemap.org/planet/20260830_080001_pt/{z}/{x}/{y}.pbf"
                    .into(),
                name: "OpenFreeMap".into(),
                attribution: OPENFREEMAP_ATTRIBUTION.into(),
            }
        );
        // A template with no directory before {z} is versioned by its text.
        let flat = tilejson(br#"{"tiles":["http://127.0.0.1:1/{z}/{x}/{y}.pbf"]}"#).unwrap();
        assert_eq!(
            flat.version,
            tiles::tag("http://127.0.0.1:1/{z}/{x}/{y}.pbf")
        );
        assert_eq!(flat.name, "");
        assert_eq!(flat.attribution, "");
        assert!(tilejson(br#"{"tiles":["http://x/{z}.pbf"]}"#).is_err());
        assert!(tilejson(b"not json").is_err());
        assert_eq!(text("a &amp; <b>b</b>\n  c"), "a & b c");
    }

    #[test]
    fn eviction_drops_the_least_recently_touched() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/vt-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let base = SystemTime::now() - Duration::from_secs(1_000);
        for (i, name) in [
            "v1/7/1/1.pbf",
            "v1/7/1/2.pbf",
            "v2/7/1/1.pbf",
            "v2/7/1/2.pbf",
        ]
        .iter()
        .enumerate()
        {
            let path = dir.join(name);
            write(&path, &[0u8; 100]).unwrap();
            fs::File::open(&path)
                .unwrap()
                .set_modified(base + Duration::from_secs(i as u64))
                .unwrap();
        }
        // Reading a tile touches it, so it survives the eviction.
        touch(&dir.join("v1/7/1/1.pbf"));
        let (versions, total) = scan(&dir).unwrap();
        assert_eq!(versions, vec!["v1", "v2"]);
        assert_eq!(total, 400);
        assert_eq!(evict(&dir, 200).unwrap(), 200);
        assert!(dir.join("v1/7/1/1.pbf").exists(), "touched");
        assert!(!dir.join("v1/7/1/2.pbf").exists(), "oldest");
        assert!(!dir.join("v2/7/1/1.pbf").exists(), "next oldest");
        assert!(dir.join("v2/7/1/2.pbf").exists());
        // The current version and the newest other are kept.
        fs::create_dir_all(dir.join("v0")).unwrap();
        prune_versions(&dir, "v2").unwrap();
        assert!(dir.join("v2").exists() && dir.join("v1").exists() && !dir.join("v0").exists());
        prune_versions(&dir, "v3").unwrap();
        assert!(dir.join("v2").exists() && !dir.join("v1").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
