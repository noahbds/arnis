//! French national datasets from the IGN Géoplateforme (`data.geopf.fr`).
//!
//! Open data under the Licence Ouverte 2.0, served without a key. They
//! complement OpenStreetMap inside France the way Overture does worldwide, and
//! under the same rule: what OSM mappers tagged always wins.
//!
//! Outside the French territories nothing is requested (see [`coverage`]), and
//! any failure degrades to generating exactly as without this module.

mod buildings;
mod coverage;
mod wfs;

pub use buildings::IgnBuildings;
pub use coverage::may_be_covered;

use crate::coordinate_system::geographic::LLBBox;
use crate::progress::emit_gui_progress_update;
use colored::Colorize;

const BUILDINGS_LAYER: &str = "BDTOPO_V3:batiment";

/// What an IGN fetch produces for one generation.
#[derive(Default)]
pub struct IgnData {
    /// BD TOPO buildings, lending heights to OSM buildings that have none.
    pub buildings: IgnBuildings,
}

/// Fetch the IGN data for `bbox`. Returns empty data outside France and on any
/// failure (non-fatal).
pub fn fetch_ign_data(
    bbox: &LLBBox,
    projection: &crate::projection::ProjectionSpec,
    debug: bool,
) -> IgnData {
    if !coverage::may_be_covered(bbox) {
        return IgnData::default();
    }
    match fetch_ign_data_inner(bbox, projection, debug) {
        Ok(data) => data,
        Err(e) => {
            eprintln!(
                "{} Failed to fetch IGN data: {e}",
                "Warning:".yellow().bold()
            );
            IgnData::default()
        }
    }
}

fn fetch_ign_data_inner(
    bbox: &LLBBox,
    projection: &crate::projection::ProjectionSpec,
    debug: bool,
) -> Result<IgnData, Box<dyn std::error::Error>> {
    let client = wfs::client()?;
    emit_gui_progress_update(6.0, "Downloading data...");

    let started = std::time::Instant::now();
    let features = wfs::fetch_features(&client, BUILDINGS_LAYER, bbox)?;
    let (transformer, _) = projection.transformer(bbox)?;
    let buildings = IgnBuildings::from_features(&features, &transformer);
    if debug {
        println!(
            "IGN: {} BD TOPO buildings received, {} with a usable height or storey count, {:.1}s",
            features.len(),
            buildings.len(),
            started.elapsed().as_secs_f64()
        );
    }
    Ok(IgnData { buildings })
}
