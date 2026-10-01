//! Settled view geometry; fetch bounds are never observation coverage.
use crate::protocol::GeoPoint;
use serde::{Deserialize, Serialize};

pub const MERCATOR_LIMIT: f64 = 85.05112878;
pub const RADIUS: f64 = 6_378_137.;

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Bounds {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}
impl Bounds {
    pub fn validate(self) -> Result<Self, String> {
        if [self.west, self.south, self.east, self.north]
            .iter()
            .any(|v| !v.is_finite())
            || self.west < -180.
            || self.east > 180.
            || self.west >= self.east
            || self.south < -MERCATOR_LIMIT
            || self.north > MERCATOR_LIMIT
            || self.south >= self.north
        {
            return Err("grid bounds need finite, ordered nonwrapping Mercator coordinates".into());
        }
        Ok(self)
    }
    pub fn union(self, other: Self) -> Self {
        Self {
            west: self.west.min(other.west),
            south: self.south.min(other.south),
            east: self.east.max(other.east),
            north: self.north.max(other.north),
        }
    }
    pub fn projected(self) -> [f64; 4] {
        let (west, south) = mercator(GeoPoint {
            lat: self.south,
            lon: self.west,
        });
        let (east, north) = mercator(GeoPoint {
            lat: self.north,
            lon: self.east,
        });
        [west, south, east, north]
    }
}
pub fn mercator(point: GeoPoint) -> (f64, f64) {
    (
        RADIUS * point.lon.to_radians(),
        RADIUS
            * (std::f64::consts::FRAC_PI_4
                + point
                    .lat
                    .clamp(-MERCATOR_LIMIT, MERCATOR_LIMIT)
                    .to_radians()
                    / 2.)
                .tan()
                .ln(),
    )
}
pub fn latitude(y: f64) -> f64 {
    (2. * (y / RADIUS).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub level: u32,
    pub x: i64,
    pub y: i64,
}
impl Region {
    pub fn choose(center: GeoPoint, bounds: Option<Bounds>, held: Option<Self>) -> Self {
        let (cx, cy) = mercator(center);
        let view = bounds.map(Bounds::projected).unwrap_or([cx, cy, cx, cy]);
        if let Some(region) = held.filter(|r| bounds.is_some() || r.level == 1) {
            let [w, s, e, n] = region.bbox();
            let inset = 128. * region.pixel_size();
            if view[0] >= w + inset
                && view[1] >= s + inset
                && view[2] <= e - inset
                && view[3] <= n - inset
            {
                return region;
            }
        }
        let mut level = if bounds.is_none() { 1 } else { 0 };
        while (view[2] - view[0]).max(view[3] - view[1]) > 512. * (1024. * 2f64.powi(level as i32))
        {
            level += 1;
        }
        let lattice = 256. * 1024. * 2f64.powi(level as i32);
        Self {
            level,
            x: (((view[0] + view[2]) / 2.) / lattice).round() as i64,
            y: (((view[1] + view[3]) / 2.) / lattice).round() as i64,
        }
    }
    pub fn pixel_size(self) -> f64 {
        1024. * 2f64.powi(self.level as i32)
    }
    pub fn bbox(self) -> [f64; 4] {
        let d = self.pixel_size();
        let x = self.x as f64 * 256. * d;
        let y = self.y as f64 * 256. * d;
        [x - 512. * d, y - 512. * d, x + 512. * d, y + 512. * d]
    }
    pub fn affine(self) -> [f64; 6] {
        let [w, _, _, n] = self.bbox();
        [w, self.pixel_size(), 0., n, 0., -self.pixel_size()]
    }
    pub fn key(self) -> String {
        format!(
            "eccc-RATE-dis14-3857-l{}-x{}-y{}",
            self.level, self.x, self.y
        )
    }
    pub fn margin(self) -> usize {
        let [_, s, _, n] = self.bbox();
        let lat = latitude(s).abs().max(latitude(n).abs());
        (1. + (1000. / (self.pixel_size() * lat.to_radians().cos())).ceil()).min(1024.) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapped_windows_pad_hold_and_switch_without_stitching() {
        let center = GeoPoint {
            lat: 50.,
            lon: -90.,
        };
        let bounds = Bounds {
            west: -94.,
            south: 48.,
            east: -86.,
            north: 52.,
        };
        let r = Region::choose(center, Some(bounds), None);
        assert_eq!(r.level, 1);
        assert_eq!(Region::choose(center, Some(bounds), Some(r)), r);
        let [w, s, e, n] = r.bbox();
        let v = bounds.projected();
        for gap in [v[0] - w, v[1] - s, e - v[2], n - v[3]] {
            assert!(gap >= 128. * r.pixel_size());
        }
        let moved = Bounds {
            west: -88.,
            east: -80.,
            ..bounds
        };
        assert_ne!(Region::choose(center, Some(moved), Some(r)), r);
        assert_eq!(Region::choose(center, None, None).level, 1);
    }
    #[test]
    fn bounds_reject_wrapping_poles_and_nan() {
        let b = Bounds {
            west: -90.,
            east: -80.,
            south: 45.,
            north: 55.,
        };
        assert!(b.validate().is_ok());
        for bad in [
            Bounds { west: 100., ..b },
            Bounds { north: 90., ..b },
            Bounds {
                south: f64::NAN,
                ..b
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }
}
