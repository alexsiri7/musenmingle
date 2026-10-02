//! The Horse Hospital (Bloomsbury, WC1) — talks, workshops and exhibitions
//! from the JSON-LD on its Squarespace event pages.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): the `User-agent: *`
//!   group allows `/whats-on/` and `/events/`, and disallows `/api/`,
//!   `/search`, `/static/` and query variants such as `?format=json` and
//!   `?format=ical`, so only the HTML pages are read.
//! * `/whats-on/` is a summary block of upcoming events with no Event
//!   markup; each item's title links its page (`a.summary-title-link` →
//!   `/events/<slug>`, where a slug may itself contain a `/`, e.g.
//!   `/events/un/stable-8`). At most [`MAX_DETAIL_PAGES`] pages a run.
//! * Each event page carries one JSON-LD `Event` whose `startDate`/`endDate`
//!   have honest offsets (`2026-10-08T19:00:00+0100`, `…+0000` after the
//!   clocks go back). The printed 24-hour start
//!   (`time.event-time-24hr-start`) must agree with it or the page is a
//!   parse error; every event is timed, so none is `all_day`. The JSON-LD
//!   `name` carries a " — The Horse Hospital" suffix and its `location` is
//!   empty, so the title is the page's `h1.eventitem-title` and every event
//!   is placed at the venue.
//! * The description is the page's lead text block; the next block holds
//!   "Doors 7pm / Tickets £6-20 Sliding Scale" (or "Free entry/donations
//!   OTD"), which is read with `parse_price`.
//! * Categories are the page's own ("Posted in Talk, Music, …"): Workshop →
//!   workshop, Talk → talk, Exhibition → exhibition, in that precedence.
//!   Most of the programme is gigs (Music, Performance, Improvisation,
//!   Sound, Dance, Film alone), which are skipped (`Ok(None)`).
//!   `qa_scope` tells the scraper check the same.

use async_trait::async_trait;
use chrono::{DateTime, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::jsonld::{extract_events, image_url};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, parse_price,
};

pub const KEY: &str = "horse-hospital";
/// Per-run cap on event page fetches (the listing shows about 30).
pub const MAX_DETAIL_PAGES: usize = 40;
const LISTING_PATH: &str = "/whats-on/";
const EVENT_PREFIX: &str = "/events/";
const VENUE_NAME: &str = "The Horse Hospital";
const VENUE_ADDRESS: &str = "Colonnade, Bloomsbury, London WC1N 1JD";
/// The site category every upcoming event carries; not a tag.
const LISTING_CATEGORY: &str = "Upcoming";

pub struct HorseHospital {
    base_url: Url,
    max_detail_pages: usize,
}

impl HorseHospital {
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

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// The event page paths (`/events/<slug>`) linked from the listing, in page
/// order, de-duplicated. `page_url` is the address the listing was fetched
/// from; links must stay on its host.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<String> {
    let doc = Html::parse_document(html);
    let mut out: Vec<String> = Vec::new();
    for a in doc.select(&selector(
        ".summary-item-record-type-event a.summary-title-link[href]",
    )) {
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
    out
}

/// Parse an event page (fetched from `page_url`) into a [`RawEvent`]: its
/// JSON-LD `Event` plus the fields the markup lacks. `None` without one.
pub fn parse_detail(html: &str, page_url: &Url) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let event = extract_events(&doc).into_iter().next()?;
    let first_text = |s: &str| doc.select(&selector(s)).next().map(element_text);
    let blocks: Vec<ElementRef<'_>> = doc
        .select(&selector(".eventitem-column-content .sqs-html-content"))
        .collect();
    let admission = blocks.iter().skip(1).map(|b| element_text(*b)).find(|t| {
        let lower = t.to_lowercase();
        lower.starts_with("doors") || lower.contains("tickets")
    });
    let categories: Vec<String> = doc
        .select(&selector(".eventitem-meta-cats a"))
        .map(element_text)
        .filter(|c| !c.is_empty() && c != LISTING_CATEGORY)
        .collect();
    let id = page_url.path().strip_prefix(EVENT_PREFIX)?;
    Some(RawEvent {
        source_event_id: id.to_string(),
        source_url: Some(page_url.to_string()),
        payload: json!({
            "url": page_url.as_str(),
            "jsonld": event,
            "title": first_text("h1.eventitem-title"),
            "printed_start": first_text("time.event-time-24hr-start"),
            "lead": blocks.first().map(|b| b.inner_html()),
            "admission": admission,
            "categories": categories,
        }),
    })
}

/// Category from the site's own categories; `None` means out of scope.
pub fn category(site_categories: &[&str]) -> Option<Category> {
    let has = |name: &str| site_categories.iter().any(|c| c.eq_ignore_ascii_case(name));
    [
        ("Workshop", Category::Workshop),
        ("Talk", Category::Talk),
        ("Exhibition", Category::Exhibition),
    ]
    .into_iter()
    .find(|(name, _)| has(name))
    .map(|(_, c)| c)
}

/// A JSON-LD date with its offset (`2026-10-08T19:00:00+0100`).
fn parse_offset_datetime(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_str(s.trim(), "%Y-%m-%dT%H:%M:%S%z")
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Normalise a Horse Hospital [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let ev = payload
        .get("jsonld")
        .ok_or_else(|| SourceError::Parse("payload without jsonld".into()))?;
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event page without a title".into()))?;
    let site_categories: Vec<&str> = payload
        .get("categories")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(category) = category(&site_categories) else {
        return Ok(None);
    };

    let date = |key: &str| ev.get(key).and_then(Value::as_str);
    let starts_at = date("startDate")
        .and_then(parse_offset_datetime)
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no startDate")))?;
    let printed = text("printed_start")
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no printed start time")))?;
    let printed_start = NaiveTime::parse_from_str(printed, "%H:%M")
        .map(|t| london_to_utc(london_date(starts_at).and_time(t)))
        .map_err(|_| SourceError::Parse(format!("{title:?}: bad printed start {printed:?}")))?;
    if printed_start != starts_at {
        return Err(SourceError::Parse(format!(
            "{title:?}: printed start {printed:?} doesn't match {}",
            date("startDate").unwrap_or_default()
        )));
    }
    let ends_at = date("endDate")
        .and_then(parse_offset_datetime)
        .filter(|e| *e > starts_at);

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(text("lead")),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day: false,
        price: text("admission").map(parse_price).unwrap_or_default(),
        url: text("url").map(str::to_string),
        image_url: image_url(ev),
        category,
        tags: site_categories.iter().map(|c| c.to_lowercase()).collect(),
    }))
}

#[async_trait]
impl Source for HorseHospital {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listing_url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let listing = ctx.get_text(&listing_url).await?;
        let paths = parse_listing(&listing, &listing_url);
        if paths.is_empty() {
            return Err(SourceError::Parse(
                "no event links found on the listing page".into(),
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
            "Only talks, workshops and exhibitions: events whose site categories \
             include Talk, Workshop or Exhibition. Events listed only under Music, \
             Performance, Improvisation, Sound, Dance or Film (gigs, concerts, \
             screenings) are left out on purpose.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(start: &str, printed: &str, categories: &[&str]) -> Value {
        json!({
            "url": "https://www.thehorsehospital.com/events/x",
            "jsonld": {"@type": "Event", "startDate": start, "endDate": "2026-10-25T17:00:00+0000"},
            "title": "A Talk",
            "printed_start": printed,
            "categories": categories,
        })
    }

    #[test]
    fn honours_the_offset_across_the_clock_change() {
        let bst = normalise_payload(&payload("2026-10-08T19:00:00+0100", "19:00", &["Talk"]))
            .unwrap()
            .unwrap();
        assert_eq!(bst.starts_at.to_rfc3339(), "2026-10-08T18:00:00+00:00");
        assert!(!bst.all_day);
        let gmt = normalise_payload(&payload("2026-10-25T14:00:00+0000", "14:00", &["Talk"]))
            .unwrap()
            .unwrap();
        assert_eq!(gmt.starts_at.to_rfc3339(), "2026-10-25T14:00:00+00:00");
        assert_eq!(
            gmt.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-25T17:00:00+00:00")
        );
    }

    #[test]
    fn a_printed_start_that_disagrees_is_a_parse_error() {
        let err = normalise_payload(&payload("2026-10-08T19:00:00+0000", "19:00", &["Talk"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("doesn't match"), "{err}");
    }

    #[test]
    fn a_missing_or_malformed_printed_start_is_a_parse_error() {
        let mut missing = payload("2026-10-08T19:00:00+0100", "19:00", &["Talk"]);
        missing.as_object_mut().unwrap().remove("printed_start");
        let err = normalise_payload(&missing).unwrap_err().to_string();
        assert!(err.contains("no printed start"), "{err}");
        let err = normalise_payload(&payload("2026-10-08T19:00:00+0100", "7pm", &["Talk"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("bad printed start"), "{err}");
    }

    #[test]
    fn an_end_date_equal_to_the_start_is_dropped() {
        let ev = normalise_payload(&payload("2026-10-25T17:00:00+0000", "17:00", &["Talk"]))
            .unwrap()
            .unwrap();
        assert!(ev.ends_at.is_none());
    }

    #[test]
    fn categories_by_precedence_and_gigs_skipped() {
        assert_eq!(
            category(&["Music", "Talk", "Workshop"]),
            Some(Category::Workshop)
        );
        assert_eq!(
            category(&["Music", "Talk", "Exhibition"]),
            Some(Category::Talk)
        );
        assert_eq!(
            category(&["Exhibition", "Visual Art"]),
            Some(Category::Exhibition)
        );
        assert_eq!(
            category(&["Music", "Performance", "Improvisation", "Dance"]),
            None
        );
        assert!(
            normalise_payload(&payload("2026-10-08T19:00:00+0100", "19:00", &["Music"]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_qa_scope_names_the_skipped_site_categories() {
        let scope = HorseHospital::new(Url::parse("https://x.test/").unwrap())
            .qa_scope()
            .unwrap();
        for word in ["Talk", "Workshop", "Exhibition", "Music"] {
            assert!(scope.contains(word), "{word}");
        }
    }
}
