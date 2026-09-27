//! London Review Bookshop (Bloomsbury, WC1A) — listing-only scraper over
//! the Events page.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   allows `/` except `/account/`.
//! * There is no JSON-LD. `/events` marks each upcoming event up as a
//!   microdata `section.event-preview[itemtype="http://schema.org/Event"]`
//!   (a featured one first, then the `.event-lines` list) with a date line
//!   (`.event-preview--date`), the title (`h2.event-preview--title`), a
//!   price (`.event-preview--price`: "£10.00", "FREE", "Sold out"), an
//!   optional subtitle (`.event-preview--desc`, usually the book) and an
//!   image. The same page lists recorded talks as
//!   `schema.org/PodcastEpisode` previews; those are not events and are
//!   never read. Each event links only to its Eventbrite booking page, which
//!   is not the venue's page and which we never fetch, so a run makes one
//!   request after robots.txt and the link is the Events page itself (as
//!   for October Gallery); the Eventbrite id is the stable event id.
//! * Date lines carry no year: "Tuesday 29 September, 7 p.m.". The year is
//!   resolved against the London date of the fetch (`listed_on`, see
//!   [`infer_date`]) and must agree with the printed weekday; a mismatch is
//!   a parse error rather than a guess. Times are London wall-clock. A line
//!   without a time would be a single all-day date (none on the page
//!   today).
//! * Events are author talks held in the shop ("we turn our shop into a
//!   miniature auditorium"); the page names no other venue, so every event
//!   is placed at the shop. Retail evenings ("Late Night Shopping") and film
//!   screenings are skipped (`Ok(None)`); everything else is a talk.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime, Utc, Weekday};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::{infer_date, parse_time_range};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, parse_price,
};

pub const KEY: &str = "london-review-bookshop";
const EVENTS_PATH: &str = "/events";
const VENUE_NAME: &str = "London Review Bookshop";
const VENUE_ADDRESS: &str = "14 Bury Place, London WC1A 2JL";
/// The shop's OpenStreetMap location.
const VENUE_LAT: f64 = 51.5184;
const VENUE_LNG: f64 = -0.1245;
/// Lower-cased title phrases marking items that are not talks.
const SKIP_PHRASES: [&str; 3] = ["late night shopping", "screening", "film night"];

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

pub struct LondonReviewBookshop {
    base_url: Url,
}

impl LondonReviewBookshop {
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

/// The Eventbrite event id at the end of a booking link
/// (`…/e/some-title-tickets-1995703170907`).
fn eventbrite_id(link: &Url) -> Option<String> {
    let host = link.host_str()?;
    if !(host == "eventbrite.co.uk" || host.ends_with(".eventbrite.co.uk")) {
        return None;
    }
    let id = link.path().trim_end_matches('/').rsplit('-').next()?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then(|| id.to_string())
}

/// Extract the Events page's event previews, in document order,
/// de-duplicated by Eventbrite id. `page_url` is the address the page was
/// fetched from (it is each event's link); `listed_on` is the London date of
/// the fetch, kept for year inference.
pub fn parse_listing(html: &str, page_url: &Url, listed_on: NaiveDate) -> Vec<RawEvent> {
    let doc = Html::parse_document(html);
    let mut out: Vec<RawEvent> = Vec::new();
    for card in doc.select(&selector("section.event-preview[itemtype]")) {
        let is_event = card
            .value()
            .attr("itemtype")
            .is_some_and(|t| t.trim_end_matches('/').ends_with("schema.org/Event"));
        if !is_event {
            continue;
        }
        let first = |s: &str| card.select(&selector(s)).next();
        let text = |s: &str| first(s).map(element_text).filter(|t| !t.is_empty());
        let Some(title) = text(".event-preview--title") else {
            continue;
        };
        let Some(id) = first("a.event-preview--copy[href]")
            .and_then(|a| a.value().attr("href"))
            .and_then(|h| page_url.join(h).ok())
            .and_then(|link| eventbrite_id(&link))
        else {
            continue;
        };
        if out.iter().any(|r| r.source_event_id == id) {
            continue;
        }
        out.push(RawEvent {
            source_event_id: id,
            source_url: Some(page_url.to_string()),
            payload: json!({
                "url": page_url.as_str(),
                "title": title,
                "date_text": text(".event-preview--date"),
                "price_text": text(".event-preview--price"),
                "subtitle": first(".event-preview--desc").map(|d| d.inner_html()),
                "image_url": first("img[itemprop=image][src]")
                    .and_then(|i| i.value().attr("src"))
                    .and_then(|src| page_url.join(src).ok())
                    .map(String::from),
                "listed_on": listed_on.to_string(),
            }),
        });
    }
    out
}

/// When an event happens, from its date line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// A single day, no time given.
    Day(NaiveDate),
    /// A day, London wall-clock start and optional end.
    Starts(NaiveDate, NaiveTime, Option<NaiveTime>),
}

/// "Tuesday 29 September" (year optional) → date, checked against the
/// weekday when one is printed.
fn parse_day(text: &str, listed_on: NaiveDate) -> Option<NaiveDate> {
    let lower = text.to_lowercase();
    let mut tokens: Vec<&str> = lower.split_whitespace().collect();
    let weekday = match tokens.first().map(|t| t.parse::<Weekday>()) {
        Some(Ok(w)) => {
            tokens.remove(0);
            Some(w)
        }
        _ => None,
    };
    let (day, month, year) = match tokens.as_slice() {
        [d, m] => (d, m, None),
        [d, m, y] => (d, m, Some(y.parse::<i32>().ok()?)),
        _ => return None,
    };
    let day: u32 = day
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .parse()
        .ok()?;
    let month = MONTHS.iter().position(|m| m == month)? as u32 + 1;
    let date = match year {
        Some(y) => NaiveDate::from_ymd_opt(y, month, day),
        None => infer_date(month, day, listed_on),
    }?;
    weekday.is_none_or(|w| date.weekday() == w).then_some(date)
}

/// Parse a date line: "Tuesday 29 September, 7 p.m.", "Saturday 3
/// October, 11 a.m. – 12.30 p.m." or a bare day.
pub fn parse_when(text: &str, listed_on: NaiveDate) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {text:?}"));
    let text = clean_text(text);
    let (day, time) = match text.split_once(',') {
        Some((d, t)) => (d, Some(t)),
        None => (text.as_str(), None),
    };
    let day = parse_day(day, listed_on).ok_or_else(err)?;
    let Some(time) = time else {
        return Ok(When::Day(day));
    };
    let clock: String = time
        .to_lowercase()
        .replace("a.m.", "am")
        .replace("p.m.", "pm")
        .split_whitespace()
        .collect();
    let (start, end) = parse_time_range(&clock).ok_or_else(err)?;
    Ok(When::Starts(day, start, end))
}

/// Category of an event; `None` means out of scope.
pub fn category(title: &str) -> Option<Category> {
    let lower = title.to_lowercase();
    (!SKIP_PHRASES.iter().any(|p| lower.contains(p))).then_some(Category::Talk)
}

/// Normalise a London Review Bookshop [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event without title".into()))?;
    let Some(category) = category(&title) else {
        return Ok(None);
    };
    let listed_on = text("listed_on")
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no listed_on")))?;
    let date_text = text("date_text")
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no date line")))?;
    let (starts_at, ends_at, all_day) = match parse_when(date_text, listed_on)? {
        When::Day(day) => (london_to_utc(day.and_time(NaiveTime::MIN)), None, true),
        When::Starts(day, start, end) => (
            london_to_utc(day.and_time(start)),
            end.filter(|e| *e > start)
                .map(|e| london_to_utc(day.and_time(e))),
            false,
        ),
    };

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(text("subtitle")),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day,
        price: text("price_text").map(parse_price).unwrap_or_default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: vec!["books".to_string()],
    }))
}

#[async_trait]
impl Source for LondonReviewBookshop {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(EVENTS_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let items = parse_listing(&ctx.get_text(&url).await?, &url, london_date(Utc::now()));
        if items.is_empty() {
            return Err(SourceError::Parse(
                "no event previews found on the Events page".into(),
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

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn parses_date_lines() {
        let listed_on = d("2026-09-27");
        let when = |s: &str| parse_when(s, listed_on).unwrap();
        assert_eq!(
            when("Tuesday 29 September, 7 p.m."),
            When::Starts(d("2026-09-29"), t(19, 0), None)
        );
        assert_eq!(
            when("Thursday 5 November, 6.30 p.m."),
            When::Starts(d("2026-11-05"), t(18, 30), None)
        );
        assert_eq!(
            when("Saturday 3 October, 11 a.m. – 12.30 p.m."),
            When::Starts(d("2026-10-03"), t(11, 0), Some(t(12, 30)))
        );
        assert_eq!(when("Saturday 3 October"), When::Day(d("2026-10-03")));
    }

    #[test]
    fn year_rolls_over_and_must_match_the_weekday() {
        // Late in the year, January is next year's.
        assert_eq!(
            parse_when("Wednesday 13 January, 7 p.m.", d("2026-11-20")).unwrap(),
            When::Starts(d("2027-01-13"), t(19, 0), None)
        );
        // 13 January 2027 is a Wednesday, not a Tuesday: refuse to guess.
        assert!(parse_when("Tuesday 13 January, 7 p.m.", d("2026-11-20")).is_err());
    }

    #[test]
    fn unrecognised_date_lines_are_errors() {
        for text in [
            "",
            "Autumn",
            "Tuesday 29 Septembre, 7 p.m.",
            "Tuesday 29 September, doors open",
            "Tuesday 31 September, 7 p.m.",
        ] {
            assert!(parse_when(text, d("2026-09-27")).is_err(), "{text:?}");
        }
    }

    #[test]
    fn talks_unless_shopping_or_screening() {
        assert_eq!(
            category("Anton Jäger & William Davies: Hyperpolitics"),
            Some(Category::Talk)
        );
        assert_eq!(category("October Late Night Shopping"), None);
        assert_eq!(category("Screening: Blue"), None);
    }

    #[test]
    fn timed_talk_is_not_all_day() {
        let event = normalise_payload(&json!({
            "title": "Hyperpolitics",
            "date_text": "Wednesday 14 October, 7 p.m.",
            "price_text": "£10.00",
            "listed_on": "2026-09-27",
        }))
        .unwrap()
        .unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-14T18:00:00+00:00");
        assert_eq!(event.ends_at, None);
        assert!(!event.all_day);
    }

    #[test]
    fn untimed_day_is_all_day_at_london_midnight() {
        let event = normalise_payload(&json!({
            "title": "Poetry day",
            "date_text": "Saturday 3 October",
            "listed_on": "2026-09-27",
        }))
        .unwrap()
        .unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-02T23:00:00+00:00");
        assert_eq!(event.ends_at, None);
        assert!(event.all_day);
    }

    #[test]
    fn eventbrite_ids() {
        let id = |s: &str| eventbrite_id(&Url::parse(s).unwrap());
        assert_eq!(
            id("https://www.eventbrite.co.uk/e/some-talk-tickets-1995703170907").as_deref(),
            Some("1995703170907")
        );
        assert_eq!(id("https://elsewhere.example/e/talk-tickets-123"), None);
        assert_eq!(id("https://www.eventbrite.co.uk/e/no-id-here"), None);
    }

    #[test]
    fn listing_skips_podcasts_and_repeats() {
        let card = |itemtype: &str, href: &str, title: &str| {
            format!(
                r#"<section class="event-preview" itemtype="{itemtype}"><a class="event-preview--copy" href="{href}"><span class="event-preview--date">Tuesday 29 September, 7 p.m.</span><h2 class="event-preview--title">{title}</h2></a></section>"#
            )
        };
        let eb = "https://www.eventbrite.co.uk/e/talk-tickets-42";
        let html = [
            card(
                "https://schema.org/PodcastEpisode",
                "/podcasts/x",
                "Podcast",
            ),
            card("http://schema.org/Event", eb, "First"),
            card("http://schema.org/Event", eb, "Second"),
            card("http://schema.org/Event", "/elsewhere", "No booking link"),
        ]
        .concat();
        let page_url = Url::parse("https://www.londonreviewbookshop.co.uk/events").unwrap();
        let listing = parse_listing(&html, &page_url, d("2026-09-27"));
        let kept: Vec<_> = listing
            .iter()
            .map(|r| (r.source_event_id.as_str(), r.payload["title"].as_str()))
            .collect();
        assert_eq!(kept, [("42", Some("First"))]);
    }

    #[test]
    fn missing_date_is_an_error() {
        assert!(
            normalise_payload(&json!({"title": "No date", "listed_on": "2026-09-27"})).is_err()
        );
    }
}
