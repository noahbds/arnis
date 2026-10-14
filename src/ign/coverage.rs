//! Where the French national datasets can have anything to say.
//!
//! A cheap gate in front of every IGN request: outside these rectangles no
//! request is sent at all. The rectangles are deliberately generous, so a bbox
//! in a neighbouring country near the border still passes and simply gets an
//! empty answer; the gate exists to keep the rest of the world off the service,
//! not to trace the frontier.

use crate::coordinate_system::geographic::LLBBox;

/// `(min_lat, min_lng, max_lat, max_lng)` of each territory the Géoplateforme serves.
const TERRITORIES: &[(f64, f64, f64, f64)] = &[
    // Metropolitan France with Corsica
    (41.2, -5.6, 51.3, 9.8),
    // Guadeloupe, Saint-Martin, Saint-Barthélemy
    (15.7, -63.3, 18.2, -60.9),
    // Martinique
    (14.3, -61.3, 15.0, -60.7),
    // French Guiana
    (2.0, -54.7, 6.0, -51.5),
    // Réunion
    (-21.5, 55.1, -20.8, 55.9),
    // Mayotte
    (-13.1, 44.9, -12.5, 45.4),
    // Saint-Pierre-et-Miquelon
    (46.7, -56.5, 47.2, -56.0),
];

/// Whether `bbox` touches a territory covered by the French datasets.
pub fn may_be_covered(bbox: &LLBBox) -> bool {
    let (min, max) = (bbox.min(), bbox.max());
    TERRITORIES
        .iter()
        .any(|&(min_lat, min_lng, max_lat, max_lng)| {
            min.lat() <= max_lat
                && max.lat() >= min_lat
                && min.lng() <= max_lng
                && max.lng() >= min_lng
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bbox(min_lat: f64, min_lng: f64, max_lat: f64, max_lng: f64) -> LLBBox {
        LLBBox::new(min_lat, min_lng, max_lat, max_lng).unwrap()
    }

    #[test]
    fn french_territories_are_covered() {
        // Baie de Somme, Ajaccio, Saint-Denis de la Réunion, Cayenne
        assert!(may_be_covered(&bbox(50.18, 1.62, 50.19, 1.63)));
        assert!(may_be_covered(&bbox(41.91, 8.73, 41.93, 8.75)));
        assert!(may_be_covered(&bbox(-20.89, 55.44, -20.87, 55.46)));
        assert!(may_be_covered(&bbox(4.92, -52.34, 4.94, -52.32)));
    }

    #[test]
    fn the_rest_of_the_world_is_not_asked() {
        // Munich, New York, Sydney, Tokyo
        assert!(!may_be_covered(&bbox(48.13, 11.56, 48.15, 11.58)));
        assert!(!may_be_covered(&bbox(40.70, -74.02, 40.72, -74.00)));
        assert!(!may_be_covered(&bbox(-33.87, 151.20, -33.85, 151.22)));
        assert!(!may_be_covered(&bbox(35.67, 139.75, 35.69, 139.77)));
    }

    #[test]
    fn a_bbox_straddling_the_edge_still_counts() {
        // Reaches from the Rhine plain across the eastern edge of the rectangle.
        assert!(may_be_covered(&bbox(48.5, 9.7, 48.6, 9.9)));
    }
}
