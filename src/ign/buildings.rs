//! Building heights and looks from the BD TOPO, the French national topographic database.
//!
//! OpenStreetMap in France has nearly every footprint (imported from the land
//! registry) but almost never a height, a type or a material, so the generator
//! has to guess them. The BD TOPO records them. There is no identifier shared
//! with OSM, and the two databases do not cut blocks into buildings the same
//! way, so an OSM building is matched to whatever BD TOPO objects cover its
//! footprint: it takes their height weighted by how much of it each one covers,
//! and the type, materials and age of the one covering most of it.
//!
//! Strictly additive, like the Overture hints: a tag OSM already has is never
//! replaced, and `building=yes` is the only building type that gets refined.

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
/// Sample points aimed for when reading a roof: small houses need half-block steps.
const ROOF_SAMPLES: f64 = 200.0;
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
    /// Type, materials and age, as OSM tag values.
    looks: Looks,
}

/// What a BD TOPO building says about how it looks, as OSM tag values.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Looks {
    /// `building=*` refinement for an OSM `building=yes`.
    kind: Option<&'static str>,
    /// `building:material`
    wall: Option<&'static str>,
    /// `roof:material`
    roof: Option<&'static str>,
    /// `start_date`: year the building first appears.
    year: Option<i32>,
}

impl Looks {
    fn is_empty(&self) -> bool {
        *self == Looks::default()
    }

    fn from_properties(props: &Value, levels: Option<i64>) -> Self {
        let text = |key: &str| props.get(key).and_then(Value::as_str);
        Self {
            kind: building_kind(
                text("nature"),
                text("usage_1"),
                props.get("nombre_de_logements").and_then(Value::as_i64),
                levels,
            ),
            wall: text("materiaux_des_murs").and_then(wall_material),
            roof: text("materiaux_de_la_toiture").and_then(roof_material),
            year: text("date_d_apparition")
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse::<i32>().ok())
                .filter(|y| (1000..=2100).contains(y)),
        }
    }
}

/// Wall material from the land-registry code the BD TOPO carries: the first
/// digit is the main material (1 stone, 2 millstone, 3 concrete, 4 brick,
/// 5 breeze block, 6 wood, 9 other, 0 unknown). The table is the land
/// registry's; on the Picardy coast brick dominates, as it should.
fn wall_material(code: &str) -> Option<&'static str> {
    match code.chars().next()? {
        '1' | '2' => Some("stone"),
        '3' => Some("concrete"),
        '4' => Some("brick"),
        '5' => Some("cement_block"),
        '6' => Some("wood"),
        _ => None,
    }
}

/// Roof material from the same registry: 1 tiles, 2 slate, 3 zinc or
/// aluminium, 4 concrete, 9 other, 0 unknown.
fn roof_material(code: &str) -> Option<&'static str> {
    match code.chars().next()? {
        '1' => Some("roof_tiles"),
        '2' => Some("slate"),
        '3' => Some("zinc"),
        '4' => Some("concrete"),
        _ => None,
    }
}

/// `building=*` for a BD TOPO nature and use, or `None` where they say nothing
/// more than `yes`.
fn building_kind(
    nature: Option<&str>,
    usage: Option<&str>,
    dwellings: Option<i64>,
    levels: Option<i64>,
) -> Option<&'static str> {
    match nature {
        Some("Eglise") => return Some("church"),
        Some("Chapelle") => return Some("chapel"),
        Some("Château") => return Some("castle"),
        Some("Fort, blockhaus, casemate") => return Some("bunker"),
        Some("Tour, donjon") => return Some("tower"),
        Some("Serre") => return Some("greenhouse"),
        Some("Silo") => return Some("silo"),
        _ => {}
    }
    match usage? {
        // Flats from three dwellings or three storeys up; a house below that.
        "Résidentiel" => match (dwellings, levels) {
            (Some(d), _) if d >= 3 => Some("apartments"),
            (_, Some(l)) if l >= 3 => Some("apartments"),
            (Some(_), _) | (_, Some(_)) => Some("house"),
            _ => None,
        },
        "Annexe" => Some("shed"),
        "Agricole" => Some("farm_auxiliary"),
        "Commercial et services" => Some("commercial"),
        "Industriel" => Some("industrial"),
        "Religieux" => Some("religious"),
        "Sportif" => Some("sports_hall"),
        _ => None,
    }
}

impl TopoBuilding {
    fn new(
        polygons: Vec<Polygon>,
        eave_height_m: Option<f64>,
        roof_rise_m: Option<f64>,
        levels: Option<i64>,
        looks: Looks,
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
            looks,
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
        let looks = Looks::from_properties(props, levels);
        if eave_height_m.is_none() && levels.is_none() && looks.is_empty() {
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
        Self::new(polygons, eave_height_m, roof_rise_m, levels, looks)
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
    looks: Looks,
}

/// How many OSM buildings an [`IgnBuildings::apply`] enriched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Enriched {
    /// Given a height or storey count.
    pub heights: usize,
    /// Of those, the ones whose height was read from the LiDAR under their
    /// own footprint rather than taken from the BD TOPO.
    pub lidar_heights: usize,
    /// Given a type, a material or a construction year.
    pub looks: usize,
    /// Given a roof shape read from the LiDAR.
    pub roofs: usize,
    /// Given a roof colour read from the aerial photographs.
    pub roof_colours: usize,
}

/// The BD TOPO buildings of an area, ready to lend their heights to OSM buildings.
#[derive(Default)]
pub struct IgnBuildings {
    buildings: Vec<TopoBuilding>,
    /// LiDAR heights above ground, to read roof shapes from where available.
    surface: Option<super::roofs::SurfaceHeights>,
    /// Aerial photograph, to read roof colours from where available.
    colours: Option<super::ortho::RoofColours>,
}

impl IgnBuildings {
    pub fn from_features(features: &[Value], transformer: &CoordTransformer) -> Self {
        Self {
            buildings: features
                .iter()
                .filter_map(|f| TopoBuilding::from_feature(f, transformer))
                .collect(),
            surface: None,
            colours: None,
        }
    }

    /// Reads roof colours from this photograph as well.
    pub fn with_colours(mut self, colours: Option<super::ortho::RoofColours>) -> Self {
        self.colours = colours;
        self
    }

    pub fn has_colours(&self) -> bool {
        self.colours.is_some()
    }

    /// Reads roof shapes from these LiDAR heights as well.
    pub fn with_surface(mut self, surface: Option<super::roofs::SurfaceHeights>) -> Self {
        self.surface = surface;
        self
    }

    pub fn has_surface(&self) -> bool {
        self.surface.is_some()
    }

    pub fn len(&self) -> usize {
        self.buildings.len()
    }

    /// Fill height, storeys, type, materials and construction year on OSM
    /// buildings, each only where OSM has nothing for it.
    pub fn apply(&self, elements: &mut [ProcessedElement]) -> Enriched {
        let mut enriched = Enriched::default();
        if self.buildings.is_empty() {
            return enriched;
        }
        let index = self.index();

        for element in elements.iter_mut() {
            let tags = element.tags();
            if !tags.contains_key("building") && !tags.contains_key("building:part") {
                continue;
            }
            // Any existing vertical tag means OSM already knows better.
            let wants_height =
                !tags.contains_key("height") && !tags.contains_key("building:levels");
            let wants_roof = self.surface.is_some() && !tags.contains_key("roof:shape");
            let wants_colour = self.colours.is_some()
                && !tags.contains_key("roof:material")
                && !tags.contains_key("roof:colour");
            if !wants_height && !wants_roof && !wants_colour && !wants_looks(tags) {
                continue;
            }
            let Some(footprint) = Footprint::of(element) else {
                continue;
            };
            let roof = self
                .surface
                .as_ref()
                .filter(|_| wants_roof)
                .and_then(|surface| super::roofs::classify(&footprint.interior_samples(), surface));
            let measured = self
                .surface
                .as_ref()
                .filter(|_| wants_height)
                .and_then(|surface| super::roofs::measure(&footprint.interior_samples(), surface));
            let hint = match (self.hint_for(&footprint, &index), roof, measured) {
                (Some(hint), _, _) => hint,
                // The LiDAR alone still gives the roof and the height.
                (None, Some(_), _) | (None, _, Some(_)) => Hint::default(),
                (None, None, None) => continue,
            };
            let tags: &mut HashMap<String, String> = match element {
                ProcessedElement::Way(way) => &mut way.tags,
                ProcessedElement::Relation(relation) => &mut relation.tags,
                ProcessedElement::Node(_) => continue,
            };
            // The roof first, so the height below knows whether it is pitched.
            let mut hint = hint;
            if let Some(roof) = roof {
                tags.insert("roof:shape".to_string(), roof.shape.to_string());
                if let Some(orientation) = roof.orientation {
                    tags.entry("roof:orientation".to_string())
                        .or_insert_with(|| orientation.to_string());
                }
                if let Some(direction) = roof.direction {
                    tags.entry("roof:direction".to_string())
                        .or_insert_with(|| direction.to_string());
                }
                if roof.shape != "flat" {
                    tags.entry("roof:height".to_string())
                        .or_insert_with(|| format!("{:.1}", roof.rise_m));
                    // Measured on this very roof, it beats the BD TOPO's figure.
                    hint.roof_rise_m = Some(roof.rise_m);
                }
                tags.insert("arnis:roof_source".to_string(), "ign_lidar_hd".to_string());
                enriched.roofs += 1;
            }
            // Measured under this very footprint, it beats the height the BD
            // TOPO spreads over a whole terrace.
            let mut from_lidar = false;
            if let Some(measured) = measured {
                hint.eave_height_m = Some(measured.eave_m);
                if hint.roof_rise_m.is_none() || roof.is_none() {
                    hint.roof_rise_m = Some(measured.top_m - measured.eave_m);
                }
                from_lidar = true;
            }
            if wants_height && write_tags(tags, hint) {
                if from_lidar {
                    tags.insert(
                        "arnis:height_source".to_string(),
                        "ign_lidar_hd".to_string(),
                    );
                    enriched.lidar_heights += 1;
                }
                enriched.heights += 1;
            }
            if write_looks(tags, hint.looks) {
                enriched.looks += 1;
            }
            // Only where neither OSM nor the BD TOPO said what the roof is made of.
            if let Some(colours) = self.colours.as_ref() {
                if !tags.contains_key("roof:material") && !tags.contains_key("roof:colour") {
                    if let Some(colour) =
                        super::ortho::roof_colour(&footprint.interior_samples(), colours)
                    {
                        tags.insert("roof:colour".to_string(), colour);
                        tags.insert(
                            "arnis:roof_colour_source".to_string(),
                            "ign_bd_ortho".to_string(),
                        );
                        enriched.roof_colours += 1;
                    }
                }
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
        // Type and materials are not averaged either: they are those of the
        // object that covers most of the footprint.
        let looks = covered
            .iter()
            .max_by_key(|&(&i, &count)| (count, std::cmp::Reverse(i)))
            .map(|(&i, _)| self.buildings[i as usize].looks)
            .unwrap_or_default();

        Some(Hint {
            eave_height_m: weighted(|b| b.eave_height_m),
            roof_rise_m: weighted(|b| b.roof_rise_m),
            levels,
            looks,
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

/// Whether OSM leaves any of the looks unsaid on a building.
fn wants_looks(tags: &HashMap<String, String>) -> bool {
    let missing = |keys: &[&str]| !keys.iter().any(|k| tags.contains_key(*k));
    tags.get("building").map(String::as_str) == Some("yes")
        || missing(&[
            "building:material",
            "building:facade:material",
            "facade:material",
            "building:colour",
            "colour",
        ])
        || missing(&["roof:material", "roof:colour"])
        || missing(&["start_date", "construction_date"])
}

/// Writes the looks where OSM has nothing. Returns whether anything was written.
fn write_looks(tags: &mut HashMap<String, String>, looks: Looks) -> bool {
    let missing =
        |tags: &HashMap<String, String>, keys: &[&str]| !keys.iter().any(|k| tags.contains_key(*k));
    let mut written = false;
    let mut set = |tags: &mut HashMap<String, String>, key: &str, value: String| {
        tags.insert(key.to_string(), value);
        written = true;
    };
    if let Some(kind) = looks.kind {
        // Only a plain `building=yes` is refined; parts keep their own tagging.
        if tags.get("building").map(String::as_str) == Some("yes") {
            set(tags, "building", kind.to_string());
        }
    }
    if let Some(wall) = looks.wall {
        // A mapped colour already decides the wall; a material would override it.
        if missing(
            tags,
            &[
                "building:material",
                "building:facade:material",
                "facade:material",
                "building:colour",
                "colour",
            ],
        ) {
            set(tags, "building:material", wall.to_string());
        }
    }
    if let Some(roof) = looks.roof {
        if missing(tags, &["roof:material", "roof:colour"]) {
            set(tags, "roof:material", roof.to_string());
        }
    }
    if let Some(year) = looks.year {
        if missing(tags, &["start_date", "construction_date"]) {
            set(tags, "start_date", year.to_string());
        }
    }
    if written {
        tags.insert("arnis:looks_source".to_string(), "ign_bdtopo".to_string());
    }
    written
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

    /// Samples at least half a block inside the outline, clear of the walls and
    /// the ground beside them that the LiDAR mixes in at the edge.
    fn interior_samples(&self) -> Vec<Point> {
        let inside = |p: Point| {
            self.outers.iter().any(|o| point_in_ring(p, o))
                && !self.inners.iter().any(|i| point_in_ring(p, i))
        };
        self.samples_at(ROOF_SAMPLES, 0.5)
            .into_iter()
            .filter(|&(x, z)| {
                [(0.5, 0.0), (-0.5, 0.0), (0.0, 0.5), (0.0, -0.5)]
                    .iter()
                    .all(|&(dx, dz)| inside((x + dx, z + dz)))
            })
            .collect()
    }

    /// Points spread evenly over the footprint, about `TARGET_SAMPLES` of them.
    fn samples(&self) -> Vec<Point> {
        // Never finer than a block: coordinates are whole blocks to begin with.
        self.samples_at(TARGET_SAMPLES, 1.0)
    }

    /// About `target` points spread evenly, no closer than `min_step` blocks.
    fn samples_at(&self, target: f64, min_step: f64) -> Vec<Point> {
        let area: f64 = self.outers.iter().map(|r| ring_area(r)).sum();
        let step = (area / target).sqrt().max(min_step);
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
        TopoBuilding::new(vec![polygon], eave, rise, levels, Looks::default()).unwrap()
    }

    fn ign(buildings: Vec<TopoBuilding>) -> IgnBuildings {
        IgnBuildings {
            buildings,
            surface: None,
            colours: None,
        }
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

        assert_eq!(data.apply(&mut elements).heights, 1);
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

        assert_eq!(data.apply(&mut elements).heights, 0);
        assert_eq!(tag(&elements[0], "height"), Some("30"));
        assert_eq!(tag(&elements[1], "height"), None);
        assert_eq!(tag(&elements[1], "building:levels"), Some("9"));
    }

    #[test]
    fn only_buildings_are_touched() {
        let data = ign(vec![topo((0.0, 0.0, 10.0, 10.0), Some(12.4), None, None)]);
        let mut elements = vec![building(1, (0, 0, 10, 10), &[("landuse", "grass")])];

        assert_eq!(data.apply(&mut elements).heights, 0);
        assert_eq!(tag(&elements[0], "height"), None);
    }

    #[test]
    fn a_footprint_mostly_outside_any_object_is_left_alone() {
        // The object covers a fifth of the footprint: a neighbour, not a match.
        let data = ign(vec![topo((0.0, 0.0, 4.0, 20.0), Some(15.0), None, None)]);
        let mut elements = vec![building(1, (0, 0, 20, 20), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements).heights, 0);
    }

    #[test]
    fn a_building_over_several_objects_takes_the_area_weighted_height() {
        // Three quarters at 10 m, one quarter at 20 m.
        let data = ign(vec![
            topo((0.0, 0.0, 30.0, 20.0), Some(10.0), None, None),
            topo((30.0, 0.0, 40.0, 20.0), Some(20.0), None, None),
        ]);
        let mut elements = vec![building(1, (0, 0, 40, 20), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements).heights, 1);
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

        assert_eq!(data.apply(&mut elements).heights, 2);
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

        assert_eq!(data.apply(&mut elements).heights, 1);
        assert_eq!(tag(&elements[0], "height"), Some("20.0"));
        assert_eq!(tag(&elements[0], "building:levels"), None);
    }

    #[test]
    fn a_storey_count_alone_is_still_used() {
        let data = ign(vec![topo((0.0, 0.0, 10.0, 10.0), None, None, Some(3))]);
        let mut elements = vec![building(1, (0, 0, 10, 10), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements).heights, 1);
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

        assert_eq!(data.apply(&mut elements).heights, 3);
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
            Looks::default(),
        )
        .unwrap();
        let data = ign(vec![ring_building]);
        let mut elements = vec![building(1, (12, 12, 28, 28), &[("building", "yes")])];

        assert_eq!(data.apply(&mut elements).heights, 0);
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

        assert_eq!(data.apply(&mut elements).heights, 1);
        assert_eq!(tag(&elements[0], "height"), Some("14.0"));
    }

    #[test]
    fn looks_fill_only_what_osm_leaves_unsaid() {
        let looks = Looks {
            kind: Some("house"),
            wall: Some("brick"),
            roof: Some("slate"),
            year: Some(1900),
        };
        let measured = TopoBuilding::new(
            vec![Polygon {
                exterior: rect(0.0, 0.0, 10.0, 10.0),
                holes: Vec::new(),
            }],
            Some(7.0),
            None,
            None,
            looks,
        )
        .unwrap();
        let data = ign(vec![measured]);
        let mut elements = vec![
            building(1, (0, 0, 10, 10), &[("building", "yes")]),
            building(
                2,
                (0, 0, 10, 10),
                &[
                    ("building", "church"),
                    ("height", "12"),
                    ("roof:colour", "red"),
                    ("building:material", "stone"),
                ],
            ),
        ];

        let enriched = data.apply(&mut elements);

        assert_eq!(
            enriched,
            Enriched {
                heights: 1,
                looks: 2,
                roofs: 0,
                roof_colours: 0,
                lidar_heights: 0
            }
        );
        assert_eq!(tag(&elements[0], "building"), Some("house"));
        assert_eq!(tag(&elements[0], "building:material"), Some("brick"));
        assert_eq!(tag(&elements[0], "roof:material"), Some("slate"));
        assert_eq!(tag(&elements[0], "start_date"), Some("1900"));
        // A mapped type, height, colour and material all stand; only the year is new.
        assert_eq!(tag(&elements[1], "building"), Some("church"));
        assert_eq!(tag(&elements[1], "height"), Some("12"));
        assert_eq!(tag(&elements[1], "roof:material"), None);
        assert_eq!(tag(&elements[1], "building:material"), Some("stone"));
        assert_eq!(tag(&elements[1], "start_date"), Some("1900"));
    }

    #[test]
    fn registry_codes_and_uses_map_to_osm_values() {
        assert_eq!(wall_material("41"), Some("brick"));
        assert_eq!(wall_material("10"), Some("stone"));
        assert_eq!(wall_material("00"), None);
        assert_eq!(roof_material("20"), Some("slate"));
        assert_eq!(roof_material("13"), Some("roof_tiles"));
        assert_eq!(
            building_kind(Some("Eglise"), Some("Religieux"), None, None),
            Some("church")
        );
        assert_eq!(
            building_kind(
                Some("Indifférenciée"),
                Some("Résidentiel"),
                Some(12),
                Some(5)
            ),
            Some("apartments")
        );
        assert_eq!(
            building_kind(
                Some("Indifférenciée"),
                Some("Résidentiel"),
                Some(1),
                Some(2)
            ),
            Some("house")
        );
        assert_eq!(building_kind(None, Some("Résidentiel"), None, None), None);
        assert_eq!(building_kind(None, Some("Indifférencié"), None, None), None);
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
