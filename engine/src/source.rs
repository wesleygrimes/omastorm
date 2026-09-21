//! Compiled source registry. Enum dispatch over every adapter this build
//! compiled (`docs/grid-adapters.md`). A new live mosaic is a `GridRef`
//! variant, a field on [`SourceRegistry`], and a box in `envelope.rs`.

use crate::{
    live,
    protocol::{
        AdapterTarget, Coverage, Family, GeoPoint, Kind, MosaicFrame, ProductClass, Selection,
        SourceInfo, Station,
    },
};
use std::collections::HashSet;
use tokio::{sync::mpsc::Sender, task::JoinHandle};

/// Nominal NEXRAD reflectivity footprint (`docs/grid-adapters.md`).
pub const NEXRAD_RADIUS_KM: f64 = 460.0;
/// Following hands off when another covering polar site is closer than this
/// fraction of the held site's distance, and by at least [`HANDOFF_MARGIN_KM`].
pub const HANDOFF_RATIO: f64 = 0.8;
pub const HANDOFF_MARGIN_KM: f64 = 1.0;

/// Borrowed adapter metadata for hello and selection. Coverage is stored on
/// the adapter and not cloned on every settled centre.
pub struct SourceMetadataBorrowed<'a> {
    pub id: &'a str,
    pub family: Family,
    pub kind: Kind,
    pub default_product_class: ProductClass,
    pub name: &'a str,
    pub attribution: &'a str,
    pub mosaic: Option<MosaicMeta<'a>>,
}

pub struct MosaicMeta<'a> {
    pub coverage: &'a Coverage,
    pub selection_priority: i32,
    /// Follow and gazetteer use this. `select_source` still works when false
    /// (the synthetic fixture).
    pub covering: bool,
}

/// Live-loop events from any GridFamily poller. `source_id` is the adapter
/// that spawned the task; late events after a switch are dropped.
pub enum GridEvent {
    Frame {
        source_id: String,
        frame: Box<MosaicFrame>,
        texture: Vec<u8>,
        start_ms: i64,
    },
    Backfill {
        source_id: String,
        frame: Box<MosaicFrame>,
        texture: Vec<u8>,
        start_ms: i64,
    },
    Offline {
        source_id: String,
        reason: String,
    },
    Silent {
        source_id: String,
        reason: String,
    },
}

/// What `select_source` needs from one grid adapter, copied off the borrow
/// so the live loop can mutate shared state.
pub enum MosaicStart {
    Static {
        frames: Vec<(MosaicFrame, Vec<u8>, i64)>,
    },
    Live {
        placeholder: Option<Box<(MosaicFrame, Vec<u8>)>>,
    },
}

pub enum MosaicStartError {
    Unknown,
    Polar,
}

/// Every adapter this build compiled. Enum dispatch, no boxed futures.
pub struct SourceRegistry {
    pub nexrad: Nexrad,
    pub opera: crate::opera::Opera,
    pub fixture: crate::grid_fixture::FixtureMosaic,
}

impl SourceRegistry {
    pub fn compiled() -> Self {
        Self {
            nexrad: Nexrad::new(),
            opera: crate::opera::Opera::new(),
            fixture: crate::grid_fixture::FixtureMosaic::new(),
        }
    }

    pub fn adapters(&self) -> [AdapterRef<'_>; 3] {
        [
            AdapterRef::Nexrad(&self.nexrad),
            AdapterRef::Grid(GridRef::Opera(&self.opera)),
            AdapterRef::Grid(GridRef::FixtureMosaic(&self.fixture)),
        ]
    }

    pub fn hello_sources(&self) -> Vec<SourceInfo> {
        self.adapters()
            .into_iter()
            .map(|a| a.source_info())
            .collect()
    }

    pub fn hello_sites(&self) -> Vec<Station> {
        self.nexrad.hello_sites()
    }

    pub fn get(&self, id: &str) -> Option<AdapterRef<'_>> {
        self.adapters().into_iter().find(|a| a.id() == id)
    }

    /// Covering-source selection (`docs/grid-adapters.md`, rules 1–5).
    pub fn covering_selection(
        &self,
        center: GeoPoint,
        held: Option<&Selection>,
    ) -> Option<Selection> {
        covering_selection(center, held, &self.candidates())
    }

    pub fn candidates(&self) -> Vec<Candidate> {
        self.adapters()
            .into_iter()
            .flat_map(|a| a.covering_candidates())
            .collect()
    }

    pub fn mosaic_start(&self, id: &str) -> Result<MosaicStart, MosaicStartError> {
        match self.get(id) {
            None => Err(MosaicStartError::Unknown),
            Some(AdapterRef::Nexrad(_)) => Err(MosaicStartError::Polar),
            Some(AdapterRef::Grid(grid)) => Ok(grid.start()),
        }
    }

    pub fn poll_mosaic(
        &self,
        id: &str,
        events: Sender<GridEvent>,
        known: HashSet<String>,
    ) -> Option<JoinHandle<()>> {
        match self.get(id)? {
            AdapterRef::Grid(grid) => grid.poll(events, known),
            AdapterRef::Nexrad(_) => None,
        }
    }

    pub fn history_max(&self, id: &str) -> Option<usize> {
        match self.get(id)? {
            AdapterRef::Grid(grid) => grid.history_max(),
            AdapterRef::Nexrad(_) => None,
        }
    }

    pub fn polls(&self, id: &str) -> bool {
        matches!(self.get(id), Some(AdapterRef::Grid(grid)) if grid.polls())
    }
}

pub enum AdapterRef<'a> {
    Nexrad(&'a Nexrad),
    Grid(GridRef<'a>),
}

/// One compiled GridFamily adapter. Adding a live mosaic is a new variant,
/// a `SourceRegistry` field, and its box in `envelope.rs`.
pub enum GridRef<'a> {
    Opera(&'a crate::opera::Opera),
    FixtureMosaic(&'a crate::grid_fixture::FixtureMosaic),
}

impl<'a> AdapterRef<'a> {
    pub fn id(&self) -> &str {
        match self {
            Self::Nexrad(a) => a.id,
            Self::Grid(grid) => grid.id(),
        }
    }

    pub fn source_info(&self) -> SourceInfo {
        match self {
            Self::Nexrad(a) => SourceInfo {
                id: a.id.to_owned(),
                family: Family::Polar,
                kind: Kind::Site,
                default_product_class: ProductClass::Reflectivity,
                name: a.name.to_owned(),
                attribution: a.attribution.to_owned(),
                selection_priority: None,
                coverage: None,
            },
            Self::Grid(grid) => grid_source_info(grid.metadata()),
        }
    }

    fn covering_candidates(&self) -> Vec<Candidate> {
        match self {
            Self::Nexrad(a) => a.site_candidates(),
            Self::Grid(grid) => grid.covering_candidate().into_iter().collect(),
        }
    }
}

impl<'a> GridRef<'a> {
    fn id(&self) -> &str {
        match self {
            Self::Opera(a) => a.id,
            Self::FixtureMosaic(a) => a.id,
        }
    }

    fn metadata(&self) -> SourceMetadataBorrowed<'a> {
        match self {
            Self::Opera(a) => a.metadata(),
            Self::FixtureMosaic(a) => a.metadata(),
        }
    }

    fn covering_candidate(&self) -> Option<Candidate> {
        let meta = self.metadata();
        let mosaic = meta.mosaic.filter(|m| m.covering)?;
        Some(Candidate {
            selection: Selection {
                source_id: meta.id.to_owned(),
                target: AdapterTarget::Mosaic,
            },
            family: Family::Grid,
            product: meta.default_product_class,
            priority: mosaic.selection_priority,
            coverage: mosaic.coverage.clone(),
            dish: None,
        })
    }

    fn start(&self) -> MosaicStart {
        match self {
            Self::Opera(a) => MosaicStart::Live {
                placeholder: a.loading_placeholder().map(Box::new),
            },
            Self::FixtureMosaic(a) => MosaicStart::Static { frames: a.frames() },
        }
    }

    fn history_max(&self) -> Option<usize> {
        match self {
            Self::Opera(_) => Some(crate::opera::HISTORY_MAX),
            Self::FixtureMosaic(_) => None,
        }
    }

    fn polls(&self) -> bool {
        matches!(self, Self::Opera(_))
    }

    fn poll(&self, events: Sender<GridEvent>, known: HashSet<String>) -> Option<JoinHandle<()>> {
        match self {
            Self::Opera(a) => a.poll(&AdapterTarget::Mosaic, events, known),
            Self::FixtureMosaic(_) => None,
        }
    }
}

fn grid_source_info(meta: SourceMetadataBorrowed<'_>) -> SourceInfo {
    let (coverage, priority) = match meta.mosaic {
        Some(m) => (Some(m.coverage.clone()), Some(m.selection_priority)),
        None => (None, None),
    };
    SourceInfo {
        id: meta.id.to_owned(),
        family: meta.family,
        kind: meta.kind,
        default_product_class: meta.default_product_class,
        name: meta.name.to_owned(),
        attribution: meta.attribution.to_owned(),
        selection_priority: priority,
        coverage,
    }
}

/// PolarFamily NEXRAD: today's chunk join, one feed with many sites.
pub struct Nexrad {
    pub id: &'static str,
    pub name: &'static str,
    pub attribution: &'static str,
    pub sites: Vec<Station>,
}

impl Nexrad {
    pub fn new() -> Self {
        let table: crate::protocol::SiteTable =
            serde_json::from_str(include_str!("../data/sites.json")).unwrap();
        let sites = table
            .sites
            .into_iter()
            .map(|mut s| {
                s.source_id = "nexrad".into();
                s.coverage = Coverage::Circle {
                    lat: None,
                    lon: None,
                    radius_km: NEXRAD_RADIUS_KM,
                };
                s
            })
            .collect();
        Self {
            id: "nexrad",
            name: "NOAA NEXRAD",
            attribution: "NOAA NEXRAD",
            sites,
        }
    }

    fn site_candidates(&self) -> Vec<Candidate> {
        self.sites
            .iter()
            .map(|site| Candidate {
                selection: Selection {
                    source_id: self.id.to_owned(),
                    target: AdapterTarget::Site {
                        site_id: site.id.clone(),
                    },
                },
                family: Family::Polar,
                product: ProductClass::Reflectivity,
                priority: 0,
                coverage: site.coverage.clone(),
                dish: Some(GeoPoint {
                    lat: site.lat,
                    lon: site.lon,
                }),
            })
            .collect()
    }

    pub fn hello_sites(&self) -> Vec<Station> {
        self.sites.clone()
    }

    pub fn station(&self, id: &str) -> Option<&Station> {
        self.sites.iter().find(|s| s.id == id)
    }

    /// Start today's live poller on a polar site target.
    pub fn poll(
        &self,
        target: &AdapterTarget,
        events: Sender<live::Event>,
        cached: Vec<i64>,
        skip_known: bool,
    ) -> Option<JoinHandle<()>> {
        match target {
            AdapterTarget::Site { site_id } if self.station(site_id).is_some() => Some(
                tokio::spawn(live::poll(site_id.clone(), events, cached, skip_known)),
            ),
            _ => None,
        }
    }
}

/// One selectable coverage: a polar dish or a grid mosaic.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub selection: Selection,
    pub family: Family,
    pub product: ProductClass,
    pub priority: i32,
    pub coverage: Coverage,
    pub dish: Option<GeoPoint>,
}

impl Candidate {
    fn contains(&self, center: GeoPoint) -> bool {
        contains(&self.coverage, center, self.dish)
    }
}

/// Great-circle distance in kilometres on the radar's 6371 km sphere.
pub fn great_circle_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.clamp(0.0, 1.0).sqrt().asin()
}

/// Whether `center` lies in `coverage`. Circle tests use `dish` when the wire
/// coverage omitted lat/lon (hello sites).
pub fn contains(coverage: &Coverage, center: GeoPoint, dish: Option<GeoPoint>) -> bool {
    match coverage {
        Coverage::Circle {
            lat,
            lon,
            radius_km,
        } => {
            let (clat, clon) = match (lat.zip(*lon), dish) {
                (Some((lat, lon)), _) => (lat, lon),
                (None, Some(p)) => (p.lat, p.lon),
                (None, None) => return false,
            };
            great_circle_km(center.lat, center.lon, clat, clon) <= *radius_km
        }
        Coverage::Box {
            north,
            south,
            east,
            west,
        } => {
            if center.lat < *south || center.lat > *north {
                return false;
            }
            if west <= east {
                center.lon >= *west && center.lon <= *east
            } else {
                center.lon >= *west || center.lon <= *east
            }
        }
        Coverage::Polygon { vertices } => point_in_polygon(center, vertices),
    }
}

fn point_in_polygon(point: GeoPoint, vertices: &[GeoPoint]) -> bool {
    if vertices.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = vertices.len() - 1;
    for i in 0..vertices.len() {
        let a = vertices[i];
        let b = vertices[j];
        let intersect = ((a.lat > point.lat) != (b.lat > point.lat))
            && (point.lon < (b.lon - a.lon) * (point.lat - a.lat) / (b.lat - a.lat) + a.lon);
        if intersect {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Inverse of GDAL's pixel-corner affine: map x/y → column/row.
pub fn inverse_affine(geotransform: [f64; 6], x: f64, y: f64) -> Option<(f64, f64)> {
    let [x0, dx, rx, y0, ry, dy] = geotransform;
    let det = dx * dy - rx * ry;
    if det.abs() < 1e-18 {
        return None;
    }
    let dx_ = x - x0;
    let dy_ = y - y0;
    let col = (dy * dx_ - rx * dy_) / det;
    let row = (-ry * dx_ + dx * dy_) / det;
    Some((col, row))
}

fn same_selection(a: &Selection, b: &Selection) -> bool {
    a == b
}

/// Rules 1–5 from `docs/grid-adapters.md` Source selection.
pub fn covering_selection(
    center: GeoPoint,
    held: Option<&Selection>,
    candidates: &[Candidate],
) -> Option<Selection> {
    let covering: Vec<&Candidate> = candidates.iter().filter(|c| c.contains(center)).collect();
    let polar: Vec<&Candidate> = covering
        .iter()
        .copied()
        .filter(|c| c.family == Family::Polar)
        .collect();
    let grids: Vec<&Candidate> = covering
        .iter()
        .copied()
        .filter(|c| c.family == Family::Grid)
        .collect();

    let polar_distance = |c: &Candidate| {
        c.dish
            .map(|p| great_circle_km(center.lat, center.lon, p.lat, p.lon))
            .unwrap_or(f64::INFINITY)
    };
    let nearest_polar = polar
        .iter()
        .copied()
        .min_by(|a, b| polar_distance(a).total_cmp(&polar_distance(b)));

    let held_polar = held.and_then(|h| {
        candidates
            .iter()
            .find(|c| c.family == Family::Polar && same_selection(&c.selection, h))
    });

    // 1–2. Polar hysteresis while the held dish covers; leave coverage
    // bypasses it and takes the nearest covering polar immediately.
    if let Some(held_site) = held_polar {
        if held_site.contains(center) {
            if let Some(nearest) = nearest_polar {
                if same_selection(&nearest.selection, &held_site.selection) {
                    return Some(held_site.selection.clone());
                }
                let nearest_d = polar_distance(nearest);
                let held_d = polar_distance(held_site);
                if nearest_d >= HANDOFF_RATIO * held_d || held_d - nearest_d < HANDOFF_MARGIN_KM {
                    return Some(held_site.selection.clone());
                }
                return Some(nearest.selection.clone());
            }
            return Some(held_site.selection.clone());
        }
        if let Some(nearest) = nearest_polar {
            return Some(nearest.selection.clone());
        }
    } else if let Some(nearest) = nearest_polar {
        // 3. Any covering polar wins; no held polar, take nearest.
        return Some(nearest.selection.clone());
    }

    if grids.is_empty() {
        return None;
    }

    // Highest product class, then highest priority, then adapter id (ascending).
    let best = grids.iter().copied().min_by(|a, b| {
        b.product
            .cmp(&a.product)
            .then(b.priority.cmp(&a.priority))
            .then(a.selection.source_id.cmp(&b.selection.source_id))
    })?;

    let held_grid = held.and_then(|h| {
        grids
            .iter()
            .copied()
            .find(|c| same_selection(&c.selection, h))
    });
    if let Some(held_grid) = held_grid {
        // 4. Equal-ranked grids keep the held one; better class or priority preempts.
        if held_grid.product == best.product && held_grid.priority == best.priority {
            return Some(held_grid.selection.clone());
        }
    }
    Some(best.selection.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Crs;

    fn dish(id: &str, lat: f64, lon: f64) -> Candidate {
        Candidate {
            selection: Selection {
                source_id: "nexrad".into(),
                target: AdapterTarget::Site { site_id: id.into() },
            },
            family: Family::Polar,
            product: ProductClass::Reflectivity,
            priority: 0,
            coverage: Coverage::Circle {
                lat: Some(lat),
                lon: Some(lon),
                radius_km: NEXRAD_RADIUS_KM,
            },
            dish: Some(GeoPoint { lat, lon }),
        }
    }

    fn grid(id: &str, product: ProductClass, priority: i32, coverage: Coverage) -> Candidate {
        Candidate {
            selection: Selection {
                source_id: id.into(),
                target: AdapterTarget::Mosaic,
            },
            family: Family::Grid,
            product,
            priority,
            coverage,
            dish: None,
        }
    }

    fn box_cov(north: f64, south: f64, east: f64, west: f64) -> Coverage {
        Coverage::Box {
            north,
            south,
            east,
            west,
        }
    }

    fn sel_site(id: &str) -> Selection {
        Selection {
            source_id: "nexrad".into(),
            target: AdapterTarget::Site { site_id: id.into() },
        }
    }

    fn between(a: GeoPoint, b: GeoPoint, fraction: f64) -> GeoPoint {
        GeoPoint {
            lat: a.lat + (b.lat - a.lat) * fraction,
            lon: a.lon + (b.lon - a.lon) * fraction,
        }
    }

    #[test]
    fn circle_box_and_polygon_containment() {
        let circle = Coverage::Circle {
            lat: Some(35.0),
            lon: Some(-97.0),
            radius_km: 100.0,
        };
        let origin = GeoPoint {
            lat: 35.0,
            lon: -97.0,
        };
        assert!(contains(&circle, origin, None));
        assert!(contains(
            &circle,
            GeoPoint {
                lat: 35.5,
                lon: -97.0
            },
            None
        ));
        assert!(!contains(
            &circle,
            GeoPoint {
                lat: 40.0,
                lon: -97.0
            },
            None
        ));
        let boxc = box_cov(1.0, 0.0, 1.0, 0.0);
        assert!(contains(&boxc, GeoPoint { lat: 0.5, lon: 0.5 }, None));
        assert!(contains(&boxc, GeoPoint { lat: 0.0, lon: 0.0 }, None));
        assert!(!contains(
            &boxc,
            GeoPoint {
                lat: 0.5,
                lon: -0.1
            },
            None
        ));
        let poly = Coverage::Polygon {
            vertices: vec![
                GeoPoint { lat: 0.0, lon: 0.0 },
                GeoPoint { lat: 0.0, lon: 1.0 },
                GeoPoint { lat: 1.0, lon: 1.0 },
                GeoPoint { lat: 1.0, lon: 0.0 },
            ],
        };
        assert!(contains(&poly, GeoPoint { lat: 0.5, lon: 0.5 }, None));
        assert!(!contains(&poly, GeoPoint { lat: 0.5, lon: 1.5 }, None));
    }

    #[test]
    fn polar_hysteresis_matches_today_while_both_cover() {
        let a = dish("AAAA", 35.0, -97.0);
        let b = dish("BBBB", 35.0, -95.0);
        let sites = [a.clone(), b.clone()];
        let pa = a.dish.unwrap();
        let pb = b.dish.unwrap();
        for fraction in [0.45, 0.5, 0.55] {
            let c = between(pa, pb, fraction);
            assert_eq!(
                covering_selection(c, Some(&sel_site("AAAA")), &sites),
                Some(sel_site("AAAA")),
                "{fraction} from A"
            );
            assert_eq!(
                covering_selection(c, Some(&sel_site("BBBB")), &sites),
                Some(sel_site("BBBB")),
                "{fraction} from B"
            );
        }
        let c = between(pa, pb, 0.6);
        assert_eq!(
            covering_selection(c, Some(&sel_site("AAAA")), &sites),
            Some(sel_site("BBBB"))
        );
        assert_eq!(
            covering_selection(c, Some(&sel_site("BBBB")), &sites),
            Some(sel_site("BBBB"))
        );
        let c = between(pa, pb, 0.4);
        assert_eq!(
            covering_selection(c, Some(&sel_site("BBBB")), &sites),
            Some(sel_site("AAAA"))
        );
    }

    #[test]
    fn leaving_polar_coverage_bypasses_hysteresis() {
        let held = dish("AAAA", 35.0, -97.0);
        let other = dish("BBBB", 42.0, -97.0);
        // ~777 km north of AAAA: outside 460 km of AAAA, on BBBB.
        let center = GeoPoint {
            lat: 42.0,
            lon: -97.0,
        };
        assert!(!held.contains(center));
        assert!(other.contains(center));
        assert_eq!(
            covering_selection(center, Some(&sel_site("AAAA")), &[held, other]),
            Some(sel_site("BBBB"))
        );
    }

    #[test]
    fn polar_beats_a_covering_grid() {
        let polar = dish("KTLX", 35.333, -97.278);
        let mosaic = grid(
            "opera",
            ProductClass::Reflectivity,
            100,
            box_cov(50.0, 20.0, -80.0, -110.0),
        );
        let center = GeoPoint {
            lat: 35.4,
            lon: -97.5,
        };
        assert_eq!(
            covering_selection(center, None, &[polar.clone(), mosaic.clone()]),
            Some(sel_site("KTLX"))
        );
        let held_grid = Selection {
            source_id: "opera".into(),
            target: AdapterTarget::Mosaic,
        };
        assert_eq!(
            covering_selection(center, Some(&held_grid), &[polar, mosaic]),
            Some(sel_site("KTLX"))
        );
    }

    #[test]
    fn grid_prefers_reflectivity_then_priority() {
        let rate = grid(
            "rate",
            ProductClass::PrecipitationRate,
            200,
            box_cov(1.0, 0.0, 1.0, 0.0),
        );
        let refl_low = grid(
            "refl-low",
            ProductClass::Reflectivity,
            1,
            box_cov(1.0, 0.0, 1.0, 0.0),
        );
        let refl_high = grid(
            "refl-high",
            ProductClass::Reflectivity,
            50,
            box_cov(1.0, 0.0, 1.0, 0.0),
        );
        let center = GeoPoint { lat: 0.5, lon: 0.5 };
        assert_eq!(
            covering_selection(center, None, &[rate.clone(), refl_low.clone()])
                .map(|s| s.source_id),
            Some("refl-low".into())
        );
        assert_eq!(
            covering_selection(center, None, &[refl_low.clone(), refl_high.clone()])
                .map(|s| s.source_id),
            Some("refl-high".into())
        );
        let held_low = Selection {
            source_id: "refl-low".into(),
            target: AdapterTarget::Mosaic,
        };
        assert_eq!(
            covering_selection(center, Some(&held_low), &[refl_low, refl_high])
                .map(|s| s.source_id),
            Some("refl-high".into()),
            "higher priority preempts"
        );
    }

    #[test]
    fn equal_rank_grids_keep_the_held_one() {
        let a = grid(
            "alpha",
            ProductClass::Reflectivity,
            10,
            box_cov(1.0, 0.0, 1.0, 0.0),
        );
        let b = grid(
            "beta",
            ProductClass::Reflectivity,
            10,
            box_cov(1.0, 0.0, 1.0, 0.0),
        );
        let center = GeoPoint { lat: 0.5, lon: 0.5 };
        let held_b = Selection {
            source_id: "beta".into(),
            target: AdapterTarget::Mosaic,
        };
        assert_eq!(
            covering_selection(center, Some(&held_b), &[a.clone(), b.clone()]).map(|s| s.source_id),
            Some("beta".into())
        );
        assert_eq!(
            covering_selection(center, None, &[a, b]).map(|s| s.source_id),
            Some("alpha".into()),
            "no held: adapter id ascending"
        );
    }

    #[test]
    fn no_covering_source_is_null() {
        let polar = dish("KTLX", 35.333, -97.278);
        let mosaic = grid(
            "fixture-mosaic",
            ProductClass::Reflectivity,
            100,
            box_cov(1.0, 0.0, 1.0, 0.0),
        );
        let center = GeoPoint {
            lat: -10.0,
            lon: 20.0,
        };
        assert_eq!(covering_selection(center, None, &[polar, mosaic]), None);
    }

    #[test]
    fn opera_is_a_live_registry_entry() {
        let registry = SourceRegistry::compiled();
        assert!(matches!(
            registry.mosaic_start("opera"),
            Ok(MosaicStart::Live { .. })
        ));
        assert!(registry.polls("opera"));
        assert!(!registry.polls("fixture-mosaic"));
        assert!(matches!(
            registry.mosaic_start("fixture-mosaic"),
            Ok(MosaicStart::Static { .. })
        ));
        assert!(matches!(
            registry.mosaic_start("nexrad"),
            Err(MosaicStartError::Polar)
        ));
        assert!(matches!(
            registry.mosaic_start("no-such"),
            Err(MosaicStartError::Unknown)
        ));
    }

    #[test]
    fn fixture_box_is_far_from_nexrad() {
        let registry = SourceRegistry::compiled();
        let over_mosaic = GeoPoint { lat: 0.5, lon: 0.5 };
        // The fixture is select_source-only; follow never picks it.
        assert_eq!(registry.covering_selection(over_mosaic, None), None);
        let over_ktlx = GeoPoint {
            lat: 35.333,
            lon: -97.278,
        };
        let picked = registry.covering_selection(over_ktlx, None).unwrap();
        assert_eq!(picked.source_id, "nexrad");
        assert_eq!(
            picked.target,
            AdapterTarget::Site {
                site_id: "KTLX".into()
            }
        );
        let nowhere = GeoPoint {
            lat: -40.0,
            lon: 20.0,
        };
        assert_eq!(registry.covering_selection(nowhere, None), None);
        // The window's home view sits 5 km west and 15 km north of KTLX.
        let home = GeoPoint {
            lat: 35.4681,
            lon: -97.3326,
        };
        assert_eq!(
            registry
                .covering_selection(home, Some(&sel_site("KTLX")))
                .map(|s| match s.target {
                    AdapterTarget::Site { site_id } => site_id,
                    AdapterTarget::Mosaic => String::new(),
                })
                .as_deref(),
            Some("KTLX")
        );
        let koun = registry.nexrad.station("KOUN").unwrap();
        assert_eq!(
            registry
                .covering_selection(
                    GeoPoint {
                        lat: koun.lat,
                        lon: koun.lon
                    },
                    Some(&sel_site("KCRI"))
                )
                .map(|s| match s.target {
                    AdapterTarget::Site { site_id } => site_id,
                    AdapterTarget::Mosaic => String::new(),
                })
                .as_deref(),
            Some("KCRI")
        );
        assert_eq!(
            registry
                .covering_selection(
                    GeoPoint {
                        lat: koun.lat,
                        lon: koun.lon
                    },
                    Some(&sel_site("KTLX"))
                )
                .map(|s| match s.target {
                    AdapterTarget::Site { site_id } => site_id,
                    AdapterTarget::Mosaic => String::new(),
                })
                .as_deref(),
            Some("KOUN")
        );
    }

    #[test]
    fn geographic_inverse_affine_matches_the_spec_example() {
        let gt = [-1.0, 0.01, 0.0, 1.0, 0.0, -0.01];
        assert_eq!(inverse_affine(gt, -1.0, 1.0), Some((0.0, 0.0)));
        let (c, r) = inverse_affine(gt, -1.0 + 0.01, 1.0).unwrap();
        assert!((c - 1.0).abs() < 1e-12 && r.abs() < 1e-12);
        let (c, r) = inverse_affine(gt, -1.0, 1.0 - 0.01).unwrap();
        assert!(c.abs() < 1e-12 && (r - 1.0).abs() < 1e-12);
        let (col, row) = inverse_affine(gt, -1.0 + 0.005, 1.0 - 0.005).unwrap();
        assert!((col - 0.5).abs() < 1e-12 && (row - 0.5).abs() < 1e-12);
        assert_eq!(
            Crs::wgs84_geographic().ellipsoid().semi_major_m,
            6_378_137.0
        );
    }

    #[test]
    fn distances_match_known_values() {
        let d = great_circle_km(35.333361, -97.277761, 36.740617, -98.127717);
        assert!((d - 174.1).abs() < 0.2, "KTLX to KVNX: {d}");
        assert_eq!(great_circle_km(10.0, 20.0, 10.0, 20.0), 0.0);
    }
}
