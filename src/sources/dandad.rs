//! D&AD events (creative-industry talks) — scraper over the page data that
//! dandad.org embeds in every page.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   disallows only `/search/`, `/basket/` and `/account/`, so `/events` and
//!   `/events/<slug>` are allowed.
//! * There is no JSON-LD, but every page embeds its server-side app state in
//!   `<script id="props" type="application/json">` (the same platform as
//!   Somerset House). The `/events` listing's `data.page.items.edges[].node`
//!   are the upcoming events (all cities) with their next instance's ISO
//!   `startDate`, `locationText` ("D&AD HQ") and `priceText`; the listing is a
//!   single page (`pageInfo.hasNextPage` was false with one event on
//!   2026-09-26). A listing with no events is an error unless the page's
//!   own count (`items.total`) is 0.
//! * Each detail page (at most [`MAX_DETAIL_PAGES`] per run) adds the
//!   `factsheet` — a "Time" row ("18:00–21:00") and a "Location" row with the
//!   full address and coordinates — plus `overviewText` (description HTML),
//!   the hero image and every `eventInstances` date. One [`RawEvent`] is
//!   emitted per instance.
//! * D&AD runs events worldwide: only items whose factsheet coordinates
//!   fall in a Greater London bounding box, or whose factsheet location (or,
//!   without one, the instance's location text) mentions London, are kept;
//!   online and other-city events are skipped (`Ok(None)`).
//! * Times are London wall-clock on the instance's `startDate`; an end time
//!   before the start is taken to be after midnight. Without a "Time" row
//!   the event is stored at London midnight with no end.
//! * Category: `map_category` over the title, else talk (D&AD's events are
//!   talks, panels and networking evenings).
//! * The site's terms allow personal use only, so the seed stores facts +
//!   link only (no description, no image); the scraper still emits what it
//!   finds and `repo::upsert_event` applies the policy.

use async_trait::async_trait;
use chrono::{Duration, NaiveDate, NaiveTime};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, in_london_bbox, london_to_utc, map_category,
    parse_price,
};

pub const KEY: &str = "dandad";
/// Upper bound on detail pages fetched per run (≈ 20 s at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 10;
const LISTING_PATH: &str = "/events";
const EVENT_PREFIX: &str = "/events/";

pub struct Dandad {
    base_url: Url,
}

impl Dandad {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// The `script#props` JSON of a dandad.org page.
pub fn page_props(html: &str) -> Result<Value, SourceError> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("script#props").expect("valid selector");
    let script = doc
        .select(&sel)
        .next()
        .ok_or_else(|| SourceError::Parse("no script#props on the page".into()))?;
    let json = script.text().collect::<String>();
    serde_json::from_str(&json)
        .map_err(|e| SourceError::Parse(format!("script#props is not valid JSON: {e}")))
}

/// One upcoming event on the listing page.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ListingItem {
    /// `/events/<slug>`.
    pub path: String,
    pub title: Option<String>,
    /// The listing's `nextInstance` (startDate, datesText, locationText, priceText).
    pub next_instance: Value,
}

/// Parse the `/events` listing's embedded data into event paths (in page
/// order, de-duplicated). Empty only when the page says there are no
/// upcoming events (`items.total == 0`); otherwise a listing without event
/// links is an error.
pub fn parse_listing(html: &str) -> Result<Vec<ListingItem>, SourceError> {
    let props = page_props(html)?;
    let edges = props
        .pointer("/data/page/items/edges")
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::Parse("script#props has no data.page.items.edges".into()))?;
    let mut out: Vec<ListingItem> = Vec::new();
    for node in edges.iter().filter_map(|e| e.get("node")) {
        let Some(path) = node.get("url").and_then(Value::as_str) else {
            continue;
        };
        let valid = path.strip_prefix(EVENT_PREFIX).is_some_and(|slug| {
            !slug.is_empty()
                && slug
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
        if !valid || out.iter().any(|i| i.path == path) {
            continue;
        }
        out.push(ListingItem {
            path: path.to_string(),
            title: node.get("title").and_then(Value::as_str).map(clean_text),
            next_instance: node.get("nextInstance").cloned().unwrap_or(Value::Null),
        });
    }
    // The site reports a count: zero upcoming events is a legitimate empty
    // listing, anything else without event links is a template change.
    let total = props
        .pointer("/data/page/items/total")
        .and_then(Value::as_u64);
    if out.is_empty() && total != Some(0) {
        return Err(SourceError::Parse(
            "no event links found on the events listing".into(),
        ));
    }
    Ok(out)
}

/// Parse one detail page into one [`RawEvent`] per event instance. `url` is
/// the address the page was fetched from; its last path segment is the
/// stable source id (suffixed with `@<date>` when there are several
/// instances). `fallback_instance` (the listing's `nextInstance`) is used
/// when the page lists no instances. Errors when the page has no event data.
pub fn parse_detail(
    html: &str,
    url: &Url,
    fallback_instance: &Value,
) -> Result<Vec<RawEvent>, SourceError> {
    let props = page_props(html)?;
    let page = props
        .pointer("/data/page")
        .filter(|p| p.get("__typename").and_then(Value::as_str) == Some("EventDetailPage"))
        .ok_or_else(|| SourceError::Parse("not an event detail page".into()))?;
    let title = page
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event page without title".into()))?;
    let factsheet = page
        .get("factsheet")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let row = |label: &str| {
        factsheet
            .iter()
            .find(|r| r.get("label").and_then(Value::as_str) == Some(label))
    };
    let time_text = row("Time")
        .and_then(|r| r.get("richtextValue").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty());
    let location = row("Location");
    let address = location
        .and_then(|r| r.get("value"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let coordinates = location
        .and_then(|r| r.get("coordinates"))
        .cloned()
        .unwrap_or(Value::Null);
    let image_url = ["/heroMedia/image/src", "/metaImage/src"]
        .iter()
        .find_map(|p| page.pointer(p).and_then(Value::as_str))
        .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
        .map(str::to_string);
    let mut instances: Vec<Value> = page
        .pointer("/eventInstances/edges")
        .and_then(Value::as_array)
        .map(|edges| {
            edges
                .iter()
                .filter_map(|e| e.get("node").cloned())
                .collect()
        })
        .unwrap_or_default();
    if instances.is_empty() && fallback_instance.is_object() {
        instances.push(fallback_instance.clone());
    }
    let slug = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let several = instances.len() > 1;
    Ok(instances
        .into_iter()
        .map(|instance| {
            let date = instance
                .get("startDate")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let id = if several {
                format!("{slug}@{date}")
            } else {
                slug.clone()
            };
            RawEvent {
                source_event_id: id,
                source_url: Some(url.to_string()),
                payload: json!({
                    "url": url.as_str(),
                    "title": title,
                    "instance": {
                        "startDate": instance.get("startDate"),
                        "datesText": instance.get("datesText"),
                        "locationText": instance.get("locationText"),
                        "priceText": instance.get("priceText"),
                    },
                    "time_text": time_text,
                    "address": address,
                    "coordinates": coordinates,
                    "description": page.get("overviewText"),
                    "image_url": image_url,
                }),
            }
        })
        .collect())
}

/// Parse a factsheet time ("18:00–21:00", "18:30", "6–9pm", "6.30pm") into
/// start and optional end.
pub fn parse_time_range(text: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    fn clock(s: &str) -> Option<(u32, u32, Option<bool>)> {
        let s = s.trim().to_lowercase();
        let (num, pm) = if let Some(n) = s.strip_suffix("pm") {
            (n.trim().to_string(), Some(true))
        } else if let Some(n) = s.strip_suffix("am") {
            (n.trim().to_string(), Some(false))
        } else {
            (s, None)
        };
        let (h, m) = match num.split_once([':', '.']) {
            Some((h, m)) => (h.parse().ok()?, m.parse().ok()?),
            None => (num.parse().ok()?, 0),
        };
        let valid = m < 60
            && if pm.is_some() {
                (1..=12).contains(&h)
            } else {
                h < 24
            };
        valid.then_some((h, m, pm))
    }
    fn to_time((h, m, pm): (u32, u32, Option<bool>)) -> Option<NaiveTime> {
        let h = match pm {
            Some(true) if h < 12 => h + 12,
            Some(false) if h == 12 => 0,
            _ => h,
        };
        NaiveTime::from_hms_opt(h, m, 0)
    }
    let Some((a, b)) = text.split_once(['–', '—', '-']) else {
        return Some((to_time(clock(text)?)?, None));
    };
    let end_clock = clock(b)?;
    let end = to_time(end_clock)?;
    let (sh, sm, spm) = clock(a)?;
    let start = match (spm, end_clock.2) {
        (None, Some(true)) => {
            let pm = to_time((sh, sm, Some(true)))?;
            if pm <= end {
                pm
            } else {
                to_time((sh, sm, Some(false)))?
            }
        }
        (None, Some(false)) => to_time((sh, sm, Some(false)))?,
        _ => to_time((sh, sm, spm))?,
    };
    Some((start, Some(end)))
}

/// Normalise a D&AD [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |p: &str| {
        payload
            .pointer(p)
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|t| !t.is_empty())
    };
    let title = text("/title").ok_or_else(|| SourceError::Parse("event without title".into()))?;
    let address_lines: Vec<String> = payload
        .get("address")
        .and_then(Value::as_str)
        .map(|a| {
            a.lines()
                .map(clean_text)
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let location_text = text("/instance/locationText");
    let place = if address_lines.is_empty() {
        location_text.clone().unwrap_or_default()
    } else {
        address_lines.join(", ")
    };
    let coord = |k: &str| payload.pointer(&format!("/coordinates/{k}"))?.as_f64();
    let lower = place.to_lowercase();
    let in_london = match (coord("lat"), coord("lng")) {
        (Some(lat), Some(lng)) => in_london_bbox(lat, lng),
        _ => false,
    } || lower.contains("london");
    if !in_london || lower.contains("online") {
        return Ok(None);
    }
    let date_text = text("/instance/startDate")
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no instance startDate")))?;
    let date = NaiveDate::parse_from_str(&date_text, "%Y-%m-%d")
        .map_err(|_| SourceError::Parse(format!("{title:?}: bad startDate {date_text:?}")))?;
    let (starts_at, ends_at, all_day) = match text("/time_text") {
        Some(t) => {
            let (start, end) = parse_time_range(&t)
                .ok_or_else(|| SourceError::Parse(format!("{title:?}: unrecognised time {t:?}")))?;
            let end = end.map(|e| {
                let end_day = if e <= start {
                    date + Duration::days(1)
                } else {
                    date
                };
                london_to_utc(end_day.and_time(e))
            });
            (london_to_utc(date.and_time(start)), end, false)
        }
        None => (london_to_utc(date.and_time(NaiveTime::MIN)), None, true),
    };
    let venue_name = location_text
        .clone()
        .or_else(|| address_lines.first().cloned())
        .unwrap_or_else(|| "D&AD".to_string());
    let address = if address_lines.len() > 1 {
        Some(address_lines[1..].join(", "))
    } else {
        address_lines.first().cloned()
    };
    let price = text("/instance/priceText")
        .map(|p| parse_price(&p))
        .unwrap_or_default();
    let category = map_category(&[title.as_str()]).unwrap_or(Category::Talk);

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(&venue_name)),
        description: clean_description(payload.get("description").and_then(Value::as_str)),
        title,
        venue_name: Some(venue_name),
        address,
        lat: coord("lat"),
        lng: coord("lng"),
        starts_at,
        ends_at: ends_at.filter(|e| *e > starts_at),
        all_day,
        price,
        url: text("/url"),
        image_url: text("/image_url"),
        category,
        tags: vec!["design".to_string(), "creative industry".to_string()],
    }))
}

#[async_trait]
impl Source for Dandad {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let items = parse_listing(&ctx.get_text(&url).await?)?;
        let mut out = Vec::new();
        for item in items.iter().take(MAX_DETAIL_PAGES) {
            let url = match self.base_url.join(&item.path) {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad detail path {}: {e}", item.path));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html, &url, &item.next_instance) {
                    Ok(raws) => out.extend(raws),
                    Err(e) => ctx.report_error(format!("{}: {e}", item.path)),
                },
                Err(e) => ctx.report_error(format!("{}: {e}", item.path)),
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Option<(String, Option<String>)> {
        parse_time_range(text).map(|(a, b)| (a.to_string(), b.map(|b| b.to_string())))
    }

    fn times(a: &str, b: Option<&str>) -> Option<(String, Option<String>)> {
        Some((a.into(), b.map(str::to_string)))
    }

    #[test]
    fn parses_factsheet_times() {
        assert_eq!(t("18:00–21:00"), times("18:00:00", Some("21:00:00")));
        assert_eq!(t("18:30"), times("18:30:00", None));
        assert_eq!(t("6–9pm"), times("18:00:00", Some("21:00:00")));
        assert_eq!(t("11–1pm"), times("11:00:00", Some("13:00:00")));
        assert_eq!(t("9.30–11am"), times("09:30:00", Some("11:00:00")));
        assert_eq!(t("6.30pm"), times("18:30:00", None));
        assert_eq!(t("TBC"), None);
    }

    fn payload(address: Option<&str>, location_text: &str, time: Option<&str>) -> Value {
        payload_at(
            address,
            location_text,
            time,
            json!({"lat": 51.5, "lng": -0.07}),
        )
    }

    fn payload_at(
        address: Option<&str>,
        location_text: &str,
        time: Option<&str>,
        coordinates: Value,
    ) -> Value {
        json!({
            "url": "https://www.dandad.org/events/x",
            "title": "A talk",
            "instance": {
                "startDate": "2026-09-30",
                "datesText": "30 September 2026",
                "locationText": location_text,
                "priceText": "£25",
            },
            "time_text": time,
            "address": address,
            "coordinates": coordinates,
        })
    }

    #[test]
    fn london_events_are_london_wall_clock() {
        let e = normalise_payload(&payload(
            Some("D&AD\r\n64 Cheshire Street\r\nE2 6EH, London"),
            "D&AD HQ",
            Some("18:00–21:00"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.venue_name.as_deref(), Some("D&AD HQ"));
        assert_eq!(
            e.address.as_deref(),
            Some("64 Cheshire Street, E2 6EH, London")
        );
        assert_eq!(e.starts_at.to_rfc3339(), "2026-09-30T17:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-09-30T20:00:00+00:00");
        assert_eq!(e.category, Category::Talk);
        assert_eq!(e.price.min.map(|d| d.to_string()).as_deref(), Some("25"));
    }

    #[test]
    fn late_end_rolls_over_midnight() {
        let e = normalise_payload(&payload(None, "Shoreditch, London", Some("20:00–01:00")))
            .unwrap()
            .unwrap();
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-01T00:00:00+00:00");
    }

    #[test]
    fn other_cities_and_online_events_are_skips() {
        for (address, location) in [
            (Some("D&AD Asia\r\nBeijing"), "Beijing"),
            (None, "New York"),
            (None, "Online"),
            (None, "Online (London time)"),
            (None, ""),
        ] {
            let beijing = json!({"lat": 39.9, "lng": 116.4});
            assert!(
                normalise_payload(&payload_at(address, location, Some("18:00"), beijing))
                    .unwrap()
                    .is_none(),
                "{location}"
            );
        }
        // Online stays a skip even with London coordinates.
        assert!(
            normalise_payload(&payload(None, "Online", Some("18:00")))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn london_coordinates_count_without_the_word_london() {
        let e = normalise_payload(&payload(
            Some("Some Studio\n91 Brick Lane\nE1 6QL"),
            "Some Studio",
            Some("18:00"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.venue_name.as_deref(), Some("Some Studio"));
        assert!(
            normalise_payload(&payload_at(
                Some("Some Studio\n91 Brick Lane\nE1 6QL"),
                "Some Studio",
                Some("18:00"),
                Value::Null,
            ))
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn bad_times_are_errors() {
        assert!(normalise_payload(&payload(None, "London", Some("evening"))).is_err());
    }

    #[test]
    fn multi_instance_pages_get_one_raw_event_per_date() {
        let props = json!({"data": {"page": {
            "__typename": "EventDetailPage",
            "title": "Workshop series",
            "factsheet": [],
            "eventInstances": {"edges": [
                {"node": {"startDate": "2026-10-01", "locationText": "London"}},
                {"node": {"startDate": "2026-10-08", "locationText": "London"}},
            ]},
        }}});
        let html = format!(
            r#"<html><body><script id="props" type="application/json">{props}</script></body></html>"#
        );
        let url: Url = "https://www.dandad.org/events/series".parse().unwrap();
        let raws = parse_detail(&html, &url, &Value::Null).unwrap();
        let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
        assert_eq!(ids, ["series@2026-10-01", "series@2026-10-08"]);
        let e = normalise_payload(&raws[1].payload).unwrap().unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-07T23:00:00+00:00");
        assert_eq!(e.ends_at, None);
    }
}
