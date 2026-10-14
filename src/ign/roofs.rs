//! Roof shapes read from the LiDAR HD surface model.
//!
//! The canopy-and-buildings height model (MNH: height above ground, 50 cm) shows
//! every roof as it is. Under one OSM footprint its heights tell a flat roof
//! from a pitched one, which way the ridge runs, whether the ends are hipped,
//! and how tall the roof is. That becomes `roof:shape`, `roof:orientation` and
//! `roof:height`, which the generator already builds from.

use crate::coordinate_system::cartesian::XZBBox;
use crate::coordinate_system::geographic::LLBBox;
use reqwest::blocking::Client;

const WMS_URL: &str = "https://data.geopf.fr/wms-r/wms";
const HEIGHT_LAYER: &str = "IGNF_LIDAR-HD_MNH_ELEVATION.ELEVATIONGRIDCOVERAGE.WGS84G";
/// Largest raster side asked for in one request (the service allows 5010).
const MAX_REQUEST_PX: usize = 2048;
/// Largest area analysed, in blocks: beyond it the download outweighs the gain.
const MAX_CELLS: usize = 6000 * 6000;
/// The service's no-data value is far below any real height.
const NO_DATA_BELOW: f32 = -1000.0;

/// A roof less than this tall is flat; one taller than this is a misreading.
const MIN_PITCH_RISE_M: f64 = 1.0;
const MAX_RISE_M: f64 = 15.0;
/// Fewer interior samples than this say too little about a roof.
const MIN_SAMPLES: usize = 12;
/// The best model must explain at least this share of the height variance.
const MIN_FIT: f64 = 0.4;
/// Ridge positions tried either side of the footprint's centre line, out to
/// [`RIDGE_SPREAD`] of its half-width: a land-registry footprint often holds
/// only part of a roof, so the ridge rarely runs through its middle.
const RIDGE_STEPS: i32 = 6;
const RIDGE_SPREAD: f64 = 0.6;
/// A hipped roof must fit this much better than a gabled one to be called hipped.
const HIP_ADVANTAGE: f64 = 0.85;

/// Height above ground over the world area, one cell per block.
pub struct SurfaceHeights {
    min_x: i32,
    min_z: i32,
    width: usize,
    height: usize,
    values: Vec<f32>,
}

impl SurfaceHeights {
    /// Downloads the height model over `bbox`, sampled once per block of
    /// `xzbbox`. Only valid where blocks map linearly to lat/lon (the local
    /// projection). `None` when the area is too large or the service fails.
    pub fn fetch(client: &Client, bbox: &LLBBox, xzbbox: &XZBBox) -> Option<Self> {
        let width = (xzbbox.max_x() - xzbbox.min_x() + 1).max(1) as usize;
        let height = (xzbbox.max_z() - xzbbox.min_z() + 1).max(1) as usize;
        if width * height > MAX_CELLS {
            return None;
        }
        let (lat0, lon0) = (bbox.min().lat(), bbox.min().lng());
        let (lat1, lon1) = (bbox.max().lat(), bbox.max().lng());
        let mut values = vec![f32::NAN; width * height];
        for tile_z in (0..height).step_by(MAX_REQUEST_PX) {
            for tile_x in (0..width).step_by(MAX_REQUEST_PX) {
                let tw = MAX_REQUEST_PX.min(width - tile_x);
                let th = MAX_REQUEST_PX.min(height - tile_z);
                // Row 0 of the world is the north edge.
                let west = lon0 + (lon1 - lon0) * tile_x as f64 / width as f64;
                let east = lon0 + (lon1 - lon0) * (tile_x + tw) as f64 / width as f64;
                let north = lat1 - (lat1 - lat0) * tile_z as f64 / height as f64;
                let south = lat1 - (lat1 - lat0) * (tile_z + th) as f64 / height as f64;
                let tile = fetch_tile(client, (south, west, north, east), tw, th)?;
                for row in 0..th {
                    let src = &tile[row * tw..(row + 1) * tw];
                    let dst = (tile_z + row) * width + tile_x;
                    values[dst..dst + tw].copy_from_slice(src);
                }
            }
        }
        // An area the survey does not cover yet comes back as no data throughout.
        let valid = values.iter().filter(|v| v.is_finite()).count();
        (valid * 2 > values.len()).then_some(Self {
            min_x: xzbbox.min_x(),
            min_z: xzbbox.min_z(),
            width,
            height,
            values,
        })
    }

    fn at(&self, x: f64, z: f64) -> Option<f64> {
        let col = (x.floor() as i64) - i64::from(self.min_x);
        let row = (z.floor() as i64) - i64::from(self.min_z);
        if col < 0 || row < 0 || col >= self.width as i64 || row >= self.height as i64 {
            return None;
        }
        let v = self.values[row as usize * self.width + col as usize];
        v.is_finite().then_some(f64::from(v))
    }

    #[cfg(test)]
    pub fn from_fn(width: usize, height: usize, f: impl Fn(usize, usize) -> f32) -> Self {
        let mut values = Vec::with_capacity(width * height);
        for z in 0..height {
            for x in 0..width {
                values.push(f(x, z));
            }
        }
        Self {
            min_x: 0,
            min_z: 0,
            width,
            height,
            values,
        }
    }
}

fn fetch_tile(
    client: &Client,
    (south, west, north, east): (f64, f64, f64, f64),
    width: usize,
    height: usize,
) -> Option<Vec<f32>> {
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
            ("LAYERS", HEIGHT_LAYER),
            ("FORMAT", "image/x-bil;bits=32"),
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
    if bytes.len() != width * height * 4 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|b| {
                let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                if v > NO_DATA_BELOW {
                    v
                } else {
                    f32::NAN
                }
            })
            .collect(),
    )
}

/// A roof as OSM tags describe it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Roof {
    pub shape: &'static str,
    /// `along` or `across` the footprint's longer side, for a gabled roof.
    pub orientation: Option<&'static str>,
    /// Compass bearing the slope faces, for a single-pitch roof.
    pub direction: Option<u16>,
    /// Gutter to ridge, metres; 0 for a flat roof.
    pub rise_m: f64,
}

/// Reads the roof over the given interior points of a footprint.
pub fn classify(points: &[(f64, f64)], heights: &SurfaceHeights) -> Option<Roof> {
    let samples: Vec<(f64, f64, f64)> = points
        .iter()
        .filter_map(|&(x, z)| Some((x, z, heights.at(x, z)?)))
        .collect();
    if samples.len() < MIN_SAMPLES {
        return None;
    }

    let mut zs: Vec<f64> = samples.iter().map(|s| s.2).collect();
    zs.sort_by(f64::total_cmp);
    let pct = |q: f64| zs[((zs.len() - 1) as f64 * q).round() as usize];
    let (eave, ridge) = (pct(0.1), pct(0.95));
    let rise = ridge - eave;
    if rise < MIN_PITCH_RISE_M {
        return Some(Roof {
            shape: "flat",
            orientation: None,
            direction: None,
            rise_m: 0.0,
        });
    }
    if rise > MAX_RISE_M {
        return None;
    }

    // The footprint's own frame: u along its longer axis, v across, from the centre.
    let n = samples.len() as f64;
    let (cx, cz) = (
        samples.iter().map(|s| s.0).sum::<f64>() / n,
        samples.iter().map(|s| s.1).sum::<f64>() / n,
    );
    let (mut sxx, mut szz, mut sxz) = (0.0, 0.0, 0.0);
    for &(x, z, _) in &samples {
        sxx += (x - cx) * (x - cx);
        szz += (z - cz) * (z - cz);
        sxz += (x - cx) * (z - cz);
    }
    let angle = 0.5 * (2.0 * sxz).atan2(sxx - szz);
    let (cos, sin) = (angle.cos(), angle.sin());
    let frame: Vec<(f64, f64, f64)> = samples
        .iter()
        .map(|&(x, z, h)| {
            let (dx, dz) = (x - cx, z - cz);
            (dx * cos + dz * sin, -dx * sin + dz * cos, h)
        })
        .collect();
    let half_length = frame.iter().map(|p| p.0.abs()).fold(0.0, f64::max);
    let half_width = frame.iter().map(|p| p.1.abs()).fold(0.0, f64::max);
    let hip_offset = (half_length - half_width).max(0.0);

    // Each candidate is h = a + b * f(u, v), fitted by least squares; the share of
    // the variance it explains decides between them. Returns (share, b).
    let fit = |f: &dyn Fn(f64, f64) -> f64| -> (f64, f64) {
        let (mut sf, mut sh, mut sff, mut sfh) = (0.0, 0.0, 0.0, 0.0);
        for &(u, v, h) in &frame {
            let x = f(u, v);
            sf += x;
            sh += h;
            sff += x * x;
            sfh += x * h;
        }
        let denom = n * sff - sf * sf;
        if denom.abs() < 1e-9 {
            return (0.0, 0.0);
        }
        let slope = (n * sfh - sf * sh) / denom;
        let intercept = (sh - slope * sf) / n;
        let mean = sh / n;
        let (mut ss_res, mut ss_tot) = (0.0, 0.0);
        for &(u, v, h) in &frame {
            let e = h - (intercept + slope * f(u, v));
            ss_res += e * e;
            ss_tot += (h - mean) * (h - mean);
        }
        let share = if ss_tot > 0.0 {
            1.0 - ss_res / ss_tot
        } else {
            0.0
        };
        (share, slope)
    };

    // Two slopes meeting at a ridge, the ridge along u ("along") or along v
    // ("across"), wherever it fits best. A roof falls away from its ridge, so
    // only a negative slope counts.
    let mut gable: (f64, &'static str) = (0.0, "along");
    for (orientation, half) in [("along", half_width), ("across", half_length)] {
        for k in -RIDGE_STEPS..=RIDGE_STEPS {
            let offset = half * RIDGE_SPREAD * f64::from(k) / f64::from(RIDGE_STEPS);
            let (share, slope) = if orientation == "along" {
                fit(&|_, v| (v - offset).abs())
            } else {
                fit(&|u, _| (u - offset).abs())
            };
            if slope < 0.0 && share > gable.0 {
                gable = (share, orientation);
            }
        }
    }
    // One slope across the whole footprint, either way.
    let mut skillion: (f64, f64, f64) = (0.0, 0.0, 0.0);
    for (du, dv) in [(0.0, 1.0), (1.0, 0.0)] {
        let (share, slope) = fit(&|u, v| u * du + v * dv);
        if share > skillion.0 {
            // Facing downhill: against the direction the roof rises.
            skillion = (share, -slope * du, -slope * dv);
        }
    }
    let (hip_share, hip_slope) = fit(&|u, v| v.abs().max(u.abs() - hip_offset));

    if hip_slope < 0.0 && hip_share >= MIN_FIT && hip_share * HIP_ADVANTAGE > gable.0 {
        return Some(Roof {
            shape: "hipped",
            orientation: None,
            direction: None,
            rise_m: rise,
        });
    }
    if gable.0 >= MIN_FIT && gable.0 >= skillion.0 {
        return Some(Roof {
            shape: "gabled",
            orientation: Some(gable.1),
            direction: None,
            rise_m: rise,
        });
    }
    if skillion.0 >= MIN_FIT {
        // Back from the footprint frame to the world: x east, z south.
        let (fu, fv) = (skillion.1, skillion.2);
        let (dx, dz) = (fu * cos - fv * sin, fu * sin + fv * cos);
        let bearing = dx.atan2(-dz).to_degrees().rem_euclid(360.0);
        return Some(Roof {
            shape: "skillion",
            orientation: None,
            direction: Some(bearing.round() as u16 % 360),
            rise_m: rise,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Interior points of the rectangle [x0, x1) x [z0, z1), one per block.
    fn points(x0: i32, z0: i32, x1: i32, z1: i32) -> Vec<(f64, f64)> {
        (x0..x1)
            .flat_map(|x| (z0..z1).map(move |z| (x as f64 + 0.5, z as f64 + 0.5)))
            .collect()
    }

    #[test]
    fn a_flat_roof_is_flat() {
        let heights = SurfaceHeights::from_fn(40, 40, |_, _| 9.0);
        let roof = classify(&points(5, 5, 25, 15), &heights).unwrap();
        assert_eq!(roof.shape, "flat");
    }

    #[test]
    fn a_ridge_along_the_long_side_is_a_gable_along() {
        // 20 x 10 house, ridge along x at z = 10, falling 0.6 m per block.
        let heights = SurfaceHeights::from_fn(40, 40, |_, z| {
            (9.0 - 0.6 * (z as f32 + 0.5 - 10.0).abs()) as f32
        });
        let roof = classify(&points(5, 5, 25, 15), &heights).unwrap();
        assert_eq!(roof.shape, "gabled");
        assert_eq!(roof.orientation, Some("along"));
        assert!((2.0..4.0).contains(&roof.rise_m), "rise {}", roof.rise_m);
    }

    #[test]
    fn a_ridge_across_the_long_side_is_a_gable_across() {
        let heights = SurfaceHeights::from_fn(40, 40, |x, _| {
            (12.0 - 0.5 * (x as f32 + 0.5 - 15.0).abs()) as f32
        });
        let roof = classify(&points(5, 5, 25, 15), &heights).unwrap();
        assert_eq!(roof.shape, "gabled");
        assert_eq!(roof.orientation, Some("across"));
    }

    #[test]
    fn sloping_ends_make_a_hipped_roof() {
        // Same house, but the roof also falls toward both short ends.
        let heights = SurfaceHeights::from_fn(40, 40, |x, z| {
            let v = (z as f32 + 0.5 - 10.0).abs();
            let u = (x as f32 + 0.5 - 15.0).abs() - 5.0;
            9.0 - 0.6 * v.max(u)
        });
        let roof = classify(&points(5, 5, 25, 15), &heights).unwrap();
        assert_eq!(roof.shape, "hipped");
    }

    #[test]
    fn an_off_centre_ridge_is_still_a_gable() {
        // Ridge at z = 12 in a footprint spanning z 5..15: the back of a row house.
        let heights = SurfaceHeights::from_fn(40, 40, |_, z| {
            (9.0 - 0.6 * (z as f32 + 0.5 - 12.0).abs()) as f32
        });
        let roof = classify(&points(5, 5, 25, 15), &heights).unwrap();
        assert_eq!(roof.shape, "gabled");
        assert_eq!(roof.orientation, Some("along"));
    }

    #[test]
    fn a_single_slope_faces_downhill() {
        // Rising toward the south (z grows), so the slope faces north.
        let heights = SurfaceHeights::from_fn(40, 40, |_, z| 4.0 + 0.3 * z as f32);
        let roof = classify(&points(5, 5, 25, 15), &heights).unwrap();
        assert_eq!(roof.shape, "skillion");
        let bearing = roof.direction.unwrap();
        assert!(bearing <= 10 || bearing >= 350, "bearing {bearing}");

        // Rising toward the east, so facing west.
        let heights = SurfaceHeights::from_fn(40, 40, |x, _| 4.0 + 0.3 * x as f32);
        let roof = classify(&points(5, 5, 25, 15), &heights).unwrap();
        assert_eq!(roof.shape, "skillion");
        let bearing = roof.direction.unwrap();
        assert!((260..=280).contains(&bearing), "bearing {bearing}");
    }

    #[test]
    fn noise_without_a_shape_is_not_guessed() {
        // A tall but shapeless surface (vegetation, clutter) gives no roof.
        let heights =
            SurfaceHeights::from_fn(
                40,
                40,
                |x, z| {
                    if (x * 7 + z * 13) % 5 == 0 {
                        12.0
                    } else {
                        6.0
                    }
                },
            );
        assert_eq!(classify(&points(5, 5, 25, 15), &heights), None);
    }

    #[test]
    fn too_few_points_say_nothing() {
        let heights = SurfaceHeights::from_fn(40, 40, |_, _| 9.0);
        assert_eq!(classify(&points(5, 5, 8, 8), &heights), None);
    }
}
