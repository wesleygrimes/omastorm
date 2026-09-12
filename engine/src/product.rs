//! Engine-owned product vocabulary (`docs/protocol.md`): codes, display
//! names, units, palettes, and class bounds. The UI only lays out what it
//! receives. Reflectivity matches `engine/data/fixture.json`; velocity is
//! a diverging inbound/outbound scale in m/s.

use crate::protocol::Frame;

/// A radar moment the engine can draw as a polar sweep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Product {
    Reflectivity,
    Velocity,
}

impl Product {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Reflectivity => "REF",
            Self::Velocity => "VEL",
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reflectivity => "Reflectivity",
            Self::Velocity => "Velocity",
        }
    }
    pub const fn units(self) -> &'static str {
        match self {
            Self::Reflectivity => "dBZ",
            Self::Velocity => "m/s",
        }
    }
    /// Palette class colors, in class order. Velocity is green inbound
    /// (toward the radar, negative) through white at 0 to red outbound.
    pub fn palette(self) -> Vec<String> {
        self.palette_slice()
            .iter()
            .map(|c| (*c).to_string())
            .collect()
    }
    pub const fn palette_slice(self) -> &'static [&'static str] {
        match self {
            Self::Reflectivity => &[
                "#34465f", "#426b88", "#4098a5", "#51b897", "#85c76b", "#cadb6b", "#f0cd61",
                "#eda24c", "#e67349", "#d84c64", "#b55096", "#e2b4df",
            ],
            Self::Velocity => &[
                "#0d4f0d", "#1e8c1e", "#3dcc3d", "#96f096", "#d4ffd4", "#f4f4f4", "#ffd4d4",
                "#f09696", "#cc3d3d", "#8c1e1e", "#5a0d0d", "#3a0033",
            ],
        }
    }
    /// Class `i` covers `bounds[i]` up to `bounds[i+1]` in `units`.
    pub fn bounds(self) -> Vec<i32> {
        self.bounds_slice().to_vec()
    }
    pub const fn bounds_slice(self) -> &'static [i32] {
        match self {
            Self::Reflectivity => &[-32, 0, 10, 20, 30, 40, 45, 50, 55, 60, 65, 70, 96],
            Self::Velocity => &[-64, -36, -26, -20, -10, -5, 0, 5, 10, 20, 26, 36, 64],
        }
    }
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "REF" => Some(Self::Reflectivity),
            "VEL" => Some(Self::Velocity),
            _ => None,
        }
    }
    /// Frame id suffix: reflectivity keeps the historical `<site>-<time>-e0`
    /// form; velocity appends `-VEL` so the catalog can hold both.
    pub fn id_suffix(self) -> &'static str {
        match self {
            Self::Reflectivity => "",
            Self::Velocity => "-VEL",
        }
    }
    /// Copy this product's vocabulary onto a frame (palette, units, name).
    pub fn apply(self, frame: &mut Frame) {
        frame.product = self.code().into();
        frame.product_name = self.name().into();
        frame.units = self.units().into();
        frame.palette = self.palette();
        frame.bounds = self.bounds();
    }
}

/// HRRR 10 m wind speed classes, m/s. Used for the Cartesian overlay texture.
pub const WIND_UNITS: &str = "m/s";
pub const WIND_PALETTE: [&str; 8] = [
    "#f4f4f4", "#c8e6c8", "#7bc87b", "#e8d44a", "#f0a020", "#e05020", "#b01030", "#6a0860",
];
pub const WIND_BOUNDS: [i32; 9] = [0, 3, 6, 10, 15, 20, 25, 30, 50];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_product_has_one_more_bound_than_palette_colors() {
        for product in [Product::Reflectivity, Product::Velocity] {
            assert_eq!(
                product.bounds_slice().len(),
                product.palette_slice().len() + 1,
                "{}",
                product.code()
            );
        }
        assert_eq!(WIND_BOUNDS.len(), WIND_PALETTE.len() + 1);
    }

    #[test]
    fn parse_accepts_ref_and_vel_only() {
        assert_eq!(Product::parse("REF"), Some(Product::Reflectivity));
        assert_eq!(Product::parse("VEL"), Some(Product::Velocity));
        assert_eq!(Product::parse("SW"), None);
        assert_eq!(Product::parse("ref"), None);
    }
}
