//! Rock type from the BRGM geological map (`geoservices.brgm.fr`).
//!
//! The simplified 1:1,000,000 lithology is the only layer served as vectors; it
//! is coarse, but enough to tell the chalk of the Picardy and Normandy cliffs
//! from the granite of Brittany. Asked once per rock area, never per block.

use reqwest::blocking::Client;

const BRGM_WFS_URL: &str = "https://geoservices.brgm.fr/geologie";
const LITHOLOGY_LAYER: &str = "ms:LITHO_1M_SIMPLIFIEE";
/// Half-sizes of the probe boxes tried around a point, in degrees (about 10 m,
/// 200 m and 1 km). The map stops at the coastline, so a wave-cut platform
/// below a cliff is only reached by the wider ones.
const PROBE_DEG: [f64; 3] = [0.0001, 0.002, 0.01];

/// The `rock=*` value for the lithology under a point, if it is one the
/// generator draws differently.
pub fn rock_kind_at(client: &Client, lat: f64, lon: f64) -> Option<&'static str> {
    // The first box that finds any mapped rock decides, even one with no special look.
    let description = PROBE_DEG
        .iter()
        .find_map(|&half| description_near(client, lat, lon, half))?;
    rock_kind_for(&description)
}

fn description_near(client: &Client, lat: f64, lon: f64, half: f64) -> Option<String> {
    let bbox = format!(
        "{},{},{},{},urn:ogc:def:crs:EPSG::4326",
        lat - half,
        lon - half,
        lat + half,
        lon + half
    );
    let body = client
        .get(BRGM_WFS_URL)
        .query(&[
            ("SERVICE", "WFS"),
            ("VERSION", "1.1.0"),
            ("REQUEST", "GetFeature"),
            ("TYPENAME", LITHOLOGY_LAYER),
            ("MAXFEATURES", "1"),
            ("BBOX", bbox.as_str()),
        ])
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .ok()?;
    description(&body)
}

/// The `DESCR` field of the first feature in a GML response.
fn description(gml: &str) -> Option<String> {
    let start = gml.find("<ms:DESCR>")? + "<ms:DESCR>".len();
    let end = start + gml[start..].find("</ms:DESCR>")?;
    Some(gml[start..end].trim().to_string())
}

fn rock_kind_for(description: &str) -> Option<&'static str> {
    let d = description.to_lowercase();
    if d.contains("craie") {
        Some("chalk")
    } else if d.contains("calcaire") {
        Some("limestone")
    } else if d.contains("granit") {
        Some("granite")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lithology_is_read_from_the_gml() {
        let gml = "<wfs:FeatureCollection><gml:featureMember><ms:LITHO_1M_SIMPLIFIEE>\
                   <ms:TYPE>Roches Sédimentaires</ms:TYPE><ms:DESCR>Craie</ms:DESCR>\
                   </ms:LITHO_1M_SIMPLIFIEE></gml:featureMember></wfs:FeatureCollection>";
        assert_eq!(description(gml).as_deref(), Some("Craie"));
        assert_eq!(description("<wfs:FeatureCollection/>"), None);
    }

    #[test]
    fn lithologies_map_to_rock_kinds() {
        assert_eq!(rock_kind_for("Craie"), Some("chalk"));
        assert_eq!(rock_kind_for("Calcaires et marnes"), Some("limestone"));
        assert_eq!(rock_kind_for("Granitoïdes"), Some("granite"));
        assert_eq!(rock_kind_for("Sables et argiles"), None);
    }
}
