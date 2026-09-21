//! Map and gazetteer clip for compiled live sources.
//!
//! `build.rs` includes this file so 1:10m Natural Earth and GeoNames stay
//! in lockstep with follow. A new live mosaic adds its box here and a
//! `GridRef` variant in `source.rs`. The helpers are used from the build
//! script and tile tests; the binary reads the box constants.
#![allow(dead_code)]

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatLonBox {
    pub north: f64,
    pub south: f64,
    pub east: f64,
    pub west: f64,
}

impl LatLonBox {
    pub fn contains(self, lon: f64, lat: f64) -> bool {
        if lat < self.south || lat > self.north {
            return false;
        }
        if self.west <= self.east {
            lon >= self.west && lon <= self.east
        } else {
            lon >= self.west || lon <= self.east
        }
    }
}

/// EUMETNET OPERA COMP service footprint (ORD docs, approx corners).
pub const OPERA: LatLonBox = LatLonBox {
    north: 70.0,
    south: 32.0,
    east: 50.0,
    west: -30.0,
};

/// Live GridFamily boxes. Polar NEXRAD uses [`nexrad_network`].
pub const LIVE_MOSAICS: &[LatLonBox] = &[OPERA];

/// Coarse NEXRAD network clip (CONUS / AK / HI / Guam), not per-dish circles.
pub fn nexrad_network(lon: f64, lat: f64) -> bool {
    (5.0..=75.0).contains(&lat) && (lon <= -20.0 || lon >= 120.0)
}

pub fn in_live_envelope(lon: f64, lat: f64) -> bool {
    nexrad_network(lon, lat) || LIVE_MOSAICS.iter().any(|b| b.contains(lon, lat))
}
