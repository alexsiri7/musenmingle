//! Nonclassical — a London promoter of new classical, experimental and
//! electronic music, at venues across town (issue #219). Read from the
//! JSON-LD on its Squarespace event pages.
//!
//! * robots.txt (checked 2026-11-08, saved as a fixture): the `User-agent: *`
//!   group allows `/events` and `/events/<slug>`, and disallows `/api/`,
//!   `/search`, `/static/` and query variants such as `?format=json` and
//!   `?format=ical`, so only the HTML pages are read.
//! * `/events` is the upcoming list (`div.eventlist`, one
//!   `article.eventlist-event` per event, no Event markup; often only one
//!   or none at all, so the source may be empty). Each article's title links
//!   its page (`/events/<slug>`, where the slug may hold the date it was
//!   posted, not the event's: `/events/2026/9/18/…` is on 19 November). At
//!   most [`MAX_DETAIL_PAGES`] pages a run.
//! * Each event page carries one JSON-LD `Event` whose `startDate`/`endDate`
//!   have honest offsets (`2026-10-15T19:30:00+0100` for a 7:30pm gig in
//!   October, `…+0000` after the clocks go back, as the listing's printed
//!   times and the pages' Google Calendar links confirm). Every event is
//!   timed, so none is `all_day`. Its `name` carries a " — nonclassical"
//!   suffix, which is dropped.
//! * The `location` is a `Place` whose `address` lines end in "United
//!   Kingdom" and usually hold only the postcode ("The Mall\nSW1Y 5AH").
//!   Events are kept when the place says London or has a London postcode
//!   ([`is_london`]); the rest (Bristol's Arnolfini, or a place whose
//!   address is only "United Kingdom") are skipped (`Ok(None)`).
//! * The description is the page's text blocks; prices are only on the
//!   ticket sites, so price is unknown.
//! * Category: every event is music (gigs, album launches, the summer
//!   party), tagged `contemporary` and `experimental`, the promoter's own
//!   genres; `crate::music` adds `classical` and the rest from titles.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::jsonld::{extract_events, image_url};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, postcode_outward};

pub const KEY: &str = "nonclassical";
/// Per-run cap on event page fetches (the list rarely shows more than a few).
pub const MAX_DETAIL_PAGES: usize = 20;
const LISTING_PATH: &str = "/events";
const EVENT_PREFIX: &str = "/events/";
const TITLE_SUFFIX: &str = "— nonclassical";
const TAGS: &[&str] = &["contemporary", "experimental"];
/// Postcode areas wholly inside London; outer ones (BR, CR, HA…) reach
/// beyond it, so those addresses must say London.
const LONDON_POSTCODE_AREAS: &[&str] = &["E", "EC", "N", "NW", "SE", "SW", "W", "WC"];

pub struct Nonclassical {
    base_url: Url,
    max_detail_pages: usize,
}

impl Nonclassical {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            max_detail_pages: MAX_DETAIL_PAGES,
        }
    }

    /// Override the per-run cap on event pages (tests).
    pub fn with_max_detail_pages(mut self, n: usize) -> Self {
        self.max_detail_pages = n;
        self
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

/// The event page paths (`/events/<slug>`) linked from the upcoming list,
/// in page order, de-duplicated; `page_url` is the address the list was
/// fetched from, and links must stay on its host. `None` when the page has
/// no event list at all (not the same as an empty one).
pub fn parse_listing(html: &str, page_url: &Url) -> Option<Vec<String>> {
    let doc = Html::parse_document(html);
    let list = doc.select(&selector("div.eventlist")).next()?;
    let mut out: Vec<String> = Vec::new();
    for a in list.select(&selector("article.eventlist-event .entry-title a[href]")) {
        let Some(Ok(url)) = a.value().attr("href").map(|h| page_url.join(h)) else {
            continue;
        };
        let valid = url.host_str() == page_url.host_str()
            && url.query().is_none()
            && url
                .path()
                .strip_prefix(EVENT_PREFIX)
                .is_some_and(|slug| !slug.is_empty());
        if valid && !out.iter().any(|p| p == url.path()) {
            out.push(url.path().to_string());
        }
    }
    Some(out)
}

/// Parse an event page (fetched from `page_url`) into a [`RawEvent`]: its
/// JSON-LD `Event` plus the description. `None` without one.
pub fn parse_detail(html: &str, page_url: &Url) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let event = extract_events(&doc).into_iter().next()?;
    let description: Vec<String> = doc
        .select(&selector(".eventitem-column-content .sqs-html-content"))
        .map(|b| b.inner_html())
        .collect();
    let id = page_url.path().strip_prefix(EVENT_PREFIX)?;
    Some(RawEvent {
        source_event_id: id.to_string(),
        source_url: Some(page_url.to_string()),
        payload: json!({
            "url": page_url.as_str(),
            "jsonld": event,
            "description": description.join("\n"),
        }),
    })
}

/// A JSON-LD date with its offset (`2026-10-15T19:30:00+0100`).
fn parse_offset_datetime(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_str(s.trim(), "%Y-%m-%dT%H:%M:%S%z")
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Whether a place (its name and address) is in London: it says so, or its
/// postcode is in one of [`LONDON_POSTCODE_AREAS`].
pub fn is_london(place: &str) -> bool {
    if place.to_lowercase().contains("london") {
        return true;
    }
    postcode_outward(place).is_some_and(|outward| {
        let area: String = outward
            .chars()
            .take_while(char::is_ascii_alphabetic)
            .collect();
        LONDON_POSTCODE_AREAS.contains(&area.as_str())
    })
}

/// The address lines of a JSON-LD `Place`, without the country.
fn address_lines(address: &str) -> String {
    address
        .lines()
        .map(clean_text)
        .filter(|l| !l.is_empty() && l != "United Kingdom")
        .collect::<Vec<_>>()
        .join(", ")
}

/// Normalise a Nonclassical [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let ev = payload
        .get("jsonld")
        .ok_or_else(|| SourceError::Parse("payload without jsonld".into()))?;
    let field = |key: &str| ev.get(key).and_then(Value::as_str);
    let title = field("name")
        .map(clean_text)
        .map(|t| t.trim_end_matches(TITLE_SUFFIX).trim_end().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event page without a title".into()))?;
    let starts_at = field("startDate")
        .and_then(parse_offset_datetime)
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no startDate")))?;
    let ends_at = field("endDate")
        .and_then(parse_offset_datetime)
        .filter(|e| *e > starts_at);

    let place = ev.get("location");
    let place_text = |key: &str| place.and_then(|p| p.get(key)).and_then(Value::as_str);
    let venue_name = place_text("name").map(clean_text).filter(|n| !n.is_empty());
    let address = place_text("address")
        .map(address_lines)
        .filter(|a| !a.is_empty());
    let where_ = [venue_name.as_deref(), address.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
    if !is_london(&where_) {
        return Ok(None);
    }

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, venue_name.as_deref()),
        description: clean_description(payload.get("description").and_then(Value::as_str)),
        title,
        venue_name,
        address,
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day: false,
        price: Price::default(),
        url: payload
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        image_url: image_url(ev),
        category: Category::Music,
        tags: TAGS.iter().map(|t| t.to_string()).collect(),
    }))
}

#[async_trait]
impl Source for Nonclassical {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listing_url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let listing = ctx.get_text(&listing_url).await?;
        let paths = parse_listing(&listing, &listing_url)
            .ok_or_else(|| SourceError::Parse("no event list on the events page".into()))?;
        if paths.len() > self.max_detail_pages {
            ctx.report_error(format!(
                "{} event pages listed, only the first {} read",
                paths.len(),
                self.max_detail_pages
            ));
        }
        let mut out = Vec::new();
        for path in paths.iter().take(self.max_detail_pages) {
            let url = match self.base_url.join(path) {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad event path {path}: {e}"));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html, &url) {
                    Some(raw) => out.push(raw),
                    None => ctx.report_error(format!("{path}: no JSON-LD Event")),
                },
                Err(e) => ctx.report_error(format!("{path}: {e}")),
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only events in London: an event whose location is outside London \
             (e.g. Bristol), or whose address doesn't say where (only \"United \
             Kingdom\"), is left out on purpose.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(start: &str, place: Value) -> Value {
        json!({
            "url": "https://www.nonclassical.co.uk/events/x",
            "jsonld": {
                "@type": "Event",
                "name": "A Gig &amp; More \u{2014} nonclassical",
                "startDate": start,
                "endDate": "2026-10-15T23:30:00+0100",
                "location": place,
            },
            "description": "<p>Sounds.</p>",
        })
    }

    fn victoria() -> Value {
        json!({"name": "The Victoria", "address": "451 Queensbridge Road\nLondon, England, E8 3AS\nUnited Kingdom"})
    }

    #[test]
    fn honours_the_offset_and_drops_the_title_suffix() {
        let ev = normalise_payload(&payload("2026-10-15T19:30:00+0100", victoria()))
            .unwrap()
            .unwrap();
        assert_eq!(ev.title, "A Gig & More");
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-10-15T18:30:00+00:00");
        assert_eq!(
            ev.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-15T22:30:00+00:00")
        );
        assert!(!ev.all_day);
        assert_eq!(ev.venue_name.as_deref(), Some("The Victoria"));
        assert_eq!(
            ev.address.as_deref(),
            Some("451 Queensbridge Road, London, England, E8 3AS")
        );
        assert_eq!(ev.category, Category::Music);
        assert_eq!(ev.tags, ["contemporary", "experimental"]);
    }

    #[test]
    fn a_bad_start_is_a_parse_error() {
        let err = normalise_payload(&payload("2026-10-15", victoria()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no startDate"), "{err}");
    }

    #[test]
    fn london_by_name_or_postcode() {
        assert!(is_london("ICA, The Mall, SW1Y 5AH"));
        assert!(is_london("Theatreship, South Quay Plaza, E14 9SH"));
        assert!(is_london("Somewhere, London"));
        assert!(is_london("Hall, 1 Road, ec1a 1bb"));
        assert!(!is_london(
            "Arnolfini, 16 Narrow Quay Bristol BS1 4QA, BS1 4QA"
        ));
        assert!(!is_london("Iklectik"));
        assert!(!is_london("Hall, Bromley BR1 1AA"));
        assert!(is_london("Hall, Bromley, London BR1 1AA"));
    }

    #[test]
    fn events_outside_london_or_without_an_address_are_skipped() {
        let bristol = json!({"name": "Arnolfini ", "address": "16 Narrow Quay Bristol BS1 4QA\nBS1 4QA\nUnited Kingdom"});
        let nowhere = json!({"name": "Iklectik", "address": "United Kingdom"});
        for place in [bristol, nowhere, Value::Null] {
            assert!(
                normalise_payload(&payload("2026-10-15T19:30:00+0100", place))
                    .unwrap()
                    .is_none()
            );
        }
    }
}
