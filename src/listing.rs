//! `GET /v1/events` query parameters and pagination cursors (pure; the SQL
//! is in `repo::list_events`).

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use uuid::Uuid;

use crate::model::Category;
use crate::normalise::london_to_utc;

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 100;
pub const DEFAULT_RADIUS_KM: f64 = 5.0;
pub const MAX_RADIUS_KM: f64 = 100.0;

/// Mean Earth radius used for haversine distances.
pub const EARTH_RADIUS_KM: f64 = 6371.0088;

/// A parsed `GET /v1/events` request.
#[derive(Debug, Clone, PartialEq)]
pub struct EventQuery {
    pub filter: EventFilter,
    pub order: EventOrder,
    pub limit: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventFilter {
    /// Events still running at or after this instant (London midnight of `from`).
    pub from: Option<DateTime<Utc>>,
    /// Events starting before this instant (London midnight after `to`).
    pub until: Option<DateTime<Utc>>,
    /// Empty = any category.
    pub categories: Vec<Category>,
    pub free_only: bool,
    /// Source keys; empty = any. An event matches if any of its listings
    /// comes from one of them.
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventOrder {
    ByStart {
        after: Option<(DateTime<Utc>, Uuid)>,
    },
    ByDistance {
        near: Near,
        after: Option<(f64, Uuid)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Near {
    pub lat: f64,
    pub lng: f64,
    pub radius_km: f64,
}

/// Inclusive lat/lng box containing every point within `radius_km` of the
/// centre (a cheap, index-friendly prefilter before the exact distance).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingBox {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lng: f64,
    pub max_lng: f64,
}

impl Near {
    /// Ignores the antimeridian, which London never gets near.
    pub fn bounding_box(&self) -> BoundingBox {
        let angle = self.radius_km / EARTH_RADIUS_KM;
        let dlat = angle.to_degrees();
        // Widest longitude offset of the circle (it is reached north of the
        // centre's parallel, so `angle / cos(lat)` would be slightly short).
        let dlng = match angle.sin() / self.lat.to_radians().cos() {
            s if s < 1.0 => s.asin().to_degrees(),
            _ => 180.0,
        };
        BoundingBox {
            min_lat: (self.lat - dlat).max(-90.0),
            max_lat: (self.lat + dlat).min(90.0),
            min_lng: self.lng - dlng,
            max_lng: self.lng + dlng,
        }
    }
}

/// The position of the last event on a page. Encoded as an opaque string;
/// only valid with the same filters and sort it was issued for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cursor {
    Start(DateTime<Utc>, Uuid),
    Distance(f64, Uuid),
}

impl Cursor {
    pub fn encode(&self) -> String {
        let plain = match self {
            Cursor::Start(t, id) => format!("s:{}:{id}", t.timestamp_micros()),
            Cursor::Distance(d, id) => format!("d:{:016x}:{id}", d.to_bits()),
        };
        plain.bytes().map(|b| format!("{b:02x}")).collect()
    }

    pub fn decode(s: &str) -> Option<Cursor> {
        if s.len() % 2 != 0 || !s.is_ascii() {
            return None;
        }
        let bytes = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        let plain = String::from_utf8(bytes).ok()?;
        let mut parts = plain.splitn(3, ':');
        let (kind, pos, id) = (parts.next()?, parts.next()?, parts.next()?);
        let id = Uuid::parse_str(id).ok()?;
        match kind {
            "s" => Some(Cursor::Start(
                DateTime::from_timestamp_micros(pos.parse().ok()?)?,
                id,
            )),
            "d" => Some(Cursor::Distance(
                f64::from_bits(u64::from_str_radix(pos, 16).ok()?),
                id,
            )),
            _ => None,
        }
    }
}

/// Parse a raw query string (`category` may repeat). Errors are messages
/// for the client.
pub fn parse_query(raw: &str) -> Result<EventQuery, String> {
    let mut filter = EventFilter::default();
    let (mut from, mut to) = (None, None);
    let (mut near, mut radius_km) = (None, None);
    let (mut limit, mut cursor) = (None, None);
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        match key.as_ref() {
            "from" => from = Some(parse_date("from", &value)?),
            "to" => to = Some(parse_date("to", &value)?),
            "category" => {
                let c = Category::ALL
                    .into_iter()
                    .find(|c| c.as_str() == value)
                    .ok_or_else(|| format!("unknown category {value:?}"))?;
                if !filter.categories.contains(&c) {
                    filter.categories.push(c);
                }
            }
            "free" => {
                filter.free_only = match value.as_ref() {
                    "true" => true,
                    "false" => false,
                    _ => return Err("free must be true or false".into()),
                }
            }
            "source" => {
                if !is_source_key(&value) {
                    return Err(format!("invalid source {value:?}"));
                }
                if !filter.sources.iter().any(|s| *s == value) {
                    filter.sources.push(value.into_owned());
                }
            }
            "near" => near = Some(parse_near(&value)?),
            "radius_km" => {
                let r: f64 = value
                    .parse()
                    .map_err(|_| "radius_km must be a number".to_string())?;
                if !(r > 0.0 && r <= MAX_RADIUS_KM) {
                    return Err(format!("radius_km must be > 0 and <= {MAX_RADIUS_KM}"));
                }
                radius_km = Some(r);
            }
            "limit" => {
                let l: i64 = value
                    .parse()
                    .map_err(|_| "limit must be an integer".to_string())?;
                if !(1..=MAX_LIMIT).contains(&l) {
                    return Err(format!("limit must be between 1 and {MAX_LIMIT}"));
                }
                limit = Some(l);
            }
            "cursor" => {
                cursor = Some(Cursor::decode(&value).ok_or_else(|| "invalid cursor".to_string())?)
            }
            other => return Err(format!("unknown parameter {other:?}")),
        }
    }
    if let (Some(f), Some(t)) = (from, to) {
        if f > t {
            return Err("from must not be after to".into());
        }
    }
    filter.from = from.map(london_midnight);
    filter.until = to.and_then(|t| t.succ_opt()).map(london_midnight);

    let order = match (near, radius_km) {
        (None, Some(_)) => return Err("radius_km requires near".into()),
        (None, None) => EventOrder::ByStart {
            after: match cursor {
                None => None,
                Some(Cursor::Start(t, id)) => Some((t, id)),
                Some(Cursor::Distance(..)) => return Err("cursor does not match the sort".into()),
            },
        },
        (Some((lat, lng)), radius_km) => EventOrder::ByDistance {
            near: Near {
                lat,
                lng,
                radius_km: radius_km.unwrap_or(DEFAULT_RADIUS_KM),
            },
            after: match cursor {
                None => None,
                Some(Cursor::Distance(d, id)) => Some((d, id)),
                Some(Cursor::Start(..)) => return Err("cursor does not match the sort".into()),
            },
        },
    };
    Ok(EventQuery {
        filter,
        order,
        limit: limit.unwrap_or(DEFAULT_LIMIT),
    })
}

/// Source keys are kebab-case ASCII (`serpentine-galleries`).
pub fn is_source_key(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn parse_date(name: &str, value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("{name} must be a date (YYYY-MM-DD)"))
}

fn london_midnight(date: NaiveDate) -> DateTime<Utc> {
    london_to_utc(date.and_time(NaiveTime::MIN))
}

fn parse_near(value: &str) -> Result<(f64, f64), String> {
    let err = || "near must be <lat>,<lng>".to_string();
    let (lat, lng) = value.split_once(',').ok_or_else(err)?;
    let lat: f64 = lat.trim().parse().map_err(|_| err())?;
    let lng: f64 = lng.trim().parse().map_err(|_| err())?;
    if !((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng)) {
        return Err("near is out of range".into());
    }
    Ok((lat, lng))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn dates_are_london_days_and_to_is_inclusive() {
        let q = parse_query("from=2026-10-01&to=2026-10-01").unwrap();
        assert_eq!(
            q.filter.from,
            Some(Utc.with_ymd_and_hms(2026, 9, 30, 23, 0, 0).unwrap())
        );
        assert_eq!(
            q.filter.until,
            Some(Utc.with_ymd_and_hms(2026, 10, 1, 23, 0, 0).unwrap())
        );
        let q = parse_query("from=2026-12-01").unwrap();
        assert_eq!(
            q.filter.from,
            Some(Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap())
        );
        assert_eq!(q.filter.until, None);
    }

    #[test]
    fn defaults() {
        let q = parse_query("").unwrap();
        assert_eq!(q.filter, EventFilter::default());
        assert_eq!(q.order, EventOrder::ByStart { after: None });
        assert_eq!(q.limit, DEFAULT_LIMIT);
        let EventOrder::ByDistance { near, .. } = parse_query("near=51.5,-0.1").unwrap().order
        else {
            panic!("expected distance order");
        };
        assert_eq!(near.radius_km, DEFAULT_RADIUS_KM);
    }

    #[test]
    fn categories_repeat_and_dedupe() {
        let q = parse_query("category=talk&category=workshop&category=talk").unwrap();
        assert_eq!(q.filter.categories, [Category::Talk, Category::Workshop]);
    }

    #[test]
    fn sources_repeat_and_dedupe() {
        let q = parse_query("source=barbican&source=design-museum&source=barbican").unwrap();
        assert_eq!(q.filter.sources, ["barbican", "design-museum"]);
    }

    #[test]
    fn rejects_bad_input() {
        for raw in [
            "from=2026-10-2x",
            "from=2026-10-02&to=2026-10-01",
            "category=concert",
            "free=yes",
            "near=51.5",
            "near=91,0",
            "near=51.5,-0.1&radius_km=0",
            "near=51.5,-0.1&radius_km=101",
            "radius_km=3",
            "limit=0",
            "limit=101",
            "cursor=zz",
            "bogus=1",
            "source=",
            "source=Barbican",
            "source=a%20b",
        ] {
            assert!(parse_query(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn cursors_round_trip_and_must_match_the_sort() {
        let id = Uuid::new_v4();
        let t = Utc.with_ymd_and_hms(2026, 10, 1, 18, 30, 0).unwrap()
            + chrono::Duration::microseconds(123_456);
        for c in [
            Cursor::Start(t, id),
            Cursor::Distance(1.234_567_890_123, id),
        ] {
            assert_eq!(Cursor::decode(&c.encode()), Some(c));
        }
        let start = Cursor::Start(t, id).encode();
        let distance = Cursor::Distance(0.5, id).encode();
        assert!(parse_query(&format!("cursor={start}")).is_ok());
        assert!(parse_query(&format!("cursor={distance}")).is_err());
        assert!(parse_query(&format!("near=51.5,-0.1&cursor={distance}")).is_ok());
        assert!(parse_query(&format!("near=51.5,-0.1&cursor={start}")).is_err());
    }

    #[test]
    fn bounding_box_contains_the_radius() {
        let near = Near {
            lat: 51.5,
            lng: -0.1,
            radius_km: 5.0,
        };
        let b = near.bounding_box();
        // 5 km is ~0.045 degrees of latitude and ~0.072 of longitude here.
        assert!((b.max_lat - 51.5 - 0.0449).abs() < 0.001, "{b:?}");
        assert!((b.max_lng + 0.1 - 0.0722).abs() < 0.001, "{b:?}");
        assert!(b.min_lat < 51.5 && b.min_lng < -0.1);
    }
}
