//! Physical geometry of the Voyager: key positions and the matrix→key map.
//!
//! The embedded JSON is extracted from QMK `keyboards/zsa/voyager/keyboard.json`
//! (`layouts.LAYOUT.layout`). Array order matches the `LAYOUT` macro, which is
//! also the key order Oryx uses inside each layer.

use std::sync::OnceLock;

use serde::Deserialize;

const VOYAGER_JSON: &str = include_str!("../resources/voyager_geometry.json");

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct GeoKey {
    /// `[row, col]` in the keyboard's electrical matrix (what KEYDOWN reports).
    pub m: [u8; 2],
    /// Position in key units (1u = one key width), x grows right, y grows down.
    pub x: f32,
    pub y: f32,
    /// Position of this key in QMK's `LAYOUT(...)` macro - used when patching
    /// `keymap.c`. NOTE: this is NOT the RGB LED index; the LED chain follows
    /// Oryx's visual order, which is simply this array's index (verified
    /// against the ledmap in Oryx-generated firmware).
    #[serde(rename = "led", default)]
    pub layout_pos: u8,
}

pub struct Geometry {
    pub keys: Vec<GeoKey>,
}

impl Geometry {
    /// Index (= Oryx layer key index) for a matrix position, if any.
    pub fn key_index(&self, row: u8, col: u8) -> Option<usize> {
        self.keys.iter().position(|k| k.m == [row, col])
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }
}

/// Parse a `#RRGGBB` string into raw `(r, g, b)` bytes. Lives here (a
/// dependency-free module) so the TUI, the egui widget and the macOS overlay
/// all share one definition instead of three copies.
pub fn parse_hex_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// The Voyager's geometry (52 keys).
pub fn voyager() -> &'static Geometry {
    static GEO: OnceLock<Geometry> = OnceLock::new();
    GEO.get_or_init(|| {
        let keys: Vec<GeoKey> =
            serde_json::from_str(VOYAGER_JSON).expect("embedded voyager geometry is valid JSON");
        Geometry { keys }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voyager_has_52_keys() {
        assert_eq!(voyager().len(), 52);
    }

    #[test]
    fn matrix_lookup_roundtrip() {
        let g = voyager();
        for (i, k) in g.keys.iter().enumerate() {
            assert_eq!(g.key_index(k.m[0], k.m[1]), Some(i));
        }
    }

    #[test]
    fn first_key_is_top_left() {
        let g = voyager();
        assert_eq!(g.keys[0].m, [0, 1]);
        assert_eq!(g.keys[0].x, 0.0);
    }
}
