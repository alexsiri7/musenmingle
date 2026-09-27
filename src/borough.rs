//! London boroughs (issue #79): which of the 32 London boroughs or the City
//! of London a point is in. Deterministic and offline: a point-in-polygon
//! test against simplified boundaries committed in
//! `src/london_boroughs.geojson` (see `docs/boroughs.md` for where the file
//! comes from and how to regenerate it).
//!
//! Source: Office for National Statistics, Local Authority Districts
//! (December 2024) Boundaries UK BFE, codes `E09*`, licensed under the Open
//! Government Licence v3.0 (credited on `/about`). Simplified with
//! Douglas-Peucker at 0.0001° (about 7-11 m), so a point within a few
//! metres of a border may land on either side, or (in a sliver between two
//! simplified rings) in none.
//!
//! `repo::upsert_event` stores `events.events.borough` from the row's
//! final coordinates, and `repo::sync_boroughs` refreshes every row after
//! each ingest run; the listing filters on `borough=` (`crate::listing`).

use std::sync::LazyLock;

/// The boroughs as `(key, name)`, in the order the filter shows them.
/// `key` is what `borough=` takes and `events.events.borough` stores.
pub const BOROUGHS: &[(&str, &str)] = &[
    ("barking-and-dagenham", "Barking and Dagenham"),
    ("barnet", "Barnet"),
    ("bexley", "Bexley"),
    ("brent", "Brent"),
    ("bromley", "Bromley"),
    ("camden", "Camden"),
    ("city-of-london", "City of London"),
    ("croydon", "Croydon"),
    ("ealing", "Ealing"),
    ("enfield", "Enfield"),
    ("greenwich", "Greenwich"),
    ("hackney", "Hackney"),
    ("hammersmith-and-fulham", "Hammersmith and Fulham"),
    ("haringey", "Haringey"),
    ("harrow", "Harrow"),
    ("havering", "Havering"),
    ("hillingdon", "Hillingdon"),
    ("hounslow", "Hounslow"),
    ("islington", "Islington"),
    ("kensington-and-chelsea", "Kensington and Chelsea"),
    ("kingston-upon-thames", "Kingston upon Thames"),
    ("lambeth", "Lambeth"),
    ("lewisham", "Lewisham"),
    ("merton", "Merton"),
    ("newham", "Newham"),
    ("redbridge", "Redbridge"),
    ("richmond-upon-thames", "Richmond upon Thames"),
    ("southwark", "Southwark"),
    ("sutton", "Sutton"),
    ("tower-hamlets", "Tower Hamlets"),
    ("waltham-forest", "Waltham Forest"),
    ("wandsworth", "Wandsworth"),
    ("westminster", "Westminster"),
];

/// The borough keys, for validating `borough=`.
pub static BOROUGH_KEYS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| BOROUGHS.iter().map(|(k, _)| *k).collect());

/// The facet value (`facets.borough`) counting events without a known
/// location. Not a filter value.
pub const UNKNOWN: &str = "unknown";

/// A borough's display name ("Tower Hamlets"); unknown keys come back as
/// given.
pub fn name(key: &str) -> &str {
    BOROUGHS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(key, |(_, n)| n)
}

const GEOJSON: &str = include_str!("london_boroughs.geojson");

/// A polygon: outer ring then holes, as `(lng, lat)` points.
type Polygon = Vec<Vec<(f64, f64)>>;

struct Shape {
    key: &'static str,
    /// `(min_lng, min_lat, max_lng, max_lat)`.
    bbox: (f64, f64, f64, f64),
    polygons: Vec<Polygon>,
}

static SHAPES: LazyLock<Vec<Shape>> = LazyLock::new(|| parse(GEOJSON));

fn parse(geojson: &str) -> Vec<Shape> {
    #[derive(serde::Deserialize)]
    struct Collection {
        features: Vec<Feature>,
    }
    #[derive(serde::Deserialize)]
    struct Feature {
        properties: Props,
        geometry: Geometry,
    }
    #[derive(serde::Deserialize)]
    struct Props {
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct Geometry {
        coordinates: Vec<Vec<Vec<(f64, f64)>>>,
    }
    let c: Collection =
        serde_json::from_str(geojson).expect("src/london_boroughs.geojson is valid GeoJSON");
    let mut shapes: Vec<Shape> = c
        .features
        .into_iter()
        .map(|f| {
            let key = BOROUGHS
                .iter()
                .find(|(_, n)| *n == f.properties.name)
                .map(|(k, _)| *k)
                .unwrap_or_else(|| panic!("unknown borough {:?}", f.properties.name));
            let mut bbox = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for &(x, y) in f.geometry.coordinates.iter().flat_map(|p| &p[0]) {
                bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
            }
            Shape {
                key,
                bbox,
                polygons: f.geometry.coordinates,
            }
        })
        .collect();
    // A fixed order, so a point in an overlap between two simplified
    // borders always gets the same answer.
    shapes.sort_by_key(|s| s.key);
    shapes
}

/// Even-odd ray casting.
fn in_ring(x: f64, y: f64, ring: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let mut j = ring.len().wrapping_sub(1);
    for i in 0..ring.len() {
        let ((xi, yi), (xj, yj)) = (ring[i], ring[j]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn in_polygon(x: f64, y: f64, p: &Polygon) -> bool {
    p.first().is_some_and(|outer| in_ring(x, y, outer)) && !p[1..].iter().any(|h| in_ring(x, y, h))
}

/// The key of the borough containing `(lat, lng)`, or None outside
/// London (or in a sliver between two simplified borders).
pub fn at(lat: f64, lng: f64) -> Option<&'static str> {
    if !lat.is_finite() || !lng.is_finite() {
        return None;
    }
    SHAPES
        .iter()
        .filter(|s| lng >= s.bbox.0 && lat >= s.bbox.1 && lng <= s.bbox.2 && lat <= s.bbox.3)
        .find(|s| s.polygons.iter().any(|p| in_polygon(lng, lat, p)))
        .map(|s| s.key)
}

/// An event's borough from its coordinates (both needed).
pub fn of(lat: Option<f64>, lng: Option<f64>) -> Option<&'static str> {
    at(lat?, lng?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_borough_has_a_shape_and_the_file_is_small() {
        assert_eq!(BOROUGHS.len(), 33);
        assert_eq!(SHAPES.len(), 33);
        let mut keys: Vec<_> = SHAPES.iter().map(|s| s.key).collect();
        keys.dedup();
        assert_eq!(keys, *BOROUGH_KEYS);
        assert!(GEOJSON.len() <= 500 * 1024, "{} bytes", GEOJSON.len());
    }

    #[test]
    fn known_venues_land_in_their_borough() {
        for (what, lat, lng, want) in [
            ("Tate Modern", 51.5076, -0.0994, "southwark"),
            ("Barbican", 51.5202, -0.0938, "city-of-london"),
            ("Whitechapel Gallery", 51.5160, -0.0705, "tower-hamlets"),
            (
                "V&A South Kensington",
                51.4966,
                -0.1722,
                "kensington-and-chelsea",
            ),
            ("National Gallery", 51.5089, -0.1283, "westminster"),
            ("Southbank Centre", 51.5065, -0.1160, "lambeth"),
            ("British Library", 51.5300, -0.1276, "camden"),
            ("Cutty Sark", 51.4829, -0.0096, "greenwich"),
            ("Kingston Museum", 51.4105, -0.2985, "kingston-upon-thames"),
        ] {
            assert_eq!(at(lat, lng), Some(want), "{what}");
        }
    }

    #[test]
    fn border_edges_and_outside_london() {
        // Either side of the City/Tower Hamlets border on Middlesex Street
        // (Petticoat Lane): the west kerb is the City, the east Tower Hamlets.
        assert_eq!(at(51.5165, -0.0775), Some("city-of-london"));
        assert_eq!(at(51.5165, -0.0760), Some("tower-hamlets"));
        // Mid-Thames at Blackfriars: the river is split between boroughs.
        assert!(matches!(
            at(51.5095, -0.1040),
            Some("city-of-london" | "southwark")
        ));
        // Outside Greater London.
        assert_eq!(at(50.8225, -0.1372), None); // Brighton
        assert_eq!(at(51.7520, -1.2577), None); // Oxford
        assert_eq!(at(f64::NAN, 0.0), None);
        assert_eq!(of(Some(51.5076), None), None);
        assert_eq!(of(Some(51.5076), Some(-0.0994)), Some("southwark"));
    }

    #[test]
    fn names() {
        assert_eq!(name("tower-hamlets"), "Tower Hamlets");
        assert_eq!(name("city-of-london"), "City of London");
        assert_eq!(name("mars"), "mars");
    }
}
