//! Conway Hall (Red Lion Square, Holborn) — talks, courses and festival
//! events from the JSON-LD on its what's-on listing.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   disallows only `/wp-admin/`.
//! * `/whats-on/` lists every upcoming event as an `li.ch-event-filter` card
//!   with the site's categories in `data-category` (`talks-debates`,
//!   `workshops`, `festivals`, `concerts`, `film`), a link to
//!   `/whats-on/event/<slug>/` and one JSON-LD `Event` block (name, start,
//!   end, location, image, description; no url or offers). One request a
//!   run, no detail pages.
//! * Some blocks carry raw newlines inside strings, which strict JSON
//!   rejects, so control characters are replaced by spaces before parsing.
//! * Times are London wall clock without an offset (`2026-10-04 18:30:00`,
//!   printed "4th October 2026 · 6:30pm"), read with
//!   `parse_london_wall_clock`. Every event is timed. Some `endDate`s
//!   carry the wrong day (a 3:00pm–4:30pm talk ending 19 days later), so
//!   only their clock time is used: on the start's day, or the next when
//!   it is earlier (an overnight event).
//! * Categories: `concerts` (the Sunday Concerts, recitals, opera) and
//!   `film` are skipped even when also tagged `festivals`; otherwise
//!   `talks-debates` → talk, `workshops` → workshop, `festivals` →
//!   community. Online-only events are skipped (hybrid ones are kept);
//!   `qa_scope` tells the scraper check.
//! * Every event is at Conway Hall (rooms such as the Brockway Room are
//!   inside it). No prices on the listing, so price is unknown.

use async_trait::async_trait;
use chrono::{DateTime, Days, Utc};
use chrono_tz::Europe::London;
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::jsonld::image_url;
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_to_utc, parse_london_wall_clock,
};

pub const KEY: &str = "conway-hall";
const LISTING_PATH: &str = "/whats-on/";
const EVENT_PREFIX: &str = "/whats-on/event/";
const VENUE_NAME: &str = "Conway Hall";
const VENUE_ADDRESS: &str = "25 Red Lion Square, London WC1R 4RL";
/// Site categories whose events are out of scope.
const SKIP_CATEGORIES: [&str; 2] = ["concerts", "film"];

pub struct ConwayHall {
    base_url: Url,
}

impl ConwayHall {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// Parse a JSON-LD block, tolerating raw control characters in strings.
fn parse_block(text: &str) -> Option<Value> {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    serde_json::from_str(cleaned.trim()).ok()
}

/// Every event card on the listing fetched from `page_url`, in page order.
/// A card without an event link or a JSON-LD `Event` is an error.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<Result<RawEvent, SourceError>> {
    let doc = Html::parse_document(html);
    let link_sel = selector("a[href]");
    let script_sel = selector(r#"script[type="application/ld+json"]"#);
    let series_sel = selector("h3.event-title span");
    let mut out: Vec<Result<RawEvent, SourceError>> = Vec::new();
    for card in doc.select(&selector("li.ch-event-filter")) {
        let url = card
            .select(&link_sel)
            .filter_map(|a| a.value().attr("href"))
            .filter_map(|h| page_url.join(h).ok())
            .find(|u| {
                u.host_str() == page_url.host_str()
                    && u.path()
                        .strip_prefix(EVENT_PREFIX)
                        .is_some_and(|slug| !slug.trim_matches('/').is_empty())
            });
        let event = card
            .select(&script_sel)
            .filter_map(|s| parse_block(&s.text().collect::<String>()))
            .find(|v| v.get("@type").and_then(Value::as_str) == Some("Event"));
        let (Some(url), Some(event)) = (url, event) else {
            out.push(Err(SourceError::Parse(format!(
                "event card without a link or JSON-LD Event: {:?}",
                card.select(&selector("h3")).next().map(element_text)
            ))));
            continue;
        };
        let categories: Vec<String> = card
            .value()
            .attr("data-category")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let series = card
            .select(&series_sel)
            .next()
            .map(element_text)
            .map(|s| s.trim_end_matches(':').trim().to_string())
            .filter(|s| !s.is_empty());
        let id = url.path().to_string();
        if out
            .iter()
            .any(|r| r.as_ref().is_ok_and(|r| r.source_event_id == id))
        {
            continue;
        }
        out.push(Ok(RawEvent {
            source_event_id: id,
            source_url: Some(url.to_string()),
            payload: json!({
                "url": url.as_str(),
                "jsonld": event,
                "categories": categories,
                "series": series,
            }),
        }));
    }
    out
}

/// The end of an event starting at `starts_at` whose JSON-LD `endDate` is
/// `end`: `end`'s London clock time on the start's day, or on the next day
/// when it is earlier. `None` when unparseable or not after the start.
pub fn end_time(starts_at: DateTime<Utc>, end: &str) -> Option<DateTime<Utc>> {
    let end = parse_london_wall_clock(end)?.with_timezone(&London).time();
    let start = starts_at.with_timezone(&London).naive_local();
    let day = if end < start.time() {
        start.date().checked_add_days(Days::new(1))?
    } else {
        start.date()
    };
    Some(london_to_utc(day.and_time(end))).filter(|e| *e > starts_at)
}

/// Category from the site's categories; `None` means out of scope.
pub fn category(site_categories: &[&str]) -> Option<Category> {
    let has = |c: &str| site_categories.contains(&c);
    if SKIP_CATEGORIES.iter().any(|c| has(c)) {
        None
    } else if has("talks-debates") {
        Some(Category::Talk)
    } else if has("workshops") {
        Some(Category::Workshop)
    } else if has("festivals") {
        Some(Category::Community)
    } else {
        None
    }
}

#[async_trait]
impl Source for ConwayHall {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let items = parse_listing(&ctx.get_text(&url).await?, &url);
        if items.is_empty() {
            return Err(SourceError::Parse("no event cards on the listing".into()));
        }
        let mut out = Vec::new();
        for item in items {
            match item {
                Ok(raw) => out.push(raw),
                Err(e) => ctx.report_error(e.to_string()),
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        let p = &raw.payload;
        let event = &p["jsonld"];
        let categories: Vec<&str> = p["categories"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let Some(category) = category(&categories) else {
            return Ok(None);
        };
        if event
            .get("eventAttendanceMode")
            .and_then(Value::as_str)
            .is_some_and(|m| m.ends_with("OnlineEventAttendanceMode"))
        {
            return Ok(None);
        }
        let title = event
            .get("name")
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| SourceError::Parse("event without a name".into()))?;
        let date = |k: &str| event.get(k).and_then(Value::as_str);
        let start = date("startDate")
            .ok_or_else(|| SourceError::Parse(format!("{title:?} has no startDate")))?;
        let starts_at = parse_london_wall_clock(start).ok_or_else(|| {
            SourceError::Parse(format!("{title:?}: unrecognised startDate {start:?}"))
        })?;
        let ends_at = date("endDate").and_then(|e| end_time(starts_at, e));
        // The series ("Ethical Matters", "Intelligence Squared", …).
        let tags: Vec<String> = p["series"]
            .as_str()
            .map(|s| s.to_lowercase())
            .into_iter()
            .collect();
        Ok(Some(NewEvent {
            sessions: Vec::new(),
            dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
            title,
            description: clean_description(event.get("description").and_then(Value::as_str)),
            venue_name: Some(VENUE_NAME.to_string()),
            address: Some(VENUE_ADDRESS.to_string()),
            lat: None,
            lng: None,
            starts_at,
            ends_at,
            all_day: false,
            price: Price::default(),
            url: p["url"].as_str().map(str::to_string),
            image_url: image_url(event),
            category,
            tags,
        }))
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only talks and debates, workshops and festival events held at Conway Hall. \
             Concerts (the Sunday Concerts, recitals, opera) and films are left out on \
             purpose, even when also listed under festivals (Bloomsbury Festival), as \
             are online-only events.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(category(&["talks-debates"]), Some(Category::Talk));
        assert_eq!(
            category(&["talks-debates", "workshops"]),
            Some(Category::Talk)
        );
        assert_eq!(
            category(&["festivals", "workshops"]),
            Some(Category::Workshop)
        );
        assert_eq!(category(&["festivals"]), Some(Category::Community));
        assert_eq!(category(&["concerts", "festivals"]), None);
        assert_eq!(category(&["film"]), None);
        assert_eq!(category(&[]), None);
    }

    #[test]
    fn the_end_keeps_its_clock_time_on_the_start_day() {
        let start = parse_london_wall_clock("2026-10-04 15:00:00").unwrap();
        assert_eq!(
            end_time(start, "2026-10-23 16:30:00"),
            parse_london_wall_clock("2026-10-04 16:30")
        );
        let start = parse_london_wall_clock("2026-10-17 19:00:00").unwrap();
        assert_eq!(
            end_time(start, "2026-10-18 14:00:00"),
            parse_london_wall_clock("2026-10-18 14:00")
        );
        assert_eq!(end_time(start, "2026-10-17 19:00:00"), None);
        assert_eq!(end_time(start, "soon"), None);
    }

    #[test]
    fn the_qa_scope_names_what_is_left_out() {
        let scope = ConwayHall::new(Url::parse("https://x.test/").unwrap())
            .qa_scope()
            .unwrap();
        for word in [
            "talks",
            "Sunday Concerts",
            "films",
            "festivals",
            "online-only",
        ] {
            assert!(scope.contains(word), "{word}");
        }
    }

    #[test]
    fn blocks_with_raw_newlines_in_strings_parse() {
        let v = parse_block("{\"@type\": \"Event\", \"description\": \"a\nb\"}").unwrap();
        assert_eq!(v["description"], "a b");
    }
}
