//! Roof colours from the IGN aerial photographs (BD ORTHO).
//!
//! Seen from above, a roof is the colour of its tiles, slates or sheeting. The
//! median colour under a footprint, once shadows, glare and overhanging trees
//! are left out, becomes `roof:colour`, which the generator maps to the nearest
//! roof block. The idea is the one "Minecraft à la carte" (IGN, 2016) used:
//! a vote over the pixels of each roof, not one pixel.

use crate::coordinate_system::cartesian::XZBBox;
use crate::coordinate_system::geographic::LLBBox;
use reqwest::blocking::Client;

const WMS_URL: &str = "https://data.geopf.fr/wms-r/wms";
const PHOTO_LAYER: &str = "ORTHOIMAGERY.ORTHOPHOTOS";
const MAX_REQUEST_PX: usize = 2048;
const MAX_CELLS: usize = 6000 * 6000;
/// Pixels darker than this are shadow, brighter are glare off glass or metal.
const MIN_LUMA: f64 = 40.0;
const MAX_LUMA: f64 = 235.0;
/// Green this far above red and blue is a tree crown over the roof.
const VEGETATION_MARGIN: i32 = 12;
/// Fewer usable pixels than this and the roof keeps its generated colour.
const MIN_PIXELS: usize = 8;

/// The photograph over the world area, one pixel per block.
pub struct RoofColours {
    min_x: i32,
    min_z: i32,
    width: usize,
    height: usize,
    rgb: Vec<[u8; 3]>,
}

impl RoofColours {
    /// Downloads the photograph over `bbox`, one pixel per block of `xzbbox`
    /// (local projection only). `None` when the area is too large or a request fails.
    pub fn fetch(client: &Client, bbox: &LLBBox, xzbbox: &XZBBox) -> Option<Self> {
        let width = (xzbbox.max_x() - xzbbox.min_x() + 1).max(1) as usize;
        let height = (xzbbox.max_z() - xzbbox.min_z() + 1).max(1) as usize;
        if width * height > MAX_CELLS {
            return None;
        }
        let (lat0, lon0) = (bbox.min().lat(), bbox.min().lng());
        let (lat1, lon1) = (bbox.max().lat(), bbox.max().lng());
        let mut rgb = vec![[0u8; 3]; width * height];
        for tile_z in (0..height).step_by(MAX_REQUEST_PX) {
            for tile_x in (0..width).step_by(MAX_REQUEST_PX) {
                let tw = MAX_REQUEST_PX.min(width - tile_x);
                let th = MAX_REQUEST_PX.min(height - tile_z);
                let west = lon0 + (lon1 - lon0) * tile_x as f64 / width as f64;
                let east = lon0 + (lon1 - lon0) * (tile_x + tw) as f64 / width as f64;
                let north = lat1 - (lat1 - lat0) * tile_z as f64 / height as f64;
                let south = lat1 - (lat1 - lat0) * (tile_z + th) as f64 / height as f64;
                let tile = fetch_tile(client, (south, west, north, east), tw, th)?;
                for row in 0..th {
                    for col in 0..tw {
                        let p = tile.get_pixel(col as u32, row as u32);
                        rgb[(tile_z + row) * width + tile_x + col] = [p[0], p[1], p[2]];
                    }
                }
            }
        }
        Some(Self {
            min_x: xzbbox.min_x(),
            min_z: xzbbox.min_z(),
            width,
            height,
            rgb,
        })
    }

    fn at(&self, x: f64, z: f64) -> Option<[u8; 3]> {
        let col = (x.floor() as i64) - i64::from(self.min_x);
        let row = (z.floor() as i64) - i64::from(self.min_z);
        if col < 0 || row < 0 || col >= self.width as i64 || row >= self.height as i64 {
            return None;
        }
        Some(self.rgb[row as usize * self.width + col as usize])
    }

    #[cfg(test)]
    pub fn from_fn(width: usize, height: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Self {
        let mut rgb = Vec::with_capacity(width * height);
        for z in 0..height {
            for x in 0..width {
                rgb.push(f(x, z));
            }
        }
        Self {
            min_x: 0,
            min_z: 0,
            width,
            height,
            rgb,
        }
    }
}

fn fetch_tile(
    client: &Client,
    (south, west, north, east): (f64, f64, f64, f64),
    width: usize,
    height: usize,
) -> Option<image::RgbImage> {
    let bbox = format!("{south},{west},{north},{east}");
    let (w, h) = (width.to_string(), height.to_string());
    let bytes = client
        .get(WMS_URL)
        .query(&[
            ("SERVICE", "WMS"),
            ("VERSION", "1.3.0"),
            ("REQUEST", "GetMap"),
            ("STYLES", ""),
            ("CRS", "EPSG:4326"),
            ("LAYERS", PHOTO_LAYER),
            ("FORMAT", "image/jpeg"),
            ("WIDTH", w.as_str()),
            ("HEIGHT", h.as_str()),
            ("BBOX", bbox.as_str()),
        ])
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .bytes()
        .ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgb8();
    (image.width() as usize == width && image.height() as usize == height).then_some(image)
}

/// The colour of the roof over these interior points, as `#rrggbb`.
pub fn roof_colour(points: &[(f64, f64)], colours: &RoofColours) -> Option<String> {
    let mut usable: Vec<[u8; 3]> = points
        .iter()
        .filter_map(|&(x, z)| colours.at(x, z))
        .filter(|&[r, g, b]| {
            let luma = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
            let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
            (MIN_LUMA..=MAX_LUMA).contains(&luma)
                && !(g > r + VEGETATION_MARGIN && g > b + VEGETATION_MARGIN)
        })
        .collect();
    if usable.len() < MIN_PIXELS {
        return None;
    }
    // Median per channel: robust to dormers, skylights and chimneys.
    let mut channel = |i: usize| -> u8 {
        usable.sort_by_key(|p| p[i]);
        usable[usable.len() / 2][i]
    };
    let (r, g, b) = (channel(0), channel(1), channel(2));
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points() -> Vec<(f64, f64)> {
        (5..15)
            .flat_map(|x| (5..15).map(move |z| (x as f64 + 0.5, z as f64 + 0.5)))
            .collect()
    }

    #[test]
    fn a_tiled_roof_reads_as_its_tiles() {
        let colours = RoofColours::from_fn(20, 20, |_, _| [168, 84, 60]);
        assert_eq!(roof_colour(&points(), &colours).as_deref(), Some("#a8543c"));
    }

    #[test]
    fn shadow_glare_and_trees_are_left_out() {
        // A slate roof with a shadowed half-row, a glinting skylight and a tree.
        let colours = RoofColours::from_fn(20, 20, |x, z| match (x, z) {
            (5, _) => [10, 10, 12],
            (6, 6) => [250, 250, 250],
            (12..=14, 12..=14) => [60, 120, 50],
            _ => [92, 96, 104],
        });
        assert_eq!(roof_colour(&points(), &colours).as_deref(), Some("#5c6068"));
    }

    #[test]
    fn a_roof_lost_in_shadow_keeps_its_generated_colour() {
        let colours = RoofColours::from_fn(20, 20, |_, _| [15, 15, 15]);
        assert_eq!(roof_colour(&points(), &colours), None);
    }
}
