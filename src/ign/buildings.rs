//! Building heights from the BD TOPO, the French national topographic database.
//!
//! OpenStreetMap in France has nearly every footprint (imported from the land
//! registry) but almost never a height, so the generator has to guess one. The
//! BD TOPO measures it. There is no identifier shared with OSM, and the two
//! databases do not cut blocks into buildings the same way, so an OSM building
//! is matched to whatever BD TOPO objects cover its footprint and takes their
//! height weighted by how much of it each one covers.
//!
//! Strictly additive, like the Overture hints: an element that already carries
//! `height` or `building:levels` is left alone.

use crate::coordinate_system::geographic::LLPoint;
use crate::coordinate_system::transformation::CoordTransformer;
use crate::osm_parser::{ProcessedElement, ProcessedMemberRole, ProcessedNode};
use serde_json::Value;
use std::collections::HashMap;

/// Share of an OSM footprint that BD TOPO objects must cover before their
/// height is trusted for it.
const MIN_COVERAGE: f64 = 0.5;
/// Sample points aimed for per footprint; coverage is measured on these.
const TARGET_SAMPLES: f64 = 120.0;
/// Plausibility window for a measured eave height, in metres.
const MIN_HEIGHT_M: f64 = 2.0;
const MAX_HEIGHT_M: f64 = 500.0;
/// A roof lower than this is flat for our purposes; a taller one is a data error.
const MIN_ROOF_RISE_M: f64 = 0.5;
const MAX_ROOF_RISE_M: f64 = 40.0;
/// Storey heights outside this range mean the storey count and the height
/// disagree; the count comes from tax records and is the less reliable of the two.
const MIN_STOREY_M: f64 = 2.2;
const MAX_STOREY_M: f64 = 5.0;
const MAX_LEVELS: i64 = 60;
/// Spatial index cell, in blocks.
const CELL: f64 = 64.0;

type Point = (f64, f64);

#[derive(Debug, Clone)]
struct Polygon {
    exterior: Vec<Point>,
    holes: Vec<Vec<Point>>,
}

impl Polygon {
    fn contains(&self, p: Point) -> bool {
        point_in_ring(p, &self.exterior) && !self.holes.iter().any(|h| point_in_ring(p, h))
    }
}

/// One BD TOPO building, in world block coordinates.
#[derive(Debug, Clone)]
pub struct TopoBuilding {
    polygons: Vec<Polygon>,
    /// `(min_x, min_z, max_x, max_z)`
    bounds: (f64, f64, f64, f64),
    /// Ground to gutter: `hauteur` is the eave height, not the ridge.
    eave_height_m: Option<f64>,
    /// Gutter to ridge, where both roof altitudes are known.
    roof_rise_m: Option<f64>,
    /// Storeys including the ground floor.
    levels: Option<i64>,
}

impl TopoBuilding {
    fn new(
        polygons: Vec<Polygon>,
        eave_height_m: Option<f64>,
        roof_rise_m: Option<f64>,
        levels: Option<i64>,
    ) -> Option<Self> {
        let mut bounds = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, z) in polygons.iter().flat_map(|p| p.exterior.iter()) {
            bounds = (
                bounds.0.min(x),
                bounds.1.min(z),
                bounds.2.max(x),
                bounds.3.max(z),
            );
        }
        if polygons.is_empty() || bounds.0 > bounds.2 {
            return None;
        }
        Some(Self {
            polygons,
            bounds,
            eave_height_m,
            roof_rise_m,
            levels,
        })
    }

    /// Parses a GeoJSON feature of `BDTOPO_V3:batiment`. `None` for a feature
    /// with no usable geometry or nothing to contribute.
    fn from_feature(feature: &Value, transformer: &CoordTransformer) -> Option<Self> {
        let props = feature.get("properties")?;
        // Buildings under construction or in ruins have no reliable height.
        if let Some(state) = props.get("etat_de_l_objet").and_then(Value::as_str) {
            if state != "En service" {
                return None;
            }
        }
        let number = |key: &str| props.get(key).and_then(Value::as_f64);

        let eave_height_m = number("hauteur").filter(|h| (MIN_HEIGHT_M..=MAX_HEIGHT_M).contains(h));
        let roof_rise_m = match (
            number("altitude_minimale_toit"),
            number("altitude_maximale_toit"),
        ) {
            (Some(low), Some(high)) => {
                Some(high - low).filter(|rise| (MIN_ROOF_RISE_M..=MAX_ROOF_RISE_M).contains(rise))
            }
            _ => None,
        };
        let levels = props
            .get("nombre_d_etages")
            .and_then(Value::as_i64)
            .filter(|l| (1..=MAX_LEVELS).contains(l));
        if eave_height_m.is_none() && levels.is_none() {
            return None;
        }

        let geometry = feature.get("geometry")?;
        let coordinates = geometry.get("coordinates")?.as_array()?;
        let ring = |ring: &Value| -> Option<Vec<Point>> {
            let mut points: Vec<Point> = ring
                .as_array()?
                .iter()
                .filter_map(|c| {
                    let c = c.as_array()?;
                    let point = LLPoint::new(c.get(1)?.as_f64()?, c.first()?.as_f64()?).ok()?;
                    let xz = transformer.transform_point(point);
                    Some((f64::from(xz.x), f64::from(xz.z)))
                })
                .collect();
            if points.len() > 1 && points.first() == points.last() {
                points.pop();
            }
            (points.len() >= 3).then_some(points)
        };
        let polygon = |rings: &Value| -> Option<Polygon> {
            let rings = rings.as_array()?;
            Some(Polygon {
                exterior: ring(rings.first()?)?,
                holes: rings.iter().skip(1).filter_map(ring).collect(),
            })
        };
        let polygons: Vec<Polygon> = match geometry.get("type")?.as_str()? {
            "Polygon" => polygon(&Value::Array(coordinates.clone()))
                .into_iter()
                .collect(),
            "MultiPolygon" => coordinates.iter().filter_map(polygon).collect(),
            _ => return None,
        };
        Self::new(polygons, eave_height_m, roof_rise_m, levels)
    }

    fn contains(&self, p: Point) -> bool {
        let (min_x, min_z, max_x, max_z) = self.bounds;
        p.0 >= min_x
            && p.0 <= max_x
            && p.1 >= min_z
            && p.1 <= max_z
            && self.polygons.iter().any(|poly| poly.contains(p))
    }
}

/// What the BD TOPO knows about the ground under one OSM footprint.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Hint {
    eave_height_m: Option<f64>,
    roof_rise_m: Option<f64>,
    levels: Option<i64>,
}

/// The BD TOPO buildings of an area, ready to lend their heights to OSM buildings.
#[derive(Default)]
pub struct IgnBuildings {
    buildings: Vec<TopoBuilding>,
}

impl IgnBuildings {
    pub fn from_features(features: &[Value], transformer: &CoordTransformer) -> Self {
        Self {
            buildings: features
                .iter()
                .filter_map(|f| TopoBuilding::from_feature(f, transformer))
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.buildings.len()
    }

    /// Fill `height` / `building:levels` on OSM buildings that carry neither.
    /// Returns the number of elements enriched.
    pub fn apply(&self, elements: &mut [ProcessedElement]) -> usize {
        if self.buildings.is_empty() {
            return 0;
        }
        let index = self.index();

        let mut enriched = 0;
        for element in elements.iter_mut() {
            let tags = element.tags();
            if !tags.contains_key("building") && !tags.contains_key("building:part") {
                continue;
            }
            // Any existing vertical tag means OSM already knows better.
            if tags.contains_key("height") || tags.contains_key("building:levels") {
                continue;
            }
            let Some(footprint) = Footprint::of(element) else {
                continue;
            };
            let Some(hint) = self.hint_for(&footprint, &index) else {
                continue;
            };

            let tags: &mut HashMap<String, String> = match element {
                ProcessedElement::Way(way) => &mut way.tags,
                ProcessedElement::Relation(relation) => &mut relation.tags,
                ProcessedElement::Node(_) => continue,
            };
            if write_tags(tags, hint) {
                enriched += 1;
            }
        }
        enriched
    }

    /// Grid cell -> indices of the buildings whose bounds touch it.
    fn index(&self) -> HashMap<(i32, i32), Vec<u32>> {
        let mut index: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, building) in self.buildings.iter().enumerate() {
            let (min_x, min_z, max_x, max_z) = building.bounds;
            for cx in cell(min_x)..=cell(max_x) {
                for cz in cell(min_z)..=cell(max_z) {
                    index.entry((cx, cz)).or_default().push(i as u32);
                }
            }
        }
        index
    }

    fn hint_for(
        &self,
        footprint: &Footprint,
        index: &HashMap<(i32, i32), Vec<u32>>,
    ) -> Option<Hint> {
        let samples = footprint.samples();
        if samples.is_empty() {
            return None;
        }

        // How many sample points each candidate covers. A point under two
        // overlapping objects counts for the first, so the shares add up.
        let mut covered: HashMap<u32, u32> = HashMap::new();
        for &p in &samples {
            let Some(candidates) = index.get(&(cell(p.0), cell(p.1))) else {
                continue;
            };
            if let Some(&hit) = candidates
                .iter()
                .find(|&&i| self.buildings[i as usize].contains(p))
            {
                *covered.entry(hit).or_default() += 1;
            }
        }
        let total: u32 = covered.values().sum();
        if f64::from(total) < MIN_COVERAGE * samples.len() as f64 {
            return None;
        }

        // Heights and roof rises average over the objects that have one, by
        // covered area; the storey count is taken from the largest contributor,
        // since averaging counts across unequal parts means nothing.
        let weighted = |value: fn(&TopoBuilding) -> Option<f64>| -> Option<f64> {
            let (mut sum, mut weight) = (0.0, 0.0);
            for (&i, &count) in &covered {
                if let Some(v) = value(&self.buildings[i as usize]) {
                    sum += v * f64::from(count);
                    weight += f64::from(count);
                }
            }
            (weight > 0.0).then(|| sum / weight)
        };
        let levels = covered
            .iter()
            .filter_map(|(&i, &count)| Some((count, i, self.buildings[i as usize].levels?)))
            // Ties broken on the index so the pick does not depend on map order.
            .max_by_key(|&(count, i, _)| (count, std::cmp::Reverse(i)))
            .map(|(_, _, levels)| levels);

        Some(Hint {
            eave_height_m: weighted(|b| b.eave_height_m),
            roof_rise_m: weighted(|b| b.roof_rise_m),
            levels,
        })
    }
}

/// Writes the hint as OSM tags. Returns whether anything was written.
fn write_tags(tags: &mut HashMap<String, String>, hint: Hint) -> bool {
    // The storey count is only kept when it agrees with the measured height.
    let levels = match (hint.levels, hint.eave_height_m) {
        (Some(levels), Some(height)) => {
            let storey = height / levels as f64;
            (MIN_STOREY_M..=MAX_STOREY_M)
                .contains(&storey)
                .then_some(levels)
        }
        (levels, None) => levels,
        (None, _) => None,
    };
    if hint.eave_height_m.is_none() && levels.is_none() {
        return false;
    }

    if let Some(levels) = levels {
        tags.insert("building:levels".to_string(), levels.to_string());
    }
    if let Some(eave) = hint.eave_height_m {
        // OSM `height` runs to the top of the roof. The generator only takes the
        // roof off the walls when the roof shape is mapped, so the measured rise
        // is added exactly in that case; otherwise the eave height is the wall
        // height it should build, and whatever roof it picks sits on top.
        let pitched = tags.get("roof:shape").is_some_and(|shape| shape != "flat");
        match hint.roof_rise_m.filter(|_| pitched) {
            Some(rise) => {
                tags.insert("height".to_string(), format!("{:.1}", eave + rise));
                tags.entry("roof:height".to_string())
                    .or_insert_with(|| format!("{rise:.1}"));
            }
            None => {
                tags.insert("height".to_string(), format!("{eave:.1}"));
            }
        }
    }
    // Marks the provenance for debugging and for the licence notice.
    tags.insert("arnis:height_source".to_string(), "ign_bdtopo".to_string());
    true
}

/// The ground an OSM building stands on: its outer rings minus its inner ones.
struct Footprint {
    outers: Vec<Vec<Point>>,
    inners: Vec<Vec<Point>>,
}

impl Footprint {
    fn of(element: &ProcessedElement) -> Option<Self> {
        let ring = |nodes: &[ProcessedNode]| -> Option<Vec<Point>> {
            let mut points: Vec<Point> = nodes
                .iter()
                .map(|n| (f64::from(n.x), f64::from(n.z)))
                .collect();
            if points.len() > 1 && points.first() == points.last() {
                points.pop();
            }
            (points.len() >= 3).then_some(points)
        };
        let footprint = match element {
            ProcessedElement::Way(way) => Self {
                outers: ring(&way.nodes).into_iter().collect(),
                inners: Vec::new(),
            },
            ProcessedElement::Relation(relation) => {
                let rings = |role: ProcessedMemberRole| -> Vec<Vec<Point>> {
                    relation
                        .members
                        .iter()
                        .filter(|m| m.role == role)
                        .filter_map(|m| ring(&m.way.nodes))
                        .collect()
                };
                Self {
                    outers: rings(ProcessedMemberRole::Outer),
                    inners: rings(ProcessedMemberRole::Inner),
                }
            }
            ProcessedElement::Node(_) => return None,
        };
        (!footprint.outers.is_empty()).then_some(footprint)
    }

    /// Points spread evenly over the footprint, about `TARGET_SAMPLES` of them.
    fn samples(&self) -> Vec<Point> {
        let area: f64 = self.outers.iter().map(|r| ring_area(r)).sum();
        // Never finer than a block: coordinates are whole blocks to begin with.
        let step = (area / TARGET_SAMPLES).sqrt().max(1.0);
        let mut samples = Vec::new();
        for outer in &self.outers {
            let (mut min_x, mut min_z, mut max_x, mut max_z) =
                (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for &(x, z) in outer {
                min_x = min_x.min(x);
                min_z = min_z.min(z);
                max_x = max_x.max(x);
                max_z = max_z.max(z);
            }
            let mut z = min_z + step / 2.0;
            while z < max_z {
                let mut x = min_x + step / 2.0;
                while x < max_x {
                    let p = (x, z);
                    if point_in_ring(p, outer) && !self.inners.iter().any(|i| point_in_ring(p, i)) {
                        samples.push(p);
                    }
                    x += step;
                }
                z += step;
            }
        }
        if samples.is_empty() {
            // Slivers thinner than the grid step: fall back to one point.
            let outer = &self.outers[0];
            let n = outer.len() as f64;
            samples.push((
                outer.iter().map(|p| p.0).sum::<f64>() / n,
                outer.iter().map(|p| p.1).sum::<f64>() / n,
            ));
        }
        samples
    }
}

fn cell(v: f64) -> i32 {
    (v / CELL).floor() as i32
}

fn ring_area(ring: &[Point]) -> f64 {
    let mut twice = 0.0;
    for i in 0..ring.len() {
        let (x1, z1) = ring[i];
        let (x2, z2) = ring[(i + 1) % ring.len()];
        twice += x1 * z2 - x2 * z1;
    }
    twice.abs() / 2.0
}

fn point_in_ring(p: Point, ring: &[Point]) -> bool {
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (xi, zi) = ring[i];
        let (xj, zj) = ring[j];
        if (zi > p.1) != (zj > p.1) && p.0 < (xj - xi) * (p.1 - zi) / (zj - zi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm_parser::{ProcessedMember, ProcessedRelation, ProcessedWay};
    use std::sync::Arc;

    fn rect(x0: f64, z0: f64, x1: f64, z1: f64) -> Vec<Point> {
        vec![(x0, z0), (x1, z0), (x1, z1), (x0, z1)]
    }

    fn topo(
        bounds: (f64, f64, f64, f64),
        eave: Option<f64>,
        rise: Option<f64>,
        levels: Option<i64>,
    ) -> TopoBuilding {
        let polygon = Polygon {
            exterior: rect(bounds.0, bounds.1, bounds.2, bounds.3),
            holes: Vec::new(),
        };
        TopoBuilding::new(vec![polygon], eave, rise, levels).unwrap()
    }

    fn ign(buildings: Vec<TopoBuilding>) -> IgnBuildings {
        IgnBuildings { buildings }
    }

    fn nodes(ring: &[(i32, i32)]) -> Vec<ProcessedNode> {
        ring.iter()
            .enumerate()
            .map(|(i, &(x, z))| ProcessedNode {
                id: i as u64,
                tags: HashMap::new(),
                x,
                z,
            })
            .collect()
    }

    fn way(id: u64, bounds: (i32, i32, i32, i32), tags: &[(&str, &str)]) -> ProcessedWay {
        let (x0, z0, x1, z1) = bounds;
        ProcessedWay {
            id,
            nodes: nodes(&[(x0, z0), (x1, z0), (x1, z1), (x0, z1), (x0, z0)]),
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn building(id: u64, bounds: (i32, i32, i32, i32), tags: &[(&str, &str)]) -> ProcessedElement {
        ProcessedElement::Way(way(id, bounds, tags))
    }

    fn tag<'a>(element: &'a ProcessedElement, key: &str) -> Option<&'a str> {
        element.tags().get(key).map(String::as_str)
    }

    #[test]
    fn an_untagged_building_takes_the_measured_height() {
        let data = ign(vec![topo(
            (0.0, 0.0, 10.0, 10.0),
            Some(12.4),
            None,
            Some(4),
        )]);
        let mut elements = vec![building(1, (0, 0, 10, 10), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements), 1);
        assert_eq!(tag(&elements[0], "height"), Some("12.4"));
        assert_eq!(tag(&elements[0], "building:levels"), Some("4"));
        assert_eq!(tag(&elements[0], "arnis:height_source"), Some("ign_bdtopo"));
    }

    #[test]
    fn an_osm_height_or_level_count_is_never_overwritten() {
        let data = ign(vec![topo(
            (0.0, 0.0, 10.0, 10.0),
            Some(12.4),
            None,
            Some(4),
        )]);
        let mut elements = vec![
            building(1, (0, 0, 10, 10), &[("building", "yes"), ("height", "30")]),
            building(
                2,
                (0, 0, 10, 10),
                &[("building", "yes"), ("building:levels", "9")],
            ),
        ];

        assert_eq!(data.apply(&mut elements), 0);
        assert_eq!(tag(&elements[0], "height"), Some("30"));
        assert_eq!(tag(&elements[1], "height"), None);
        assert_eq!(tag(&elements[1], "building:levels"), Some("9"));
    }

    #[test]
    fn only_buildings_are_touched() {
        let data = ign(vec![topo((0.0, 0.0, 10.0, 10.0), Some(12.4), None, None)]);
        let mut elements = vec![building(1, (0, 0, 10, 10), &[("landuse", "grass")])];

        assert_eq!(data.apply(&mut elements), 0);
        assert_eq!(tag(&elements[0], "height"), None);
    }

    #[test]
    fn a_footprint_mostly_outside_any_object_is_left_alone() {
        // The object covers a fifth of the footprint: a neighbour, not a match.
        let data = ign(vec![topo((0.0, 0.0, 4.0, 20.0), Some(15.0), None, None)]);
        let mut elements = vec![building(1, (0, 0, 20, 20), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements), 0);
    }

    #[test]
    fn a_building_over_several_objects_takes_the_area_weighted_height() {
        // Three quarters at 10 m, one quarter at 20 m.
        let data = ign(vec![
            topo((0.0, 0.0, 30.0, 20.0), Some(10.0), None, None),
            topo((30.0, 0.0, 40.0, 20.0), Some(20.0), None, None),
        ]);
        let mut elements = vec![building(1, (0, 0, 40, 20), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements), 1);
        let height: f64 = tag(&elements[0], "height").unwrap().parse().unwrap();
        assert!((height - 12.5).abs() < 0.6, "height {height}");
    }

    #[test]
    fn several_buildings_inside_one_object_all_inherit_it() {
        let data = ign(vec![topo((0.0, 0.0, 40.0, 10.0), Some(9.0), None, None)]);
        let mut elements = vec![
            building(1, (0, 0, 20, 10), &[("building", "yes")]),
            building(2, (20, 0, 40, 10), &[("building", "house")]),
        ];

        assert_eq!(data.apply(&mut elements), 2);
        assert_eq!(tag(&elements[0], "height"), Some("9.0"));
        assert_eq!(tag(&elements[1], "height"), Some("9.0"));
    }

    #[test]
    fn a_storey_count_that_contradicts_the_height_is_dropped() {
        // One storey for 20 m: the tax-record count is wrong, the height is kept.
        let data = ign(vec![topo(
            (0.0, 0.0, 10.0, 10.0),
            Some(20.0),
            None,
            Some(1),
        )]);
        let mut elements = vec![building(1, (0, 0, 10, 10), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements), 1);
        assert_eq!(tag(&elements[0], "height"), Some("20.0"));
        assert_eq!(tag(&elements[0], "building:levels"), None);
    }

    #[test]
    fn a_storey_count_alone_is_still_used() {
        let data = ign(vec![topo((0.0, 0.0, 10.0, 10.0), None, None, Some(3))]);
        let mut elements = vec![building(1, (0, 0, 10, 10), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements), 1);
        assert_eq!(tag(&elements[0], "building:levels"), Some("3"));
        assert_eq!(tag(&elements[0], "height"), None);
    }

    #[test]
    fn the_roof_rise_is_added_only_under_a_mapped_pitched_roof() {
        let data = ign(vec![topo(
            (0.0, 0.0, 10.0, 10.0),
            Some(6.0),
            Some(3.0),
            None,
        )]);
        let mut elements = vec![
            building(1, (0, 0, 10, 10), &[("building", "yes")]),
            building(
                2,
                (0, 0, 10, 10),
                &[("building", "yes"), ("roof:shape", "gabled")],
            ),
            building(
                3,
                (0, 0, 10, 10),
                &[("building", "yes"), ("roof:shape", "flat")],
            ),
        ];

        assert_eq!(data.apply(&mut elements), 3);
        // Unmapped roof: the eave height is the wall height.
        assert_eq!(tag(&elements[0], "height"), Some("6.0"));
        assert_eq!(tag(&elements[0], "roof:height"), None);
        // Mapped pitched roof: total height, with the rise the generator subtracts.
        assert_eq!(tag(&elements[1], "height"), Some("9.0"));
        assert_eq!(tag(&elements[1], "roof:height"), Some("3.0"));
        assert_eq!(tag(&elements[2], "height"), Some("6.0"));
    }

    #[test]
    fn a_courtyard_does_not_count_as_covered_ground() {
        // A ring of building around a hole; the OSM footprint sits in the hole.
        let ring_building = TopoBuilding::new(
            vec![Polygon {
                exterior: rect(0.0, 0.0, 40.0, 40.0),
                holes: vec![rect(10.0, 10.0, 30.0, 30.0)],
            }],
            Some(18.0),
            None,
            None,
        )
        .unwrap();
        let data = ign(vec![ring_building]);
        let mut elements = vec![building(1, (12, 12, 28, 28), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements), 0);
    }

    #[test]
    fn a_multipolygon_relation_is_matched_on_its_outer_ring() {
        let data = ign(vec![topo((0.0, 0.0, 20.0, 20.0), Some(14.0), None, None)]);
        let relation = ProcessedRelation {
            id: 7,
            tags: HashMap::from([("building".to_string(), "yes".to_string())]),
            members: vec![ProcessedMember {
                role: ProcessedMemberRole::Outer,
                way: Arc::new(way(70, (0, 0, 20, 20), &[])),
            }],
        };
        let mut elements = vec![ProcessedElement::Relation(relation)];

        assert_eq!(data.apply(&mut elements), 1);
        assert_eq!(tag(&elements[0], "height"), Some("14.0"));
    }

    #[test]
    fn features_are_read_from_geojson() {
        let bbox =
            crate::coordinate_system::geographic::LLBBox::new(50.18, 1.62, 50.19, 1.63).unwrap();
        let (transformer, _) = CoordTransformer::llbbox_to_xzbbox(&bbox, 1.0).unwrap();
        let feature = |state: &str, height: f64| {
            serde_json::json!({
                "type": "Feature",
                "properties": {
                    "etat_de_l_objet": state,
                    "hauteur": height,
                    "nombre_d_etages": 2,
                    "altitude_minimale_toit": 20.0,
                    "altitude_maximale_toit": 23.5
                },
                "geometry": {"type": "MultiPolygon", "coordinates": [[[
                    [1.6240, 50.1850, 20.0], [1.6243, 50.1850, 20.0],
                    [1.6243, 50.1852, 20.0], [1.6240, 50.1852, 20.0],
                    [1.6240, 50.1850, 20.0]
                ]]]}
            })
        };

        let kept = IgnBuildings::from_features(&[feature("En service", 6.2)], &transformer);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept.buildings[0].eave_height_m, Some(6.2));
        assert_eq!(kept.buildings[0].roof_rise_m, Some(3.5));
        assert_eq!(kept.buildings[0].levels, Some(2));

        // A ruin, and a height no building has, contribute nothing.
        assert_eq!(
            IgnBuildings::from_features(&[feature("En ruine", 6.2)], &transformer).len(),
            0
        );
        let absurd = IgnBuildings::from_features(&[feature("En service", 0.4)], &transformer);
        assert_eq!(absurd.buildings[0].eave_height_m, None);
    }
}
