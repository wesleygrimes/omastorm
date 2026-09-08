mod catalog;
mod live;
mod osm;
mod protocol;
mod sweep;
mod tiles;

use catalog::Entry;
use chrono::{DateTime, Utc};
use protocol::{
    Basemap, Command, Connection, ConnectionStatus, Frame, FrameStatus, Geometry, Handshake, Hello,
    Message, NaturalEarth, Places, Rejection, SiteSelection, SiteTable, Source, State, Station,
    TileReady, TimelineEntry, VERSION, is_texture_path,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tiles::{Ready, Set, TileKey};
// The daemon runs on tokio (DESIGN.md, engine runtime); the launcher paths
// (`ensure`, `stop`) stay on std sockets, since they are short and sequential.
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader},
    net::UnixListener,
    sync::{
        Notify,
        mpsc::{self, Receiver, Sender},
    },
    task::{JoinHandle, spawn_blocking},
    time::{interval, sleep, timeout, timeout_at},
};

/// Development only: the path of an archived Level II volume to decode and
/// show at startup as `archived`, the way the checks and captures expect.
/// Unset, the daemon starts lean, with no frame, and waits for
/// `select_site`; nothing archived travels inside the binary.
const ARCHIVE_ENV: &str = "OMASTORM_ARCHIVE";
const MAX_LINE: u64 = 16 * 1024;
/// A client that cannot take one message in this long has stalled; its
/// connection is closed rather than letting it hold anything up.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a texture outlives its last reference from `state` (`docs/protocol.md`).
const RETIRE_AFTER: Duration = Duration::from_secs(30);
/// Messages queued for one client. A `tiles_needed` answer is up to 64
/// `tile_ready` lines in a burst, so the queue holds a couple of those; a
/// client that still falls behind is dropped by the next broadcast.
const QUEUE: usize = 128;
/// A reachable feed whose newest radial for the station is this old or older
/// is `stale`, and `unavailable` at three times that.
const STALE_AFTER: Duration = Duration::from_secs(600);
const UNAVAILABLE_AFTER: Duration = Duration::from_secs(1800);
/// Playback advances one frame per tick and loops (DESIGN.md, timeline).
const PLAY_LOOP: Duration = Duration::from_secs(10);
const PLAY_STEP_MIN: Duration = Duration::from_millis(250);
const PLAY_STEP_MAX: Duration = Duration::from_millis(1000);
/// Following hands off when a pan settles with another station closer than
/// this fraction of the current one's distance to the view centre, and
/// closer by at least `HANDOFF_MARGIN_KM` (DESIGN.md, site navigation).
/// Relative, so the dead band scales with the spacing: about a twentieth
/// of the distance between two stations on either side of their midpoint.
const HANDOFF_RATIO: f64 = 0.8;
const HANDOFF_MARGIN_KM: f64 = 1.0;

/// Fingerprint of the running executable, set once in `main`.
static BUILD: OnceLock<String> = OnceLock::new();

/// Hash the executable image this process is running. That covers every
/// source file, embedded asset, dependency, and compiler version without
/// enumerating them, so adding a module can never let a stale daemon pass as
/// current. Read through `/proc/self/exe`, which still opens the original
/// image after Cargo replaces the file on disk; the daemon hashes itself at
/// startup, before any rebuild.
fn fingerprint() -> io::Result<String> {
    let image = fs::read("/proc/self/exe").or_else(|_| fs::read(env::current_exe()?))?;
    let mut hash = DefaultHasher::new();
    image.hash(&mut hash);
    Ok(format!("{:016x}", hash.finish()))
}
fn build_id() -> &'static str {
    BUILD.get().expect("fingerprint is computed before use")
}
/// The embedded station snapshot (`engine/data/sites.json`).
fn site_table() -> SiteTable {
    serde_json::from_str(include_str!("../data/sites.json")).unwrap()
}
fn hello() -> Hello {
    let table = site_table();
    Hello {
        v: VERSION,
        engine: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        build: build_id().to_owned(),
        sites: table.sites,
        sites_source: table.source,
        sites_retrieved: table.retrieved,
        sites_notes: table.notes,
    }
}
fn fixture_frame() -> Frame {
    serde_json::from_str(include_str!("../data/fixture.json")).unwrap()
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
/// Milliseconds since the epoch as the protocol writes times.
fn iso(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
fn compact(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_default()
}
/// What a client can step through (`state.timeline`, `docs/protocol.md`):
/// the station's complete frames from the catalog, oldest first, and the
/// sweep in progress after them while one is painting. `shown` is the frame
/// the user stepped or sought to; `None` follows the newest, so each new
/// sweep replaces the picture (DESIGN.md, live sweeps as built).
#[derive(Default)]
struct Timeline {
    stored: Vec<Entry>,
    partial: Option<Entry>,
    shown: Option<String>,
}
impl Timeline {
    fn new(stored: Vec<Entry>) -> Self {
        Timeline {
            stored,
            partial: None,
            shown: None,
        }
    }
    fn len(&self) -> usize {
        self.stored.len() + usize::from(self.partial.is_some())
    }
    fn following(&self) -> bool {
        self.shown.is_none()
    }
    fn index_of(&self, id: &str) -> Option<usize> {
        self.stored.iter().position(|e| e.id == id).or_else(|| {
            self.partial
                .as_ref()
                .filter(|p| p.id == id)
                .map(|_| self.stored.len())
        })
    }
    fn id_at(&self, index: usize) -> Option<&str> {
        self.stored
            .get(index)
            .or(self.partial.as_ref().filter(|_| index == self.stored.len()))
            .map(|e| e.id.as_str())
    }
    /// Whether `index` names the sweep in progress rather than a stored frame.
    fn is_partial(&self, index: usize) -> bool {
        self.partial.is_some() && index == self.stored.len()
    }
    /// The frame on screen, `None` only while the timeline is empty.
    fn position(&self) -> Option<usize> {
        match &self.shown {
            None => self.len().checked_sub(1),
            Some(id) => self.index_of(id),
        }
    }
    /// Show the frame at `index`; landing on the newest follows again. The
    /// index to show when the position changed.
    fn pin(&mut self, index: usize) -> Option<usize> {
        let from = self.position();
        self.shown = if index + 1 == self.len() {
            None
        } else {
            self.id_at(index).map(str::to_owned)
        };
        (from != Some(index)).then_some(index)
    }
    /// `[` and `]`: `delta` frames from the one on screen, stopping at the ends.
    fn step(&mut self, delta: i64) -> Option<usize> {
        let from = self.position()? as i64;
        let to = (from + delta).clamp(0, self.len() as i64 - 1) as usize;
        self.pin(to)
    }
    /// Show frame `id`, or say it is not here.
    fn seek(&mut self, id: &str) -> Result<Option<usize>, ()> {
        let index = self.index_of(id).ok_or(())?;
        Ok(self.pin(index))
    }
    /// Playback: the next complete frame, wrapping to the oldest after the
    /// newest; nothing to loop over with fewer than two.
    fn advance(&mut self) -> Option<usize> {
        if self.stored.len() < 2 {
            return None;
        }
        let from = self.position().unwrap_or(0);
        let to = if from + 1 >= self.stored.len() {
            0
        } else {
            from + 1
        };
        self.pin(to)
    }
    /// A sweep began or grew: it is the newest entry until it completes or
    /// the next volume replaces it.
    fn begin(&mut self, entry: Entry) {
        self.partial = Some(entry);
    }
    /// A complete frame joined the catalog: it takes its place in time
    /// order and the ring drops the oldest past `RING`. True when the frame
    /// the user was pinned to fell off the ring; the pin moves to the
    /// oldest, and the caller shows it.
    fn complete(&mut self, entry: Entry) -> bool {
        self.partial = None;
        self.insert(entry)
    }
    /// How many complete frames the loop has.
    fn complete_count(&self) -> usize {
        self.stored.len()
    }
    /// A complete frame from an earlier volume (a backfill) or the live
    /// one, in time order; the sweep in progress is untouched. True when the
    /// pinned frame fell off the ring.
    fn insert(&mut self, entry: Entry) -> bool {
        self.stored.retain(|e| e.id != entry.id);
        let at = self
            .stored
            .partition_point(|e| e.start_ms <= entry.start_ms);
        self.stored.insert(at, entry);
        let excess = self.stored.len().saturating_sub(catalog::RING);
        self.stored.drain(..excess);
        match &self.shown {
            Some(id) if self.index_of(id).is_none() => {
                self.shown = self.stored.first().map(|e| e.id.clone());
                true
            }
            _ => false,
        }
    }
    fn entries(&self) -> Vec<TimelineEntry> {
        let entry = |e: &Entry, status| TimelineEntry {
            id: e.id.clone(),
            scan_time: e.scan_time.clone(),
            status,
        };
        self.stored
            .iter()
            .map(|e| entry(e, FrameStatus::Complete))
            .chain(self.partial.iter().map(|e| entry(e, FrameStatus::Partial)))
            .collect()
    }
}
/// The sweep in progress, held in memory so a client that stepped back can
/// return to it (End) between chunks without a texture on disk.
struct Pending {
    frame: Frame,
    texture: Vec<u8>,
    lut: Vec<u8>,
    /// Collection time of its newest radial, milliseconds since the epoch:
    /// the freshest evidence that the feed is up while it paints.
    end_ms: i64,
}
/// A live sweep encoded and, if complete, catalogued, on its way to the
/// timeline: the frame, its two textures, and the collection times of its
/// first and newest radial.
struct Arrival {
    frame: Frame,
    texture: Vec<u8>,
    lut: Vec<u8>,
    start_ms: i64,
    end_ms: i64,
}
/// The condition of a reachable feed from the age of the newest radial the
/// station has published: `Ok`, then `Stale`, then `Unavailable`. `Loading`
/// and `Offline` are not judged by age: a switch or the poller set them and
/// the next sweep clears them. `None` (no radial yet) keeps the current
/// condition.
fn feed_condition(current: ConnectionStatus, evidence_age: Option<u64>) -> ConnectionStatus {
    match (current, evidence_age) {
        (ConnectionStatus::Loading | ConnectionStatus::Offline, _) | (_, None) => current,
        (_, Some(age)) if age >= UNAVAILABLE_AFTER.as_secs() => ConnectionStatus::Unavailable,
        (_, Some(age)) if age >= STALE_AFTER.as_secs() => ConnectionStatus::Stale,
        _ => ConnectionStatus::Ok,
    }
}
/// A live station's frame from an assembled sweep: the fixture frame's
/// product, palette, and bounds (the engine's reflectivity vocabulary), the
/// sweep's geometry and times, and the station table's coordinates. Texture
/// paths are filled in by `publish_frame`.
fn live_frame(template: &Frame, station: &Station, sweep: &sweep::Sweep, complete: bool) -> Frame {
    Frame {
        id: format!("{}-{}-e0", station.id, compact(sweep.start_ms)),
        product: template.product.clone(),
        product_name: template.product_name.clone(),
        units: template.units.clone(),
        elevation_deg: (sweep.elevation_deg() * 100.0).round() / 100.0,
        scan_time: iso(sweep.start_ms),
        sweep_end: iso(sweep.end_ms),
        status: if complete {
            FrameStatus::Complete
        } else {
            FrameStatus::Partial
        },
        texture: String::new(),
        azimuth_lut: String::new(),
        rays: sweep.rows(),
        gates: u32::from(sweep.gates),
        first_gate_m: sweep.first_gate_m,
        gate_spacing_m: sweep.gate_spacing_m,
        scale: sweep.scale,
        offset: sweep.offset,
        site: Geometry {
            lat: station.lat,
            lon: station.lon,
            alt_m: station.alt_m,
        },
        palette: template.palette.clone(),
        bounds: template.bounds.clone(),
    }
}
/// The frame shown while a station's first live sweep loads and nothing is
/// cached: one blank row, so the shader draws nothing, at the station's
/// coordinates, with no scan time to show.
fn empty_frame(template: &Frame, station: &Station) -> Frame {
    Frame {
        id: format!("{}-loading", station.id),
        product: template.product.clone(),
        product_name: template.product_name.clone(),
        units: template.units.clone(),
        elevation_deg: 0.0,
        scan_time: String::new(),
        sweep_end: String::new(),
        status: FrameStatus::Partial,
        texture: String::new(),
        azimuth_lut: String::new(),
        rays: 1,
        gates: 1,
        first_gate_m: template.first_gate_m,
        gate_spacing_m: template.gate_spacing_m.max(1),
        scale: 0.0,
        offset: 0.0,
        site: Geometry {
            lat: station.lat,
            lon: station.lon,
            alt_m: station.alt_m,
        },
        palette: template.palette.clone(),
        bounds: template.bounds.clone(),
    }
}
/// The frame a lean daemon starts on before any `select_site`: the loading
/// placeholder for no station at all, sited at the middle of the contiguous
/// network so the map shows the whole set of markers until a station is
/// chosen. `site.id` is empty, so any home the UI names differs from it.
fn startup_frame(template: &Frame) -> Frame {
    let nowhere = Station {
        id: String::new(),
        name: String::new(),
        state: String::new(),
        lat: 39.8,
        lon: -98.6,
        alt_m: 0.0,
    };
    empty_frame(template, &nowhere)
}
/// The textures behind a loading placeholder: one blank gate, so the
/// shader draws nothing, and an azimuth lookup for it.
fn blank_textures(frame: &Frame) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let sweep = sweep::Sweep {
        rays: Vec::new(),
        start_ms: 0,
        end_ms: 0,
        gates: 1,
        first_gate_m: frame.first_gate_m,
        gate_spacing_m: frame.gate_spacing_m,
        scale: 1.0,
        offset: 0.0,
    };
    let texture = sweep::png(1, 1, &sweep.texture(&frame.bounds, frame.palette.len()))?;
    let lut = sweep::png(3600, 1, &sweep.azimuth_lut())?;
    Ok((texture, lut))
}
/// Great-circle distance in kilometres on the radar's 6371 km sphere.
fn great_circle_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.clamp(0.0, 1.0).sqrt().asin()
}
/// The station following should hand off to when the view centre settles
/// at `lat`, `lon`: the nearest table station, when it is not `current` and
/// beats it by the hysteresis rule. `None` keeps the current station, so a
/// centre between two stations does not flap. Nothing about the camera is
/// decided here: the centre is the user's.
fn handoff<'a>(sites: &'a [Station], current: &str, lat: f64, lon: f64) -> Option<&'a Station> {
    let distance = |s: &Station| great_circle_km(lat, lon, s.lat, s.lon);
    let nearest = sites
        .iter()
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))?;
    if nearest.id == current {
        return None;
    }
    match sites.iter().find(|s| s.id == current) {
        Some(held)
            if distance(nearest) >= HANDOFF_RATIO * distance(held)
                || distance(held) - distance(nearest) < HANDOFF_MARGIN_KM =>
        {
            None
        }
        _ => Some(nearest),
    }
}
fn initial_state(
    frame: Frame,
    osm: protocol::Osm,
    source: Source,
    status: ConnectionStatus,
) -> State {
    // Archived, the station is the one the frame names; lean, none yet.
    let site = match source {
        Source::Archived => frame.id.split('-').next().unwrap_or_default().to_owned(),
        Source::Live => String::new(),
    };
    State {
        v: VERSION,
        source,
        connection: Connection {
            status,
            age_seconds: 0,
        },
        site: SiteSelection {
            id: site,
            follow: true,
            locked: false,
        },
        // Filled from the `Timeline` at every snapshot.
        timeline: Vec::new(),
        frame,
        basemap: Basemap {
            ne: NaturalEarth {
                version: tiles::NE_VERSION,
            },
            osm,
        },
        playing: false,
    }
}
fn line(message: &Message) -> String {
    let mut text = serde_json::to_string(message).unwrap();
    text.push('\n');
    text
}
/// Store `value`, reporting whether anything changed.
fn set<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}
struct Shared {
    state: State,
    /// The tile masks rendered this session (`tiles.rs`).
    tiles: tiles::Store,
    /// Each client's bounded outgoing queue; `try_send` never waits, so a
    /// client that stops reading is dropped instead of blocking the others.
    clients: Vec<(u64, Sender<String>)>,
    next_client: u64,
    /// The fixture frame: the engine's reflectivity vocabulary (product,
    /// palette, bounds) that live frames share.
    template: Frame,
    /// The station table from `hello`.
    sites: Vec<Station>,
    /// The runtime directory textures are published to.
    dir: PathBuf,
    catalog: Arc<catalog::Catalog>,
    /// The poller for the selected station in live mode (`live.rs`);
    /// aborted and replaced by a site switch.
    live: Option<JoinHandle<()>>,
    events: Sender<live::Event>,
    /// `scanTime` of the newest complete frame, milliseconds since the
    /// epoch, for `connection.ageSeconds`; `None` while there is none.
    frame_ms: Option<i64>,
    /// The frames a client can step through, and which one is on screen.
    timeline: Timeline,
    /// The sweep in progress, when the timeline lists one.
    pending: Option<Pending>,
    /// Wakes the player task when `playing` becomes true.
    wake: Arc<Notify>,
    /// The last `state` line sent, so a tick that changed nothing is not
    /// re-sent.
    last_broadcast: String,
}
impl Shared {
    fn snapshot(&mut self) -> String {
        let age = self
            .frame_ms
            .map_or(0, |ms| now_ms().saturating_sub(ms).max(0) as u64 / 1000);
        self.state.connection.age_seconds = age;
        self.state.timeline = self.timeline.entries();
        // Live, the condition follows the newest radial received: the sweep
        // in progress while one paints, else the newest complete frame. A
        // half-finished cut from a station that then fell silent ages like
        // any other evidence.
        if self.state.source == Source::Live {
            let newest_ms = self
                .pending
                .as_ref()
                .map(|p| p.end_ms)
                .into_iter()
                .chain(self.frame_ms)
                .max();
            let evidence_age = newest_ms.map(|ms| now_ms().saturating_sub(ms).max(0) as u64 / 1000);
            self.state.connection.status =
                feed_condition(self.state.connection.status, evidence_age);
        }
        line(&Message::State(&self.state))
    }
    /// Show `frame` with its textures published under `tex/`.
    fn show(&mut self, mut frame: Frame, texture: &[u8], lut: &[u8]) -> io::Result<()> {
        publish_frame(&self.dir, &mut frame, texture, lut)?;
        self.state.frame = frame;
        Ok(())
    }
    /// Show the timeline's frame at `index`: the sweep in progress from
    /// memory, a stored frame read back from the catalog. Either way the
    /// textures are published under a new revision, since anything shown
    /// earlier may have been retired.
    fn show_position(&mut self, index: usize) -> io::Result<()> {
        if self.timeline.is_partial(index) {
            let pending = self
                .pending
                .as_ref()
                .ok_or_else(|| io::Error::other("the sweep in progress has no texture"))?;
            let mut frame = pending.frame.clone();
            publish_frame(&self.dir, &mut frame, &pending.texture, &pending.lut)?;
            self.state.frame = frame;
            return Ok(());
        }
        let id = self
            .timeline
            .id_at(index)
            .ok_or_else(|| io::Error::other(format!("no frame at {index}")))?
            .to_owned();
        let stored = self
            .catalog
            .load(&id)?
            .ok_or_else(|| io::Error::other(format!("{id} is no longer in the catalog")))?;
        self.show(stored.frame, &stored.texture, &stored.azimuth_lut)
    }
    /// Go live on a station: the newest cached frame (or an empty one)
    /// shows at once under `loading`, the timeline is the station's
    /// catalog, and a poller replaces the previous station's. Reselecting
    /// the live station changes nothing.
    fn select_site(&mut self, id: &str) -> (bool, Option<String>) {
        if id == self.state.site.id && self.state.source == Source::Live {
            return (false, None);
        }
        let Some(station) = self.sites.iter().find(|s| s.id == id).cloned() else {
            return (
                false,
                Some(format!("Unknown site {id}; stations are listed in hello.")),
            );
        };
        let listed = self.catalog.list(&station.id).unwrap_or_else(|e| {
            eprintln!("Frame catalog: {e}");
            Vec::new()
        });
        let cached = match listed.last() {
            Some(newest) => self.catalog.load(&newest.id).unwrap_or_else(|e| {
                eprintln!("Frame catalog: {e}");
                None
            }),
            None => None,
        };
        let shown = match cached {
            Some(stored) => {
                let start_ms = stored.start_ms;
                self.show(stored.frame, &stored.texture, &stored.azimuth_lut)
                    .map(|()| Some(start_ms))
            }
            None => {
                let frame = empty_frame(&self.template, &station);
                blank_textures(&frame)
                    .and_then(|(texture, lut)| self.show(frame, &texture, &lut))
                    .map(|()| None)
            }
        };
        let frame_ms = match shown {
            Ok(frame_ms) => frame_ms,
            Err(e) => {
                eprintln!("Publishing the frame for {}: {e}", station.id);
                return (
                    false,
                    Some(format!("Could not publish a frame for {}.", station.id)),
                );
            }
        };
        if let Some(task) = self.live.take() {
            task.abort();
        }
        self.frame_ms = frame_ms;
        let cached: Vec<i64> = listed.iter().map(|e| e.start_ms).collect();
        self.timeline = Timeline::new(listed);
        self.pending = None;
        self.state.playing = false;
        self.state.site.id = station.id.clone();
        self.state.source = Source::Live;
        self.state.connection.status = ConnectionStatus::Loading;
        self.live = Some(tokio::spawn(live::poll(
            station.id,
            self.events.clone(),
            cached,
        )));
        (true, None)
    }
    /// A pan settled with the map centred at `lat`, `lon`. While following and
    /// not locked, the nearest station takes over when it beats the current
    /// one by the hysteresis rule (`handoff`); the switch is a `select_site`,
    /// so an uncached station opens on the loading view. Locked, or with
    /// following off, the centre is noted for nothing.
    fn view_center(&mut self, lat: f64, lon: f64) -> (bool, Option<String>) {
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return (
                false,
                Some("view_center needs lat in [-90, 90] and lon in [-180, 180].".into()),
            );
        }
        if !self.state.site.follow || self.state.site.locked {
            return (false, None);
        }
        match handoff(&self.sites, &self.state.site.id, lat, lon) {
            Some(station) => {
                let id = station.id.clone();
                self.select_site(&id)
            }
            None => (false, None),
        }
    }
    /// A live sweep for the selected station grew or completed: it joins the
    /// timeline (a complete frame in time order, a growing one as the newest
    /// entry) and takes the screen while the user follows the newest frame.
    /// Broadcasts either way, since the timeline changed.
    fn arrived(&mut self, arrival: Arrival, complete: bool) -> io::Result<()> {
        let Arrival {
            frame,
            texture,
            lut,
            start_ms,
            end_ms,
        } = arrival;
        let entry = Entry {
            id: frame.id.clone(),
            scan_time: frame.scan_time.clone(),
            start_ms,
        };
        let following = self.timeline.following();
        self.state.connection.status = ConnectionStatus::Ok;
        let shown = if complete {
            self.frame_ms = Some(start_ms);
            self.pending = None;
            let dropped = self.timeline.complete(entry);
            if following {
                self.show(frame, &texture, &lut)
            } else if dropped {
                self.show_position(0)
            } else {
                Ok(())
            }
        } else {
            self.timeline.begin(entry);
            self.pending = Some(Pending {
                frame,
                texture,
                lut,
                end_ms,
            });
            if following {
                self.show_position(self.timeline.len() - 1)
            } else {
                Ok(())
            }
        };
        self.broadcast();
        shown
    }
    /// An earlier volume's frame joined the catalog: it takes its place in
    /// the timeline without touching the frame on screen, unless the pin
    /// fell off the ring. The age keeps following the newest frame.
    fn backfilled(&mut self, entry: Entry) -> io::Result<()> {
        if self.frame_ms.is_none_or(|ms| entry.start_ms > ms) {
            self.frame_ms = Some(entry.start_ms);
        }
        let shown = if self.timeline.insert(entry) {
            self.show_position(0)
        } else {
            Ok(())
        };
        self.broadcast();
        shown
    }
    /// `step` or `seek`: stop playback and show the frame the move lands on.
    /// A move that cannot be shown leaves the position where it was.
    fn navigate(
        &mut self,
        moved: impl FnOnce(&mut Timeline) -> Result<Option<usize>, &'static str>,
    ) -> (bool, Option<String>) {
        let before = self.timeline.shown.clone();
        let target = match moved(&mut self.timeline) {
            Ok(target) => target,
            Err(message) => return (false, Some(message.into())),
        };
        let paused = set(&mut self.state.playing, false);
        let Some(index) = target else {
            return (paused, None);
        };
        match self.show_position(index) {
            Ok(()) => (true, None),
            Err(e) => {
                eprintln!("Showing timeline frame {index}: {e}");
                self.timeline.shown = before;
                (
                    paused,
                    Some("Could not load the requested frame; keeping the current frame.".into()),
                )
            }
        }
    }
    /// Start playback when there is something to loop over.
    fn play(&mut self) -> bool {
        if self.state.playing || self.timeline.stored.len() < 2 {
            return false;
        }
        self.state.playing = true;
        self.wake.notify_one();
        true
    }
    /// One playback tick: the next frame, or the end of playback when it
    /// cannot be shown.
    fn tick(&mut self) {
        if !self.state.playing {
            return;
        }
        if let Some(index) = self.timeline.advance() {
            if let Err(e) = self.show_position(index) {
                eprintln!("Playback stopped at frame {index}: {e}");
                self.state.playing = false;
            }
            self.broadcast();
        }
    }
    fn broadcast(&mut self) {
        let message = self.snapshot();
        if message == self.last_broadcast {
            return;
        }
        self.last_broadcast = message.clone();
        // Never let a stalled UI hold up other clients. Its writer closes on EOF.
        self.clients
            .retain(|(_, client)| client.try_send(message.clone()).is_ok());
    }
    /// Carry the fetch path's condition into `state.basemap.osm`; a change
    /// is broadcast like any other.
    fn update_osm(&mut self, info: protocol::Osm) {
        if set(&mut self.state.basemap.osm, info) {
            self.broadcast();
        }
    }
    /// A well-formed command from a client; the reader has already logged
    /// `Unsupported` ones by name. Broadcasts if anything changed and returns
    /// the message for the sender when the command could not be carried out;
    /// a rejection never changes shared state.
    fn apply(&mut self, command: Command) -> Option<String> {
        let (changed, rejection) = match command {
            Command::SelectSite { id } => self.select_site(&id),
            Command::Follow { enabled } => (set(&mut self.state.site.follow, enabled), None),
            Command::Lock { enabled } => (set(&mut self.state.site.locked, enabled), None),
            Command::ViewCenter { lat, lon } => self.view_center(lat, lon),
            Command::Seek { id } => self.navigate(|timeline| {
                timeline.seek(&id).map_err(
                    |()| "Requested frame is not in the timeline; keeping the current frame.",
                )
            }),
            Command::Step { delta } => self.navigate(|timeline| Ok(timeline.step(delta))),
            Command::Play => (self.play(), None),
            Command::Pause => (set(&mut self.state.playing, false), None),
            Command::SetProduct {
                product,
                elevation_index: 0,
            } if product == self.state.frame.product => (false, None),
            Command::SetProduct { .. } => (
                false,
                Some("Only reflectivity at elevation index 0 is available in this build.".into()),
            ),
            // Tile requests and place search are answered to the sender, not state.
            Command::TilesNeeded { .. } | Command::SearchPlaces { .. } | Command::Unsupported => {
                return None;
            }
        };
        if changed {
            self.broadcast();
        }
        rejection
    }
}

/// `$XDG_RUNTIME_DIR/omastorm`, without creating it.
fn runtime_path() -> io::Result<PathBuf> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR is required"))?;
    if !Path::new(&base).is_absolute() {
        return Err(io::Error::other("XDG_RUNTIME_DIR must be absolute"));
    }
    Ok(PathBuf::from(base).join("omastorm"))
}
fn runtime() -> io::Result<PathBuf> {
    let dir = runtime_path()?;
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}
/// Write `bytes` under `tex/` as an immutable revision and return its
/// protocol path.
fn publish(dir: &Path, stem: &str, frame: &str, bytes: &[u8]) -> io::Result<String> {
    fs::create_dir_all(dir.join("tex"))?;
    // Nanosecond revision plus PID avoids Qt cache collisions across daemon restarts.
    let revision = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("tex/{stem}-{frame}-{}-r{revision}.png", std::process::id());
    if !is_texture_path(&name) {
        return Err(io::Error::other(format!(
            "Refusing to publish texture path {name:?}; see docs/protocol.md"
        )));
    }
    let temporary = dir.join(format!("{name}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, dir.join(&name))?;
    Ok(name)
}
/// Publish a frame's sweep texture and azimuth lookup under `tex/` and set
/// its paths.
fn publish_frame(dir: &Path, frame: &mut Frame, texture: &[u8], lut: &[u8]) -> io::Result<()> {
    frame.texture = publish(dir, "sweep", &frame.id, texture)?;
    frame.azimuth_lut = publish(dir, "azlut", &frame.id, lut)?;
    Ok(())
}
/// Encode a sweep's texture and lookup as PNGs.
fn encode(sweep: &sweep::Sweep, frame: &Frame) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let pixels = sweep.texture(&frame.bounds, frame.palette.len());
    let texture = sweep::png(u32::from(sweep.gates), sweep.rows(), &pixels)?;
    let lut = sweep::png(3600, 1, &sweep.azimuth_lut())?;
    Ok((texture, lut))
}
/// Decode the fixture's lowest sweep and publish the sweep texture and the
/// azimuth lookup for the initial frame; also its `scanTime` in milliseconds.
fn decode_and_publish(dir: &Path, template: &Frame, archive: &[u8]) -> io::Result<(Frame, i64)> {
    let started = Instant::now();
    let mut frame = template.clone();
    let sweep = sweep::lowest_reflectivity(archive)
        .map_err(|e| io::Error::other(format!("Decoding the archive: {e}")))?;
    let decoded = started.elapsed();
    frame.rays = sweep.rows();
    frame.gates = u32::from(sweep.gates);
    frame.first_gate_m = sweep.first_gate_m;
    frame.gate_spacing_m = sweep.gate_spacing_m;
    frame.scale = sweep.scale;
    frame.offset = sweep.offset;
    let (sweep_png, lut_png) = encode(&sweep, &frame)?;
    let encoded = started.elapsed();
    publish_frame(dir, &mut frame, &sweep_png, &lut_png)?;
    // The launcher waits a bounded time for the socket; keep these visible.
    eprintln!(
        "Archive ready: decoded in {decoded:.2?}, textures encoded by {encoded:.2?}, published by {:.2?}",
        started.elapsed()
    );
    Ok((frame, sweep.start_ms))
}
/// Turn the pollers' events into frames: encode on the blocking pool and
/// record complete frames in the catalog, then hand the frame to the
/// timeline, which publishes it if it is to be shown, and broadcast. An
/// event for a station that is no longer selected is dropped.
async fn live_events(shared: Arc<Mutex<Shared>>, mut events: Receiver<live::Event>) {
    while let Some(event) = events.recv().await {
        match event {
            live::Event::Sweep {
                site,
                sweep,
                complete,
                provenance,
            } => {
                let (frame, catalog) = {
                    let shared = shared.lock().unwrap();
                    if shared.state.site.id != site || shared.state.source != Source::Live {
                        continue;
                    }
                    let Some(station) = shared.sites.iter().find(|s| s.id == site) else {
                        continue;
                    };
                    (
                        live_frame(&shared.template, station, &sweep, complete),
                        shared.catalog.clone(),
                    )
                };
                let encoded = spawn_blocking(move || -> io::Result<Arrival> {
                    let started = Instant::now();
                    let (texture, lut) = encode(&sweep, &frame)?;
                    if complete {
                        catalog.store(
                            &site,
                            &frame,
                            sweep.start_ms,
                            &texture,
                            &lut,
                            &provenance,
                        )?;
                    }
                    eprintln!(
                        "{} Live {site}: {} rays {} in {:.0?}",
                        iso(now_ms()),
                        sweep.rays.len(),
                        if complete { "complete" } else { "partial" },
                        started.elapsed()
                    );
                    Ok(Arrival {
                        frame,
                        texture,
                        lut,
                        start_ms: sweep.start_ms,
                        end_ms: sweep.end_ms,
                    })
                })
                .await
                .map_err(io::Error::other)
                .and_then(|r| r);
                match encoded {
                    Ok(arrival) => {
                        let mut shared = shared.lock().unwrap();
                        // A switch while encoding: this frame belongs to the
                        // previous station's timeline, which is gone.
                        if !arrival.frame.id.starts_with(&shared.state.site.id)
                            || shared.state.source != Source::Live
                        {
                            continue;
                        }
                        if let Err(e) = shared.arrived(arrival, complete) {
                            eprintln!("Live frame: {e}");
                        }
                    }
                    Err(e) => eprintln!("Live frame: {e}"),
                }
            }
            live::Event::Backfill {
                site,
                sweep,
                provenance,
            } => {
                let (frame, catalog) = {
                    let shared = shared.lock().unwrap();
                    if shared.state.site.id != site || shared.state.source != Source::Live {
                        continue;
                    }
                    let Some(station) = shared.sites.iter().find(|s| s.id == site) else {
                        continue;
                    };
                    (
                        live_frame(&shared.template, station, &sweep, true),
                        shared.catalog.clone(),
                    )
                };
                let stored = spawn_blocking(move || -> io::Result<Entry> {
                    let (texture, lut) = encode(&sweep, &frame)?;
                    catalog.store(&site, &frame, sweep.start_ms, &texture, &lut, &provenance)?;
                    eprintln!(
                        "{} Live {site}: backfilled {} from {}",
                        iso(now_ms()),
                        frame.scan_time,
                        provenance
                    );
                    Ok(Entry {
                        id: frame.id,
                        scan_time: frame.scan_time,
                        start_ms: sweep.start_ms,
                    })
                })
                .await
                .map_err(io::Error::other)
                .and_then(|r| r);
                match stored {
                    Ok(entry) => {
                        let mut shared = shared.lock().unwrap();
                        if !entry.id.starts_with(&shared.state.site.id)
                            || shared.state.source != Source::Live
                        {
                            continue;
                        }
                        if let Err(e) = shared.backfilled(entry) {
                            eprintln!("Backfill frame: {e}");
                        }
                    }
                    Err(e) => eprintln!("Backfill frame: {e}"),
                }
            }
            live::Event::Offline { site, reason } => {
                report(&shared, &site, &reason, ConnectionStatus::Offline);
            }
            live::Event::Silent { site, reason } => {
                report(&shared, &site, &reason, ConnectionStatus::Unavailable);
            }
        }
    }
}
/// The poller's word on the feed for `site`: `Offline` when the bucket could
/// not be reached, `Unavailable` when it answered with nothing for the
/// station. Broadcast if the condition changed; ignored for a station no
/// longer selected.
fn report(shared: &Mutex<Shared>, site: &str, reason: &str, condition: ConnectionStatus) {
    let mut shared = shared.lock().unwrap();
    if shared.state.site.id != site || shared.state.source != Source::Live {
        return;
    }
    eprintln!("Live {site}: {reason}");
    if set(&mut shared.state.connection.status, condition) {
        shared.broadcast();
    }
}
/// Playback: woken by `play`, it advances the timeline one frame per
/// `PLAY_INTERVAL` until `playing` is cleared, then waits for the next wake.
async fn player(shared: Arc<Mutex<Shared>>, wake: Arc<Notify>) {
    loop {
        wake.notified().await;
        loop {
            // The step is re-judged every frame, so a backfill landing
            // mid-loop slows the loop rather than shortening it.
            let step = {
                let shared = shared.lock().unwrap();
                if !shared.state.playing {
                    break;
                }
                play_step(shared.timeline.complete_count())
            };
            sleep(step).await;
            let mut shared = shared.lock().unwrap();
            if !shared.state.playing {
                break;
            }
            shared.tick();
        }
    }
}
/// Playback loops the complete frames over about `PLAY_LOOP` however many
/// there are, within `PLAY_STEP_MIN` to `PLAY_STEP_MAX` per frame: a fresh
/// station's dozen backfilled frames turn slowly enough to read, a full
/// ring turns at four frames a second.
fn play_step(frames: usize) -> Duration {
    (PLAY_LOOP / frames.max(1) as u32).clamp(PLAY_STEP_MIN, PLAY_STEP_MAX)
}
/// Files under `tex/` that no `state` references, keyed by when each was first
/// seen unreferenced. Time is measured from observation rather than from file
/// timestamps, so a file left by an earlier daemon counts from this daemon's
/// start: a texture served immediately before a crash survives the grace period.
#[derive(Default)]
struct Retirement {
    unreferenced: HashMap<PathBuf, Instant>,
}
impl Retirement {
    /// Given the files present and the files the state references, return
    /// those whose grace period has run out.
    fn sweep(
        &mut self,
        present: impl IntoIterator<Item = PathBuf>,
        referenced: &HashSet<PathBuf>,
        now: Instant,
    ) -> Vec<PathBuf> {
        let mut seen = HashMap::new();
        for path in present {
            if !referenced.contains(&path) {
                let since = self.unreferenced.get(&path).copied().unwrap_or(now);
                seen.insert(path, since);
            }
        }
        // Referenced again, or already gone: forget it. Deleted below: forget it too.
        self.unreferenced = seen;
        let expired: Vec<PathBuf> = self
            .unreferenced
            .iter()
            .filter(|(_, since)| now.duration_since(**since) >= RETIRE_AFTER)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &expired {
            self.unreferenced.remove(path);
        }
        expired
    }
}
fn cleanup(dir: &Path, shared: &Mutex<Shared>, retirement: &mut Retirement) -> io::Result<()> {
    let referenced: HashSet<PathBuf> = shared
        .lock()
        .unwrap()
        .state
        .referenced_files()
        .map(|path| dir.join(path))
        .collect();
    let mut present = Vec::new();
    for entry in fs::read_dir(dir.join("tex"))? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            present.push(entry.path());
        }
    }
    for path in retirement.sweep(present, &referenced, Instant::now()) {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
/// One line from a client. Rejections go back on `reply`, that client's own
/// queue, so no other client hears about a command it did not send; a tile
/// request goes to that client's tile task on `tiles`.
fn receive(
    shared: &Mutex<Shared>,
    reply: &Sender<String>,
    tiles: &Sender<tiles::Request>,
    bytes: &[u8],
) {
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) if value.is_object() => value,
        _ => return eprintln!("Ignoring malformed command"),
    };
    let kind = value["type"].as_str().unwrap_or_default();
    let message = match Command::deserialize(&value) {
        // A command newer than this build; the sender is not told, since
        // ignoring it is the documented answer (docs/protocol.md).
        Ok(Command::Unsupported) => return eprintln!("Ignoring unsupported command: {kind}"),
        Ok(Command::TilesNeeded { z, x0, y0, x1, y1 }) => {
            match (tiles::Request { z, x0, y0, x1, y1 }).validate() {
                // A full request queue means the client is flooding; the
                // newest request supersedes anyway, so dropping is harmless.
                Ok(request) => {
                    let _ = tiles.try_send(request);
                    return;
                }
                Err(reason) => format!("Invalid tiles_needed command: {reason}."),
            }
        }
        Ok(Command::SearchPlaces { query, lat, lon }) => {
            if query.chars().count() > 200 {
                "search_places query is too long.".into()
            } else {
                let origin = match (lat, lon) {
                    (None, None) => None,
                    (Some(lat), Some(lon))
                        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) =>
                    {
                        Some((lat, lon))
                    }
                    _ => {
                        let message =
                            "search_places needs lat in [-90, 90] and lon in [-180, 180].";
                        let rejection = Rejection {
                            v: VERSION,
                            command: kind,
                            message,
                        };
                        let _ = reply.try_send(line(&Message::Error(&rejection)));
                        return;
                    }
                };
                let results = tiles::search_places(&query, origin, 8);
                let message = Places {
                    v: VERSION,
                    query: &query,
                    results: &results,
                };
                let _ = reply.try_send(line(&Message::Places(&message)));
                return;
            }
        }
        Ok(command) => match shared.lock().unwrap().apply(command) {
            Some(message) => message,
            None => return,
        },
        Err(e) => format!("Invalid {kind} command: {e}."),
    };
    let rejection = Rejection {
        v: VERSION,
        command: kind,
        message: &message,
    };
    // A full queue means this client has stalled; the next broadcast drops it.
    let _ = reply.try_send(line(&Message::Error(&rejection)));
}
/// Register a connection and start its two tasks: a writer draining the
/// client's queue and a reader turning its lines into commands. Must run
/// inside the runtime.
fn client(
    stream: tokio::net::UnixStream,
    shared: Arc<Mutex<Shared>>,
    osm: Arc<osm::Osm>,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<String>(QUEUE);
    let (tiles_tx, tiles_rx) = mpsc::channel::<tiles::Request>(8);
    let id;
    {
        let mut shared = shared.lock().unwrap();
        let snapshot = shared.snapshot();
        if shared.clients.len() >= 64 {
            return Err(io::Error::other("Too many clients"));
        }
        tx.try_send(line(&Message::Hello(&hello()))).unwrap();
        tx.try_send(snapshot).unwrap();
        id = shared.next_client;
        shared.next_client += 1;
        shared.clients.push((id, tx.clone()));
    }
    tokio::spawn(serve_tiles(shared.clone(), osm, tx.clone(), tiles_rx));
    tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if !matches!(
                timeout(WRITE_TIMEOUT, writer.write_all(message.as_bytes())).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        // Every sender is gone (the reader ended and the client was removed)
        // or the client stalled: either way the connection is finished.
        let _ = writer.shutdown().await;
    });
    tokio::spawn(async move {
        // Bound allocation even when a client never sends a newline.
        let mut reader = AsyncBufReader::new(reader).take(MAX_LINE + 1);
        loop {
            let mut bytes = Vec::new();
            reader.set_limit(MAX_LINE + 1);
            let read = reader.read_until(b'\n', &mut bytes).await;
            if !matches!(read, Ok(n) if n > 0)
                || bytes.len() as u64 > MAX_LINE
                || !bytes.ends_with(b"\n")
            {
                break;
            }
            receive(&shared, &tx, &tiles_tx, &bytes);
        }
        shared
            .lock()
            .unwrap()
            .clients
            .retain(|(client_id, _)| *client_id != id);
        // `tx` drops here; with the clone in `clients` gone, the writer's
        // queue closes and it shuts the socket down.
    });
    Ok(())
}
/// Answer one client's `tiles_needed` requests, tile by tile, centre-out.
/// A tile rendered this session is answered at once; another is drawn on
/// the blocking pool and published, then announced. From z7 the vector tile
/// is fetched (or read from the cache) and the tile answered as `osm`; when
/// it cannot be, the tile is answered as `ne` and asked for again once the
/// back-off passes, so the client hears it a second time under a new path.
/// A newer request from the same client supersedes the pending tiles
/// outside its rectangle, and its own tiles follow. Ends when the client's
/// queue closes.
async fn serve_tiles(
    shared: Arc<Mutex<Shared>>,
    osm: Arc<osm::Osm>,
    reply: Sender<String>,
    mut requests: Receiver<tiles::Request>,
) {
    let dir = match runtime_path() {
        Ok(dir) => dir,
        Err(e) => return eprintln!("Tiles: {e}"),
    };
    let geography = tiles::Geography::embedded();
    let Some(mut request) = requests.recv().await else {
        return;
    };
    let mut queue = request.centre_out();
    loop {
        let mut newer = None;
        let mut announced = HashSet::new();
        let mut stand_ins = Vec::new();
        for key in queue {
            // The newest request decides which pending tiles are still wanted.
            while let Ok(latest) = requests.try_recv() {
                newer = Some(latest);
            }
            if newer.is_some_and(|latest| !latest.contains(key)) || !announced.insert(key) {
                continue;
            }
            let mut served = None;
            if (osm::FROM_ZOOM..=osm::MAX_ZOOM).contains(&key.z) {
                match serve_osm(&shared, &osm, &dir, key).await {
                    Ok(Some(ready)) => served = Some((Set::Osm, ready)),
                    Ok(None) => stand_ins.push(key),
                    Err(e) => eprintln!("Tile {key:?} (osm): {e}"),
                }
                shared.lock().unwrap().update_osm(osm.info());
            }
            let (set, ready) = match served {
                Some(served) => served,
                None => match serve_ne(&shared, geography, &dir, key).await {
                    Ok(ready) => (Set::Ne, ready),
                    Err(e) => {
                        eprintln!("Tile {key:?}: {e}");
                        continue;
                    }
                },
            };
            let message = TileReady {
                v: VERSION,
                set: set.name(),
                z: key.z,
                x: key.x,
                y: key.y,
                path: &ready.path,
                labels: ready.labels,
            };
            if reply
                .send(line(&Message::TileReady(&message)))
                .await
                .is_err()
            {
                return;
            }
        }
        osm.trim().await;
        // The next pass: a newer request, or the `ne` stand-ins once the
        // back-off has passed, unless a request arrives first.
        queue = match newer {
            Some(latest) => {
                request = latest;
                request.centre_out()
            }
            None if stand_ins.is_empty() => match requests.recv().await {
                Some(next) => {
                    request = next;
                    request.centre_out()
                }
                None => return,
            },
            None => match timeout_at(osm.retry_at().into(), requests.recv()).await {
                Ok(Some(next)) => {
                    request = next;
                    request.centre_out()
                }
                Ok(None) => return,
                Err(_) => stand_ins,
            },
        };
    }
}
/// Publish `png` under `path` on the blocking pool and record it, deleting
/// what fell off the store's cap.
async fn publish_tile(
    shared: &Mutex<Shared>,
    dir: &Path,
    set: Set,
    key: TileKey,
    render: impl FnOnce() -> io::Result<(Vec<u8>, Vec<protocol::Label>)> + Send + 'static,
) -> io::Result<Ready> {
    let path = shared.lock().unwrap().tiles.path(set, key);
    let (root, name) = (dir.to_path_buf(), path.clone());
    let labels = spawn_blocking(move || {
        let (png, labels) = render()?;
        tiles::write(&root, &name, &png)?;
        Ok::<_, io::Error>(labels)
    })
    .await
    .map_err(io::Error::other)??;
    let ready = Ready { path, labels };
    let evicted = shared
        .lock()
        .unwrap()
        .tiles
        .announce(set, key, ready.clone());
    for old in evicted {
        let _ = fs::remove_file(dir.join(old));
    }
    Ok(ready)
}
/// The `ne` tile, rendered unless this session already has it.
async fn serve_ne(
    shared: &Mutex<Shared>,
    geography: &'static tiles::Geography,
    dir: &Path,
    key: TileKey,
) -> io::Result<Ready> {
    if let Some(ready) = shared.lock().unwrap().tiles.ready(Set::Ne, key).cloned() {
        return Ok(ready);
    }
    publish_tile(shared, dir, Set::Ne, key, move || {
        Ok((
            tiles::render(geography, key)?,
            tiles::labels(geography, key),
        ))
    })
    .await
}
/// The `osm` tile, rendered from the cached or fetched vector tile unless
/// this session already has it; `None` when the vector tile cannot be had
/// right now.
async fn serve_osm(
    shared: &Mutex<Shared>,
    osm: &osm::Osm,
    dir: &Path,
    key: TileKey,
) -> io::Result<Option<Ready>> {
    if let Some(ready) = shared.lock().unwrap().tiles.ready(Set::Osm, key).cloned() {
        return Ok(Some(ready));
    }
    let Some(bytes) = osm.tile(key).await else {
        return Ok(None);
    };
    // The version is known once a tile is: name this generation after it.
    shared
        .lock()
        .unwrap()
        .tiles
        .osm_version(&osm.info().version);
    publish_tile(shared, dir, Set::Osm, key, move || osm::render(&bytes, key))
        .await
        .map(Some)
}
/// Run the daemon: decode and publish synchronously, then serve on a
/// current-thread tokio runtime with the texture cleanup on an interval and
/// a pair of tasks per client.
fn serve(dir: PathBuf) -> io::Result<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("engine.lock"))?;
    lock.try_lock()
        .map_err(|e| io::Error::other(format!("Engine already running: {e}")))?;
    let socket = dir.join("engine.sock");
    // Only the lock owner can recover a stale socket or publish textures.
    match fs::remove_file(&socket) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let template = fixture_frame();
    // Development runs may start on an archived volume; the shipped daemon
    // starts with no frame and goes live on the first `select_site`.
    let archive = env::var_os(ARCHIVE_ENV).filter(|path| !path.is_empty());
    let (frame, frame_ms, entries, source, status) = match archive {
        Some(path) => {
            let bytes = fs::read(&path).map_err(|e| {
                io::Error::other(format!(
                    "Reading {ARCHIVE_ENV} {}: {e}",
                    Path::new(&path).display()
                ))
            })?;
            let (frame, ms) = decode_and_publish(&dir, &template, &bytes)?;
            let entry = Entry {
                id: frame.id.clone(),
                scan_time: frame.scan_time.clone(),
                start_ms: ms,
            };
            (
                frame,
                Some(ms),
                vec![entry],
                Source::Archived,
                ConnectionStatus::Ok,
            )
        }
        None => {
            let mut frame = startup_frame(&template);
            let (texture, lut) = blank_textures(&frame)?;
            publish_frame(&dir, &mut frame, &texture, &lut)?;
            eprintln!("Ready with no frame; waiting for select_site");
            (
                frame,
                None,
                Vec::new(),
                Source::Live,
                ConnectionStatus::Loading,
            )
        }
    };
    // Tile names end in a generation tag so pixels never change under a name
    // the UI has cached: eight hex digits of the build fingerprint (`ne`
    // geography travels inside the binary, so the build is its version).
    let tile_store = tiles::Store::open(&dir, &build_id()[..8])?;
    // Opens the vector tile cache and builds the HTTP client; fetches nothing.
    let osm = Arc::new(osm::Osm::open()?);
    // The frame ring buffer; live frames are written here as they complete.
    let catalog = Arc::new(catalog::Catalog::open(osm::cache_root()?.join("frames"))?);
    let (events, event_rx) = mpsc::channel(16);
    let wake = Arc::new(Notify::new());
    let shared = Arc::new(Mutex::new(Shared {
        state: initial_state(frame, osm.info(), source, status),
        tiles: tile_store,
        clients: Vec::new(),
        next_client: 0,
        template,
        sites: hello().sites,
        dir: dir.clone(),
        catalog,
        live: None,
        events,
        frame_ms,
        timeline: Timeline::new(entries),
        pending: None,
        wake: wake.clone(),
        last_broadcast: String::new(),
    }));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    // Bind needs the reactor, so it happens inside the runtime; a bind
    // failure ends `serve` with the lock still held until we return.
    let _guard = runtime.enter();
    let listener = UnixListener::bind(&socket)?;
    runtime.spawn(live_events(shared.clone(), event_rx));
    runtime.spawn(player(shared.clone(), wake));
    let cleanup_shared = shared.clone();
    runtime.spawn(async move {
        let mut retirement = Retirement::default();
        // The first tick completes at once, so cleanup runs at startup too.
        let mut tick = interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            if let Err(e) = cleanup(&dir, &cleanup_shared, &mut retirement) {
                eprintln!("Texture cleanup: {e}");
            }
            // Once a second while live, `ageSeconds` and `stale` move on a
            // quiet feed; `broadcast` sends nothing when nothing changed.
            let mut shared = cleanup_shared.lock().unwrap();
            if shared.state.source == Source::Live {
                shared.broadcast();
            }
        }
    });
    runtime.block_on(async {
        loop {
            match listener
                .accept()
                .await
                .and_then(|(stream, _)| client(stream, shared.clone(), osm.clone()))
            {
                Ok(()) => {}
                Err(e) => eprintln!("Client: {e}"),
            }
        }
    })
}
/// The hello of whatever daemon answers on the socket, of any build, or
/// `None` when nothing accepts the connection.
fn handshake(dir: &Path) -> io::Result<Option<Handshake>> {
    let Ok(stream) = UnixStream::connect(dir.join("engine.sock")) else {
        return Ok(None);
    };
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut reader = BufReader::new(stream).take(64 * 1024);
    let mut text = String::new();
    // A daemon that has bound the socket but not yet published its hello is
    // starting, not broken: the read timeout means "not ready", the same
    // answer as no socket at all, so `ensure` keeps waiting out its deadline.
    match reader.read_line(&mut text) {
        Ok(_) => {}
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    let handshake: Handshake = serde_json::from_str(&text).map_err(io::Error::other)?;
    if handshake.kind != "hello" {
        return Err(io::Error::other(
            "Existing engine did not answer with a hello",
        ));
    }
    Ok(Some(handshake))
}
/// Whether a daemon of this build answers on the socket. A daemon of another
/// build or protocol is ended here (decided 2026-09-06, DESIGN.md): windows
/// reconnect within a second, while refusing it left the launch key dead
/// after every rebuild until a manual `stop`.
fn ready(dir: &Path) -> io::Result<bool> {
    let Some(handshake) = handshake(dir)? else {
        return Ok(false);
    };
    if handshake.v != VERSION || handshake.build != build_id() {
        terminate(dir, handshake.pid)?;
        eprintln!(
            "Stopped engine of another build (PID {}); starting this build.",
            handshake.pid
        );
        return Ok(false);
    }
    Ok(true)
}
/// Whether no daemon holds `engine.lock`. Taking the lock briefly here is
/// harmless: only `serve` keeps it, and a `serve` racing us simply waits.
fn lock_is_free(dir: &Path) -> io::Result<bool> {
    match OpenOptions::new().write(true).open(dir.join("engine.lock")) {
        Ok(lock) => Ok(lock.try_lock().is_ok()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}
/// End the daemon answering on the socket, whatever its build, and wait until
/// it has released the socket and the lock. No daemon means nothing to do.
/// The daemon needs no signal handler: the OS releases its lock and `serve`
/// already recovers a leftover socket file.
fn stop(dir: PathBuf) -> io::Result<()> {
    let Some(handshake) = handshake(&dir)? else {
        return Ok(());
    };
    terminate(&dir, handshake.pid)?;
    println!("Stopped engine (PID {})", handshake.pid);
    Ok(())
}
/// Signal the daemon with `pid` and wait up to 2 s until its socket refuses
/// connections and `engine.lock` is free.
fn terminate(dir: &Path, pid: u32) -> io::Result<()> {
    // 0 would signal our own process group and 1 is init; neither is a daemon.
    let target = libc::pid_t::try_from(pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| io::Error::other(format!("Existing engine reports PID {pid}")))?;
    // SAFETY: kill(2) only sends a signal; it touches no memory of ours.
    if unsafe { libc::kill(target, libc::SIGTERM) } != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(io::Error::other(format!("Cannot signal PID {pid}: {e}")));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let refused = UnixStream::connect(dir.join("engine.sock")).is_err();
        if refused && lock_is_free(dir)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "Engine (PID {pid}) did not stop within 2 s"
            )));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn ensure(dir: PathBuf) -> io::Result<()> {
    if ready(&dir)? {
        return Ok(());
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("engine.log"))?;
    let mut child = Process::new(env::current_exe()?)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()?;
    // Startup opens the caches and publishes a blank frame (an archived
    // development volume adds about 0.3 s; timings go to engine.log). Allow
    // far more than that, so a slow disk or a busy machine gets a slow
    // launch rather than a killed daemon.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if ready(&dir)? {
            return Ok(());
        }
        let _ = child.try_wait()?; // A concurrent launcher may have won the lock.
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::other(format!(
        "Engine did not become ready; see {}",
        dir.join("engine.log").display()
    )))
}
fn main() -> io::Result<()> {
    BUILD.set(fingerprint()?).expect("set once");
    let mode = env::args().nth(1).unwrap_or_else(|| "serve".into());
    match mode.as_str() {
        "serve" => serve(runtime()?),
        "ensure" => ensure(runtime()?),
        "stop" => stop(runtime_path()?),
        _ => Err(io::Error::other(
            "Usage: omastorm-engine [serve|ensure|stop]",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> HashSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }
    fn files(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }
    fn entry(minute: i64) -> Entry {
        Entry {
            id: format!("KJAX-20260907T00{minute:02}00Z-e0"),
            scan_time: format!("2026-09-07T00:{minute:02}:00Z"),
            start_ms: 1_788_998_400_000 + minute * 60_000,
        }
    }
    fn ids(timeline: &Timeline) -> Vec<(String, FrameStatus)> {
        timeline
            .entries()
            .into_iter()
            .map(|e| (e.id, e.status))
            .collect()
    }

    #[test]
    fn stepping_pins_a_frame_and_the_newest_follows_again() {
        let mut timeline = Timeline::new(vec![entry(0), entry(5), entry(10)]);
        assert!(timeline.following());
        assert_eq!(timeline.position(), Some(2));
        // Steps stop at the ends; a step that goes nowhere is not a change.
        assert_eq!(timeline.step(1), None);
        assert_eq!(timeline.step(-1), Some(1));
        assert!(!timeline.following());
        assert_eq!(timeline.step(-5), Some(0));
        assert_eq!(timeline.step(-1), None);
        assert_eq!(timeline.position(), Some(0));
        // Landing on the newest frame follows again.
        assert_eq!(timeline.step(2), Some(2));
        assert!(timeline.following());
        // Seeking: a listed id, the same id, an unknown id.
        assert_eq!(timeline.seek(&entry(5).id), Ok(Some(1)));
        assert_eq!(timeline.seek(&entry(5).id), Ok(None));
        assert_eq!(timeline.seek("KJAX-nowhere"), Err(()));
        assert_eq!(timeline.position(), Some(1));
    }

    #[test]
    fn a_sweep_in_progress_is_the_newest_entry_until_it_completes() {
        let mut timeline = Timeline::new(vec![entry(0), entry(5)]);
        timeline.begin(entry(10));
        assert_eq!(
            ids(&timeline),
            vec![
                (entry(0).id, FrameStatus::Complete),
                (entry(5).id, FrameStatus::Complete),
                (entry(10).id, FrameStatus::Partial),
            ]
        );
        assert!(timeline.is_partial(2) && !timeline.is_partial(1));
        // Following: the partial sweep is on screen; stepping back leaves it
        // and End (seek to the newest) returns to it.
        assert_eq!(timeline.position(), Some(2));
        assert_eq!(timeline.step(-1), Some(1));
        assert!(!timeline.following());
        assert_eq!(timeline.seek(&entry(10).id), Ok(Some(2)));
        assert!(timeline.following());
        // The cut completes: the entry becomes complete, still followed.
        assert!(!timeline.complete(entry(10)));
        assert_eq!(timeline.len(), 3);
        assert!(
            timeline
                .entries()
                .iter()
                .all(|e| e.status == FrameStatus::Complete)
        );
        assert!(timeline.following());
        // A pinned frame stays pinned while sweeps arrive.
        assert_eq!(timeline.step(-2), Some(0));
        timeline.begin(entry(15));
        assert_eq!(timeline.position(), Some(0));
        assert_eq!(timeline.len(), 4);
        assert!(!timeline.complete(entry(15)));
        assert_eq!(timeline.position(), Some(0));
        // A complete frame lands in time order even when it arrives late.
        assert!(!timeline.complete(entry(12)));
        assert_eq!(timeline.id_at(3), Some(entry(12).id.as_str()));
        assert_eq!(timeline.id_at(4), Some(entry(15).id.as_str()));
    }

    #[test]
    fn the_ring_drops_the_oldest_and_moves_a_pin_that_fell_off() {
        let mut timeline = Timeline::new((0..catalog::RING as i64).map(entry).collect());
        assert_eq!(timeline.step(-(catalog::RING as i64)), Some(0));
        assert!(timeline.complete(entry(catalog::RING as i64)));
        assert_eq!(timeline.len(), catalog::RING);
        assert_eq!(timeline.position(), Some(0));
        assert_eq!(timeline.id_at(0), Some(entry(1).id.as_str()));
        // Pinned elsewhere, the pin keeps its frame while the index shifts.
        assert_eq!(timeline.step(5), Some(5));
        assert!(!timeline.complete(entry(catalog::RING as i64 + 1)));
        assert_eq!(timeline.position(), Some(4));
        assert_eq!(timeline.id_at(4), Some(entry(6).id.as_str()));
    }

    #[test]
    fn playback_paces_the_loop_to_ten_seconds() {
        assert_eq!(play_step(1).as_millis(), 1000);
        assert_eq!(play_step(12).as_millis(), 833);
        assert_eq!(play_step(36).as_millis(), 277);
        assert_eq!(play_step(60).as_millis(), 250);
    }
    #[test]
    fn a_backfilled_frame_keeps_the_sweep_in_progress() {
        let mut timeline = Timeline::new(vec![entry(10)]);
        timeline.begin(entry(20));
        assert!(!timeline.insert(entry(5)));
        assert_eq!(
            ids(&timeline),
            vec![
                (entry(5).id, FrameStatus::Complete),
                (entry(10).id, FrameStatus::Complete),
                (entry(20).id, FrameStatus::Partial),
            ]
        );
        assert!(timeline.following());
    }
    #[test]
    fn playback_loops_over_complete_frames() {
        let mut timeline = Timeline::new(vec![entry(0)]);
        assert_eq!(timeline.advance(), None, "one frame is nothing to loop");
        assert!(!timeline.complete(entry(5)));
        assert!(!timeline.complete(entry(10)));
        timeline.begin(entry(15));
        // From the sweep in progress the loop starts at the oldest frame and
        // skips the partial sweep at the end.
        assert_eq!(timeline.advance(), Some(0));
        assert_eq!(timeline.advance(), Some(1));
        assert_eq!(timeline.advance(), Some(2));
        assert_eq!(timeline.advance(), Some(0));
        // Without a sweep in progress the newest frame is followed as the
        // loop passes it.
        assert!(!timeline.complete(entry(15)));
        assert_eq!(timeline.advance(), Some(1));
        assert_eq!(timeline.advance(), Some(2));
        assert_eq!(timeline.advance(), Some(3));
        assert!(timeline.following());
        assert_eq!(timeline.advance(), Some(0));
        assert!(!timeline.following());
    }

    #[test]
    fn a_reachable_feed_is_judged_by_the_newest_radial_age() {
        use ConnectionStatus::*;
        let stale = STALE_AFTER.as_secs();
        let unavailable = UNAVAILABLE_AFTER.as_secs();
        for from in [Ok, Stale, Unavailable] {
            assert_eq!(feed_condition(from, Some(0)), Ok);
            assert_eq!(feed_condition(from, Some(stale - 1)), Ok);
            assert_eq!(feed_condition(from, Some(stale)), Stale);
            assert_eq!(feed_condition(from, Some(unavailable - 1)), Stale);
            assert_eq!(feed_condition(from, Some(unavailable)), Unavailable);
            // Nothing received yet: nothing to judge.
            assert_eq!(feed_condition(from, None), from);
        }
        // A switch or the poller set these; only a sweep clears them.
        for held in [Loading, Offline] {
            for age in [None, Some(0), Some(stale), Some(unavailable)] {
                assert_eq!(feed_condition(held, age), held);
            }
        }
        assert_eq!(
            serde_json::to_string(&Unavailable).unwrap(),
            "\"unavailable\""
        );
    }

    #[test]
    fn retires_thirty_seconds_after_the_last_reference() {
        let mut retirement = Retirement::default();
        let t0 = Instant::now();
        let both = files(&["tex/a.png", "tex/b.png"]);
        // Both referenced: nothing is tracked.
        assert!(
            retirement
                .sweep(both.clone(), &set(&["tex/a.png", "tex/b.png"]), t0)
                .is_empty()
        );
        assert!(retirement.unreferenced.is_empty());
        // b stops being referenced at t0 + 5 s; it survives until t0 + 35 s.
        let only_a = set(&["tex/a.png"]);
        let t5 = t0 + Duration::from_secs(5);
        assert!(retirement.sweep(both.clone(), &only_a, t5).is_empty());
        assert!(
            retirement
                .sweep(both.clone(), &only_a, t5 + Duration::from_secs(29))
                .is_empty()
        );
        assert_eq!(
            retirement.sweep(both.clone(), &only_a, t5 + RETIRE_AFTER),
            files(&["tex/b.png"])
        );
        assert!(retirement.unreferenced.is_empty());
        // Deleted files stop being present; the current texture is never retired.
        assert!(
            retirement
                .sweep(files(&["tex/a.png"]), &only_a, t5 + Duration::from_secs(90))
                .is_empty()
        );
    }

    #[test]
    fn a_reference_that_returns_resets_the_clock() {
        let mut retirement = Retirement::default();
        let t0 = Instant::now();
        let present = files(&["tex/a.png", "tex/b.png"]);
        assert!(
            retirement
                .sweep(present.clone(), &set(&["tex/a.png"]), t0)
                .is_empty()
        );
        // b is referenced again at 20 s, then dropped again at 25 s.
        let t20 = t0 + Duration::from_secs(20);
        assert!(
            retirement
                .sweep(present.clone(), &set(&["tex/a.png", "tex/b.png"]), t20)
                .is_empty()
        );
        let t25 = t0 + Duration::from_secs(25);
        assert!(
            retirement
                .sweep(present.clone(), &set(&["tex/a.png"]), t25)
                .is_empty()
        );
        // 30 s after the first drop is not enough; 30 s after the second is.
        assert!(
            retirement
                .sweep(present.clone(), &set(&["tex/a.png"]), t0 + RETIRE_AFTER)
                .is_empty()
        );
        assert_eq!(
            retirement.sweep(present, &set(&["tex/a.png"]), t25 + RETIRE_AFTER),
            files(&["tex/b.png"])
        );
    }

    #[test]
    fn leftovers_from_an_earlier_daemon_count_from_first_sight() {
        let mut retirement = Retirement::default();
        let start = Instant::now();
        let present = files(&["tex/old.png", "tex/old.png.tmp", "tex/new.png"]);
        let referenced = set(&["tex/new.png"]);
        assert!(
            retirement
                .sweep(present.clone(), &referenced, start)
                .is_empty()
        );
        assert!(
            retirement
                .sweep(
                    present.clone(),
                    &referenced,
                    start + Duration::from_secs(29)
                )
                .is_empty()
        );
        let mut expired = retirement.sweep(present, &referenced, start + RETIRE_AFTER);
        expired.sort();
        assert_eq!(expired, files(&["tex/old.png", "tex/old.png.tmp"]));
    }
}
#[cfg(test)]
mod handoff_tests {
    use super::{great_circle_km, handoff, site_table};
    use crate::protocol::Station;

    fn station(id: &str, lat: f64, lon: f64) -> Station {
        Station {
            id: id.into(),
            name: id.into(),
            state: String::new(),
            lat,
            lon,
            alt_m: 0.0,
        }
    }
    /// A point `fraction` of the way from `a` to `b` along the parallel.
    fn between(a: &Station, b: &Station, fraction: f64) -> (f64, f64) {
        (
            a.lat + (b.lat - a.lat) * fraction,
            a.lon + (b.lon - a.lon) * fraction,
        )
    }

    #[test]
    fn distances_match_known_values() {
        let d = great_circle_km(35.333361, -97.277761, 36.740617, -98.127717);
        assert!((d - 174.1).abs() < 0.2, "KTLX to KVNX: {d}");
        assert_eq!(great_circle_km(10.0, 20.0, 10.0, 20.0), 0.0);
    }

    #[test]
    fn the_midpoint_between_two_stations_keeps_whichever_is_held() {
        let a = station("AAAA", 35.0, -97.0);
        let b = station("BBBB", 35.0, -95.0);
        let sites = [a.clone(), b.clone()];
        for fraction in [0.45, 0.5, 0.55] {
            let (lat, lon) = between(&a, &b, fraction);
            assert!(
                handoff(&sites, "AAAA", lat, lon).is_none(),
                "{fraction} from A"
            );
            assert!(
                handoff(&sites, "BBBB", lat, lon).is_none(),
                "{fraction} from B"
            );
        }
        // Past the dead band the nearer station takes over, and only it.
        let (lat, lon) = between(&a, &b, 0.6);
        assert_eq!(
            handoff(&sites, "AAAA", lat, lon).map(|s| &s.id[..]),
            Some("BBBB")
        );
        assert!(handoff(&sites, "BBBB", lat, lon).is_none());
        let (lat, lon) = between(&a, &b, 0.4);
        assert_eq!(
            handoff(&sites, "BBBB", lat, lon).map(|s| &s.id[..]),
            Some("AAAA")
        );
        assert!(handoff(&sites, "AAAA", lat, lon).is_none());
    }

    #[test]
    fn co_located_stations_need_a_kilometre_to_swap() {
        // KOUN and KCRI are 300 m apart; near them the ratio alone would flap.
        let sites = site_table().sites;
        let koun = sites.iter().find(|s| s.id == "KOUN").unwrap();
        assert!(handoff(&sites, "KCRI", koun.lat, koun.lon).is_none());
        assert!(handoff(&sites, "KOUN", koun.lat + 0.002, koun.lon).is_none());
        // From Oklahoma City's radar the Norman pair is a real hand-off.
        assert!(handoff(&sites, "KTLX", koun.lat, koun.lon).is_some());
    }

    #[test]
    fn the_fixture_home_view_stays_on_ktlx() {
        let sites = site_table().sites;
        // The window's home view sits 5 km west and 15 km north of the site.
        assert!(handoff(&sites, "KTLX", 35.4681, -97.3326).is_none());
        // A station outside the table (or archived) hands off at once.
        assert_eq!(
            handoff(&sites, "ZZZZ", 35.333361, -97.277761).map(|s| &s.id[..]),
            Some("KTLX")
        );
    }
}
