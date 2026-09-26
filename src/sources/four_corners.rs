//! Four Corners (Bethnal Green, E2) — listing-only CSS scraper over the
//! "What's on" page.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   allows `/`.
//! * There is no JSON-LD. `/whats-on/` lists every current and upcoming
//!   item as a `div[data-box-url]` card (the link is that attribute, not an
//!   `<a>`, pointing at `/whats-on/<slug>`) with type labels
//!   (`.event-type span`: "Exhibition", "Guest exhibition", "Talk",
//!   "Film screening", "Training", "Project", "Open call", …), a date line,
//!   the title (`h4`), a summary and an image. Detail pages add only longer
//!   prose (no price or other structured facts), so a run makes one request
//!   after robots.txt; the summary is the description.
//! * Date lines are either a date-only range
//!   (`Fri 2 October 2026 – Sat 31 October 2026`), stored as London
//!   midnight of the first and last day, or one date and a London
//!   wall-clock start time (`Wed 7 October 2026 | 18:30`). Anything else is
//!   a parse error.
//! * Category comes from the type labels: any exhibition label makes an
//!   exhibition (an exhibition's own talks and screenings are listed
//!   separately), else "Talk" makes a talk (talks with a screening
//!   included). Everything else (film screenings, courses, community
//!   projects, open calls, artist support) is skipped (`Ok(None)`).
//! * A "Coming soon..." title prefix is dropped.
//! * The site states no prices on the listing, so price is unknown.
//! * Every item is placed at the gallery.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc};

pub const KEY: &str = "four-corners";
const LISTING_PATH: &str = "/whats-on/";
const DETAIL_PREFIX: &str = "/whats-on/";
const VENUE_NAME: &str = "Four Corners";
const VENUE_ADDRESS: &str = "121 Roman Road, Bethnal Green, London E2 0QN";
/// The building's OpenStreetMap location.
const VENUE_LAT: f64 = 51.5289;
const VENUE_LNG: f64 = -0.0489;

pub struct FourCorners {
    base_url: Url,
}

impl FourCorners {
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

/// Extract the What's on cards, in document order, de-duplicated by slug.
/// `page_url` is the address the listing was fetched from.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<RawEvent> {
    let doc = Html::parse_document(html);
    let mut out: Vec<RawEvent> = Vec::new();
    for card in doc.select(&selector("div[data-box-url]")) {
        let Some(Ok(url)) = card.value().attr("data-box-url").map(|h| page_url.join(h)) else {
            continue;
        };
        let Some(slug) = url
            .path()
            .strip_prefix(DETAIL_PREFIX)
            .filter(|s| !s.is_empty() && !s.contains('/'))
        else {
            continue;
        };
        let content = |s: &str| {
            card.select(&selector(&format!(".event-content {s}")))
                .next()
                .map(element_text)
                .filter(|t| !t.is_empty())
        };
        let Some(title) = content("h4") else {
            continue;
        };
        if url.host() != page_url.host() || out.iter().any(|r| r.source_event_id == slug) {
            continue;
        }
        let types: Vec<String> = card
            .select(&selector(".event-type span"))
            .map(element_text)
            .filter(|t| !t.is_empty())
            .collect();
        let summary: Vec<String> = card
            .select(&selector(
                ".event-content > p:not(.event-type):not(.event-date)",
            ))
            .map(|p| p.inner_html())
            .collect();
        out.push(RawEvent {
            source_event_id: slug.to_string(),
            source_url: Some(url.to_string()),
            payload: json!({
                "url": url.as_str(),
                "title": title,
                "types": types,
                "date_text": content(".event-date"),
                "summary": (!summary.is_empty()).then(|| summary.join("\n")),
                "image_url": card
                    .select(&selector(".event-image img[src]"))
                    .next()
                    .and_then(|i| i.value().attr("src"))
                    .and_then(|src| page_url.join(src).ok())
                    .map(String::from),
            }),
        });
    }
    out
}

/// When an item happens, from its date line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// First and last day (equal for a single day), no time given.
    Days(NaiveDate, NaiveDate),
    /// A start date and London wall-clock time.
    Starts(NaiveDate, NaiveTime),
}

/// "Fri 2 October 2026" (weekday optional) → date.
fn parse_day(s: &str) -> Option<NaiveDate> {
    let mut tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens
        .first()
        .is_some_and(|t| t.chars().all(|c| c.is_ascii_alphabetic()))
    {
        tokens.remove(0);
    }
    NaiveDate::parse_from_str(&tokens.join(" "), "%d %B %Y").ok()
}

/// Parse a card's date line (see the module docs for the two forms).
pub fn parse_when(text: &str) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {text:?}"));
    let text = clean_text(text);
    if let Some((day, time)) = text.split_once('|') {
        let day = parse_day(day).ok_or_else(err)?;
        let time = NaiveTime::parse_from_str(time.trim(), "%H:%M").map_err(|_| err())?;
        return Ok(When::Starts(day, time));
    }
    let (first, last) = match text.split_once(['–', '—']) {
        Some((a, b)) => (parse_day(a), parse_day(b)),
        None => (parse_day(&text), parse_day(&text)),
    };
    match (first, last) {
        (Some(first), Some(last)) if first <= last => Ok(When::Days(first, last)),
        _ => Err(err()),
    }
}

/// Category from the card's type labels; `None` means out of scope.
pub fn category(types: &[String]) -> Option<Category> {
    let has = |want: &str| types.iter().any(|t| t.eq_ignore_ascii_case(want));
    if has("exhibition") || has("guest exhibition") {
        Some(Category::Exhibition)
    } else if has("talk") {
        Some(Category::Talk)
    } else {
        None
    }
}

fn strip_coming_soon(title: &str) -> &str {
    const PREFIX: &str = "coming soon";
    match title.get(..PREFIX.len()) {
        Some(p) if p.eq_ignore_ascii_case(PREFIX) => title[PREFIX.len()..]
            .trim_start_matches(['.', '…', ':', '-', '–', ' '])
            .trim(),
        _ => title,
    }
}

/// Normalise a Four Corners [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |k: &str| payload.get(k).and_then(Value::as_str);
    let types: Vec<String> = payload
        .get("types")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let Some(category) = category(&types) else {
        return Ok(None);
    };
    let title = text("title")
        .map(|t| strip_coming_soon(&clean_text(t)).to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("item without title".into()))?;
    let date_text = text("date_text")
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no date line")))?;
    let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
    let (starts_at, ends_at, all_day): (DateTime<Utc>, _, _) = match parse_when(date_text)? {
        When::Days(first, last) => (
            midnight(first),
            (last > first).then(|| midnight(last)),
            true,
        ),
        When::Starts(day, time) => (london_to_utc(day.and_time(time)), None, false),
    };

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(text("summary")),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day,
        price: Default::default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: vec!["photography".to_string()],
    }))
}

#[async_trait]
impl Source for FourCorners {
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
            return Err(SourceError::Parse(
                "no event cards found on the What's on page".into(),
            ));
        }
        Ok(items)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn types(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn parses_date_lines() {
        assert_eq!(
            parse_when(
                "Fri&nbsp;2&nbsp;October&nbsp;2026 &ndash; Sat&nbsp;31&nbsp;October&nbsp;2026"
            )
            .unwrap(),
            When::Days(d("2026-10-02"), d("2026-10-31"))
        );
        assert_eq!(
            parse_when("Sun 1 November 2026 – Wed 30 December 2026").unwrap(),
            When::Days(d("2026-11-01"), d("2026-12-30"))
        );
        assert_eq!(
            parse_when("Sat 3 October 2026").unwrap(),
            When::Days(d("2026-10-03"), d("2026-10-03"))
        );
        assert_eq!(
            parse_when("Wed 7 October 2026 | 18:30").unwrap(),
            When::Starts(d("2026-10-07"), NaiveTime::from_hms_opt(18, 30, 0).unwrap())
        );
    }

    #[test]
    fn unrecognised_date_lines_are_errors() {
        for text in [
            "Autumn 2026",
            "Sat 31 October 2026 – Fri 2 October 2026",
            "Wed 7 October 2026 | 6.30pm",
            "7 October",
            "",
        ] {
            assert!(parse_when(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn exhibition_labels_win_then_talks() {
        assert_eq!(
            category(&types(&["Exhibition", "Talk", "Film screening"])),
            Some(Category::Exhibition)
        );
        assert_eq!(
            category(&types(&["Guest exhibition"])),
            Some(Category::Exhibition)
        );
        assert_eq!(
            category(&types(&["Talk", "Film screening"])),
            Some(Category::Talk)
        );
        for labels in [
            &["Film screening"][..],
            &["Training", "Project"],
            &["Open call", "Training", "Project"],
            &["Artist support"],
            &[],
        ] {
            assert_eq!(category(&types(labels)), None, "{labels:?}");
        }
    }

    #[test]
    fn drops_coming_soon_prefix() {
        assert_eq!(
            strip_coming_soon("Coming soon... Hate Uprooted Diasporic Season"),
            "Hate Uprooted Diasporic Season"
        );
        assert_eq!(strip_coming_soon("Coming soon: Show"), "Show");
        assert_eq!(
            strip_coming_soon("Solidarity on the Streets"),
            "Solidarity on the Streets"
        );
    }

    #[test]
    fn timed_talk_is_not_all_day() {
        let event = normalise_payload(&json!({
            "title": "Battle for the East End",
            "types": ["Talk"],
            "date_text": "Wed 7 October 2026 | 18:30",
        }))
        .unwrap()
        .unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-07T17:30:00+00:00");
        assert_eq!(event.ends_at, None);
        assert!(!event.all_day);
    }

    #[test]
    fn single_day_exhibition_is_all_day_without_end() {
        let event = normalise_payload(&json!({
            "title": "One day only",
            "types": ["Exhibition"],
            "date_text": "Sat 3 October 2026",
        }))
        .unwrap()
        .unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-02T23:00:00+00:00");
        assert_eq!(event.ends_at, None);
        assert!(event.all_day);
    }

    #[test]
    fn missing_date_is_an_error() {
        assert!(normalise_payload(&json!({"title": "No date", "types": ["Talk"]})).is_err());
    }
}
