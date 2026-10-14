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
mod foreshore;
mod geology;
mod ortho;
mod roofs;
mod wfs;

pub use buildings::IgnBuildings;
pub use coverage::may_be_covered;

use crate::coordinate_system::geographic::LLBBox;
use crate::progress::emit_gui_progress_update;
use colored::Colorize;

const BUILDINGS_LAYER: &str = "BDTOPO_V3:batiment";
const FORESHORE_LAYER: &str = "BDTOPO_V3:zone_d_estran";
/// Rock areas whose lithology is looked up per generation.
const MAX_ROCK_LOOKUPS: usize = 20;

/// What an IGN fetch produces for one generation.
#[derive(Default)]
pub struct IgnData {
    /// BD TOPO buildings, lending heights to OSM buildings that have none.
    pub buildings: IgnBuildings,
    /// Areas to draw alongside the OSM ones (shingle and rock foreshore).
    pub elements: Vec<crate::osm_parser::ProcessedElement>,
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
    let (transformer, xzbbox) = projection.transformer(bbox)?;
    // Roof shapes need blocks that map linearly to lat/lon, as in the local projection.
    let surface = (projection.kind == crate::projection::ProjectionKind::Local)
        .then(|| roofs::SurfaceHeights::fetch(&client, bbox, &xzbbox))
        .flatten();
    let colours = (projection.kind == crate::projection::ProjectionKind::Local)
        .then(|| ortho::RoofColours::fetch(&client, bbox, &xzbbox))
        .flatten();
    let buildings = IgnBuildings::from_features(&features, &transformer)
        .with_surface(surface)
        .with_colours(colours);

    // The foreshore is a bonus on top of the heights: its failure costs only itself.
    let elements = match wfs::fetch_features(&client, FORESHORE_LAYER, bbox) {
        Ok(features) => {
            // A handful of rock areas at most; capped so a rocky coastline
            // cannot turn into hundreds of requests to the geological service.
            let mut asked = 0;
            let mut rock_kind = |lat: f64, lon: f64| {
                asked += 1;
                (asked <= MAX_ROCK_LOOKUPS)
                    .then(|| geology::rock_kind_at(&client, lat, lon))
                    .flatten()
            };
            foreshore::elements_from_features(
                &features,
                &transformer,
                &projection.clip_bbox(&xzbbox),
                &mut rock_kind,
            )
        }
        Err(e) => {
            eprintln!(
                "{} Failed to fetch the IGN foreshore: {e}",
                "Warning:".yellow().bold()
            );
            Vec::new()
        }
    };
    if debug {
        println!(
            "IGN: {} BD TOPO buildings received, {} with a usable height or storey count, {:.1}s",
            features.len(),
            buildings.len(),
            started.elapsed().as_secs_f64()
        );
        println!(
            "IGN: {} foreshore areas (shingle, rock, mud)",
            elements.len()
        );
        println!(
            "IGN: aerial photograph {}",
            if buildings.has_colours() {
                "loaded"
            } else {
                "unavailable"
            }
        );
        println!(
            "IGN: LiDAR roof heights {}",
            if buildings.has_surface() {
                "loaded"
            } else {
                "unavailable"
            }
        );
    }
    Ok(IgnData {
        buildings,
        elements,
    })
}
