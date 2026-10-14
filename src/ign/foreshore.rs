//! What the foreshore is made of, from the BD TOPO `zone_d_estran` layer.
//!
//! Where the sea is placed by altitude, the uncovered foreshore is sand by
//! default. The BD TOPO says where it is shingle or rock instead, in polygons
//! small enough to draw. They become `natural=beach` + `surface=shingle` and
//! `natural=bare_rock` areas, which the generator already knows how to render.
//!
//! Its sand polygons are left out: on the Baie de Somme one of them covers the
//! whole bay, salt marsh included, and would paint over the grass of the marsh.

use crate::clipping::clip_way_to_bbox;
use crate::coordinate_system::cartesian::XZBBox;
use crate::coordinate_system::geographic::LLPoint;
use crate::coordinate_system::transformation::CoordTransformer;
use crate::osm_parser::{ProcessedElement, ProcessedNode, ProcessedWay};
use serde_json::Value;
use std::collections::HashMap;

/// Ids of the ways made here: bit 62 set, bits 61 and 63 clear, so they collide
/// neither with OSM ids, the parser's assembled rings (bit 61) nor Overture (bit 63).
const ID_BIT: u64 = 1 << 62;
const ID_MASK: u64 = (1 << 61) - 1;

/// OSM tags for a BD TOPO foreshore nature, or `None` for natures not drawn.
fn tags_for(nature: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match nature {
        "Graviers ou galets" => Some(&[("natural", "beach"), ("surface", "shingle")]),
        "Rocher" => Some(&[("natural", "bare_rock")]),
        "Vase" => Some(&[("natural", "wetland"), ("wetland", "tidalflat")]),
        _ => None,
    }
}

/// Converts `BDTOPO_V3:zone_d_estran` features into closed ways clipped to `clip`.
/// `rock_kind` names the rock under a (lat, lon) point, for the `rock=*` tag of
/// rock areas.
pub fn elements_from_features(
    features: &[Value],
    transformer: &CoordTransformer,
    clip: &XZBBox,
    rock_kind: &mut dyn FnMut(f64, f64) -> Option<&'static str>,
) -> Vec<ProcessedElement> {
    let mut elements = Vec::new();
    for feature in features {
        let Some(props) = feature.get("properties") else {
            continue;
        };
        let Some(tags) = props
            .get("nature")
            .and_then(Value::as_str)
            .and_then(tags_for)
        else {
            continue;
        };
        let key = props
            .get("cleabs")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(geometry) = feature.get("geometry") else {
            continue;
        };
        for (index, ring) in outer_rings(geometry).into_iter().enumerate() {
            let id = stable_id(key, index);
            let nodes: Vec<ProcessedNode> = ring
                .iter()
                .enumerate()
                .filter_map(|(i, &(lng, lat))| {
                    let xz = transformer.transform_point(LLPoint::new(lat, lng).ok()?);
                    Some(ProcessedNode {
                        id: id.wrapping_add(i as u64 + 1) & ID_MASK | ID_BIT,
                        tags: HashMap::new(),
                        x: xz.x,
                        z: xz.z,
                    })
                })
                .collect();
            let nodes = clip_way_to_bbox(&nodes, clip);
            if nodes.len() < 4 {
                continue;
            }
            let mut tags: HashMap<String, String> = tags
                .iter()
                .map(|&(k, v)| (k.to_string(), v.to_string()))
                .collect();
            if tags.get("natural").map(String::as_str) == Some("bare_rock") {
                let n = ring.len() as f64;
                let lng = ring.iter().map(|p| p.0).sum::<f64>() / n;
                let lat = ring.iter().map(|p| p.1).sum::<f64>() / n;
                if let Some(kind) = rock_kind(lat, lng) {
                    tags.insert("rock".to_string(), kind.to_string());
                }
            }
            elements.push(ProcessedElement::Way(ProcessedWay { id, nodes, tags }));
        }
    }
    elements
}

/// Exterior rings of a GeoJSON Polygon or MultiPolygon, as (lng, lat), closed.
fn outer_rings(geometry: &Value) -> Vec<Vec<(f64, f64)>> {
    let ring = |ring: &Value| -> Option<Vec<(f64, f64)>> {
        let points: Vec<(f64, f64)> = ring
            .as_array()?
            .iter()
            .filter_map(|c| {
                let c = c.as_array()?;
                Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?))
            })
            .collect();
        (points.len() >= 4 && points.first() == points.last()).then_some(points)
    };
    let Some(coordinates) = geometry.get("coordinates").and_then(Value::as_array) else {
        return Vec::new();
    };
    match geometry.get("type").and_then(Value::as_str) {
        Some("Polygon") => coordinates.first().and_then(ring).into_iter().collect(),
        Some("MultiPolygon") => coordinates
            .iter()
            .filter_map(|polygon| polygon.as_array()?.first().and_then(ring))
            .collect(),
        _ => Vec::new(),
    }
}

/// Same object, same id, in every run: FNV-1a of the BD TOPO key and ring index.
fn stable_id(key: &str, index: usize) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes().chain((index as u32).to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    // Leave room below for the node ids derived from it.
    (hash & ID_MASK & !0xFFFF) | ID_BIT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate_system::geographic::LLBBox;

    fn feature(nature: &str) -> Value {
        serde_json::json!({
            "type": "Feature",
            "properties": {"cleabs": "ZONESTRA0000000001", "nature": nature},
            "geometry": {"type": "MultiPolygon", "coordinates": [[[
                [1.4880, 50.1780], [1.4890, 50.1780], [1.4890, 50.1790],
                [1.4880, 50.1790], [1.4880, 50.1780]
            ]]]}
        })
    }

    fn convert(features: &[Value]) -> Vec<ProcessedElement> {
        let bbox = LLBBox::new(50.177, 1.488, 50.186, 1.502).unwrap();
        let (transformer, xzbbox) = CoordTransformer::llbbox_to_xzbbox(&bbox, 1.0).unwrap();
        elements_from_features(features, &transformer, &xzbbox, &mut |_, _| Some("chalk"))
    }

    #[test]
    fn shingle_and_rock_become_drawable_areas() {
        let elements = convert(&[feature("Graviers ou galets"), feature("Rocher")]);
        assert_eq!(elements.len(), 2);
        let tags = |i: usize| elements[i].tags().clone();
        assert_eq!(tags(0).get("natural").map(String::as_str), Some("beach"));
        assert_eq!(tags(0).get("surface").map(String::as_str), Some("shingle"));
        assert_eq!(
            tags(1).get("natural").map(String::as_str),
            Some("bare_rock")
        );
        assert_eq!(tags(1).get("rock").map(String::as_str), Some("chalk"));
        assert_eq!(
            tags(0).get("rock"),
            None,
            "only rock areas are asked for their rock"
        );
        assert!(elements[0].nodes().count() >= 4);
    }

    #[test]
    fn sand_is_left_to_the_land_cover() {
        assert!(convert(&[feature("Sable")]).is_empty());
    }

    #[test]
    fn ids_are_stable_and_clear_of_other_sources() {
        let a = convert(&[feature("Rocher")]);
        let b = convert(&[feature("Rocher")]);
        assert_eq!(a[0].id(), b[0].id());
        assert_eq!(a[0].id() & (1 << 62), 1 << 62);
        assert_eq!(a[0].id() & ((1 << 63) | (1 << 61)), 0);
    }
}
