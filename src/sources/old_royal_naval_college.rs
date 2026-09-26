//! Old Royal Naval College (Greenwich) — hand-written scraper over the
//! "What's on" listing and the detail pages of dated items.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   with an empty `Disallow:` and no Crawl-delay, so everything is allowed.
//! * There is no JSON-LD `Event` (only Yoast's Article/WebPage graph), so CSS
//!   selectors are used. `/whats-on/` shows 18 cards (`section.news
//!   .news__item`) and a "Load more" button (`#loadmore_events`) while more
//!   exist; the next batch is server-rendered at `/whats-on/page/N/`, and the
//!   button is absent on the last page. A "Featured" block above the grid
//!   repeats cards and is ignored; cards are de-duplicated by path. Links are
//!   absolute `https://ornc.org/whats-on/<slug>/` URLs; only their path is
//!   kept and joined onto the configured base URL.
//! * Each card has a label (`.heading--sm`: "Exhibition", "Film and TV",
//!   "After Hours", …), a title and a `<strong>` schedule line, `dates | times`
//!   ("Tue 6 Oct | 6pm-8.30pm", "20 Nov 2026 - 7 Feb 2027"). Most cards are
//!   recurring programmes ("Daily | 10am–5pm", "Every Monday", "Various dates
//!   in 2026") or lists of sessions ("6–8 & 11–15 Nov", "Sat 3 Apr & Sun 4 Apr
//!   2027"): no month name, or an `&`/`,`, means a skip. Dates usually have
//!   no year: `fetch` records the London date of the run (`listed_on`) and
//!   [`parse_date_range`] (shared with Chisenhale Gallery) resolves the year
//!   against it. Anything but an exhibition that spans more than one day is
//!   a run of sessions and skipped too.
//! * Times are London wall-clock. "Various times" means no time (the item
//!   is stored `all_day`); of several sessions ("7pm & 8.30pm") the first is
//!   the start. Exhibitions are dates only, stored as London midnight of
//!   their first and last day (`all_day`).
//! * The detail page of every dated card is fetched for the price (the
//!   "Tickets: …" line), the description (`.event__copy`) and the page
//!   `<title>`. Category: the "Exhibition" label → exhibition; otherwise
//!   `map_category` over the card title and the page title ("Film Talk
//!   Adjani Salmon | Event | …" → talk); concerts, performances, murder
//!   mysteries and family days match nothing and are skipped.
//! * Every item is placed at the College.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::{parse_date_range, parse_time_range};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, map_category,
    parse_price,
};

pub const KEY: &str = "old-royal-naval-college";
/// Upper bound on listing pages fetched per run.
pub const MAX_LISTING_PAGES: usize = 5;
/// Upper bound on detail pages fetched per run (≈ 40 s at 1 req / 2 s);
/// dated cards beyond it are left out of the run.
pub const MAX_DETAIL_PAGES: usize = 20;
const LISTING_PATH: &str = "/whats-on/";
const SITE_HOST: &str = "ornc.org";
const VENUE_NAME: &str = "Old Royal Naval College";
const VENUE_ADDRESS: &str = "King William Walk, London SE10 9NN";
/// The Painted Hall (OSM).
const VENUE_LAT: f64 = 51.4829;
const VENUE_LNG: f64 = -0.0066;
const EXHIBITION_LABEL: &str = "Exhibition";
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

pub struct OldRoyalNavalCollege {
    base_url: Url,
}

impl OldRoyalNavalCollege {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// One listing card, as printed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Card {
    /// `/whats-on/<slug>/`.
    pub path: String,
    pub title: String,
    pub label: Option<String>,
    /// The schedule line, e.g. "Tue 6 Oct | 6pm-8.30pm".
    pub when: Option<String>,
    pub image_url: Option<String>,
}

/// One page of the listing.
#[derive(Debug, serde::Serialize)]
pub struct Listing {
    pub cards: Vec<Card>,
    /// Whether the page offers "Load more" (a further `/whats-on/page/N/`).
    pub has_more: bool,
    /// Cards that could not be read, for the fetch to report.
    pub problems: Vec<String>,
}

/// What a detail page adds to its card.
#[derive(Debug, serde::Serialize)]
pub struct Detail {
    pub page_title: Option<String>,
    pub price_text: Option<String>,
    pub description: Option<String>,
}

/// The first and last day of an item and, if given, its start and end time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub first: NaiveDate,
    pub last: NaiveDate,
    pub times: Option<(NaiveTime, Option<NaiveTime>)>,
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// The `/whats-on/<slug>/` path of an on-site event link.
fn event_path(href: &str) -> Option<String> {
    let u = Url::parse(href).ok()?;
    let host = u.host_str()?;
    if host.trim_start_matches("www.") != SITE_HOST || u.query().is_some() {
        return None;
    }
    let slug = u.path().strip_prefix(LISTING_PATH)?.trim_end_matches('/');
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    valid.then(|| format!("{LISTING_PATH}{slug}/"))
}

fn parse_card(item: ElementRef<'_>) -> Result<Card, String> {
    let first = |s: &str| item.select(&selector(s)).next();
    let first_text = |s: &str| first(s).map(element_text).filter(|t| !t.is_empty());
    let title = first_text("h3.text-listing");
    let href = first(".news__details a[href]")
        .and_then(|a| a.value().attr("href"))
        .unwrap_or_default();
    let (Some(path), Some(title)) = (event_path(href), title.clone()) else {
        return Err(format!(
            "unreadable card {:?} ({href:?})",
            title.unwrap_or_default()
        ));
    };
    Ok(Card {
        path,
        title,
        label: first_text(".heading--sm"),
        when: first_text(".news__details > strong"),
        image_url: first("a.news__img[data-back]")
            .and_then(|e| e.value().attr("data-back"))
            .map(str::to_string),
    })
}

pub fn parse_listing(html: &str) -> Listing {
    let doc = Html::parse_document(html);
    let mut cards = Vec::new();
    let mut problems = Vec::new();
    for item in doc.select(&selector("section.news .news__item")) {
        match parse_card(item) {
            Ok(card) => cards.push(card),
            Err(problem) => problems.push(problem),
        }
    }
    Listing {
        cards,
        has_more: doc.select(&selector("#loadmore_events")).next().is_some(),
        problems,
    }
}

pub fn parse_detail(html: &str) -> Detail {
    let doc = Html::parse_document(html);
    let price_text = doc
        .select(&selector(".event__book p strong"))
        .map(element_text)
        .find(|t| t.starts_with("Tickets"));
    Detail {
        page_title: doc
            .select(&selector("title"))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty()),
        price_text,
        description: doc
            .select(&selector(".event__copy"))
            .next()
            .map(text_outside_noscript)
            .filter(|t| !t.is_empty()),
    }
}

/// The text of an element, leaving out `<noscript>` fallbacks: the
/// lazy-loaded images inside the copy repeat themselves there, and
/// `<noscript>` content parses as raw markup.
fn text_outside_noscript(e: ElementRef<'_>) -> String {
    let parts: Vec<&str> = e
        .descendants()
        .filter(|n| {
            !n.ancestors().any(|a| {
                a.value()
                    .as_element()
                    .is_some_and(|el| el.name() == "noscript")
            })
        })
        .filter_map(|n| n.value().as_text().map(|t| &**t))
        .collect();
    clean_text(&parts.join(" "))
}

/// Whether the text has a month's name, in full or cut short ("Oct",
/// "Sept").
fn names_a_month(text: &str) -> bool {
    text.split(|c: char| !c.is_alphabetic()).any(|w| {
        let w = w.to_lowercase();
        w.len() >= 3 && MONTHS.iter().any(|m| m.starts_with(&w))
    })
}

/// Parse a card's schedule line. `Ok(None)` for recurring programmes and
/// lists of sessions (see the module docs); an error for a date that can't
/// be read.
pub fn parse_when(text: &str, listed_on: NaiveDate) -> Result<Option<Schedule>, SourceError> {
    let (date_part, time_part) = match text.split_once('|') {
        Some((d, t)) => (d.trim(), Some(t.trim())),
        None => (text.trim(), None),
    };
    if date_part.contains(['&', ',']) || !names_a_month(date_part) {
        return Ok(None);
    }
    let Some((first, last)) = parse_date_range(date_part, listed_on)? else {
        return Ok(None);
    };
    let times = match time_part {
        Some(t) if !t.to_lowercase().starts_with("various") => {
            let first_session = t.split('&').next().unwrap_or(t);
            Some(parse_time_range(first_session).ok_or_else(|| {
                SourceError::Parse(format!("unrecognised time {t:?} in {text:?}"))
            })?)
        }
        _ => None,
    };
    Ok(Some(Schedule { first, last, times }))
}

fn is_exhibition(label: Option<&str>) -> bool {
    label.is_some_and(|l| l.eq_ignore_ascii_case(EXHIBITION_LABEL))
}

/// A card's schedule if it is one item on one day, or an exhibition;
/// `Ok(None)` to skip it.
fn dated(
    label: Option<&str>,
    when: Option<&str>,
    listed_on: NaiveDate,
) -> Result<Option<Schedule>, SourceError> {
    let Some(schedule) = when
        .map(|w| parse_when(w, listed_on))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    if is_exhibition(label) {
        return Ok(Some(Schedule {
            times: None,
            ..schedule
        }));
    }
    Ok((schedule.first == schedule.last).then_some(schedule))
}

/// Whether a card is dated (worth its detail page), or an error for a
/// schedule line that can't be read.
pub fn is_dated(card: &Card, listed_on: NaiveDate) -> Result<bool, SourceError> {
    dated(card.label.as_deref(), card.when.as_deref(), listed_on).map(|s| s.is_some())
}

/// The in-scope category of an item, or `None` to skip it.
pub fn category(label: Option<&str>, title: &str, page_title: Option<&str>) -> Option<Category> {
    if is_exhibition(label) {
        return Some(Category::Exhibition);
    }
    map_category(
        &[Some(title), page_title]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
    )
}

/// The [`RawEvent`] for a card, with its detail page if one was fetched.
/// `listed_on` is the London date of the fetch, for year inference.
pub fn card_event(
    card: &Card,
    base: &Url,
    listed_on: NaiveDate,
    detail: Option<&Detail>,
) -> RawEvent {
    let url = base
        .join(&card.path)
        .map(String::from)
        .unwrap_or_else(|_| format!("https://{SITE_HOST}{}", card.path));
    let mut payload = serde_json::to_value(card).expect("card serialises");
    payload["url"] = json!(url);
    payload["listed_on"] = json!(listed_on.to_string());
    if let Some(Value::Object(detail)) = detail.map(|d| serde_json::to_value(d).expect("detail")) {
        payload.as_object_mut().expect("object").extend(detail);
    }
    RawEvent {
        source_event_id: card.path.trim_matches('/').to_string(),
        source_url: Some(url),
        payload,
    }
}

/// Normalise an Old Royal Naval College [`RawEvent`] payload (a card plus
/// its detail page).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty())
    };
    let title = text("title")
        .map(clean_text)
        .ok_or_else(|| SourceError::Parse("card without title".into()))?;
    let listed_on = text("listed_on")
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .ok_or_else(|| SourceError::Parse("payload without listed_on date".into()))?;
    let label = text("label");
    let Some(schedule) = dated(label, text("when"), listed_on)
        .map_err(|e| SourceError::Parse(format!("{title:?}: {e}")))?
    else {
        return Ok(None);
    };
    let Some(category) = category(label, &title, text("page_title")) else {
        return Ok(None);
    };

    let (starts_at, ends_at) = match schedule.times {
        Some((start, end)) => (
            london_to_utc(schedule.first.and_time(start)),
            end.map(|e| london_to_utc(schedule.last.and_time(e))),
        ),
        None => (
            london_to_utc(schedule.first.and_time(NaiveTime::MIN)),
            Some(london_to_utc(schedule.last.and_time(NaiveTime::MIN))),
        ),
    };
    let ends_at = ends_at.filter(|e| *e > starts_at);

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(text("description")),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day: schedule.times.is_none(),
        price: text("price_text").map(parse_price).unwrap_or_default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: Vec::new(),
    }))
}

#[async_trait]
impl Source for OldRoyalNavalCollege {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let join = |path: &str| {
            self.base_url
                .join(path)
                .map_err(|e| SourceError::Config(e.to_string()))
        };
        let listed_on = london_date(Utc::now());
        let mut cards: Vec<Card> = Vec::new();
        for page in 1..=MAX_LISTING_PAGES {
            let path = if page == 1 {
                LISTING_PATH.to_string()
            } else {
                format!("{LISTING_PATH}page/{page}/")
            };
            let listing = parse_listing(&ctx.get_text(&join(&path)?).await?);
            if page == 1 && listing.cards.is_empty() {
                return Err(SourceError::Parse(format!(
                    "no event cards on {LISTING_PATH}"
                )));
            }
            for problem in listing.problems {
                ctx.report_error(problem);
            }
            for card in listing.cards {
                if !cards.iter().any(|c| c.path == card.path) {
                    cards.push(card);
                }
            }
            if !listing.has_more {
                break;
            }
        }

        let mut out = Vec::new();
        let mut details = 0;
        for card in &cards {
            // A card whose schedule can't be read is kept so that normalise
            // reports it.
            if !matches!(is_dated(card, listed_on), Ok(true)) {
                out.push(card_event(card, &self.base_url, listed_on, None));
                continue;
            }
            if details == MAX_DETAIL_PAGES {
                continue;
            }
            details += 1;
            let result = match join(&card.path) {
                Ok(url) => ctx.get_text(&url).await.map_err(SourceError::from),
                Err(e) => Err(e),
            };
            match result {
                Ok(html) => out.push(card_event(
                    card,
                    &self.base_url,
                    listed_on,
                    Some(&parse_detail(&html)),
                )),
                Err(e) => ctx.report_error(format!("{}: {e}", card.path)),
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

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    const LISTED_ON: &str = "2026-09-26";

    /// (first, last, start, end) as strings, or `None` for a skip.
    fn when(text: &str) -> Option<(String, String, Option<String>, Option<String>)> {
        parse_when(text, d(LISTED_ON)).unwrap().map(|s| {
            (
                s.first.to_string(),
                s.last.to_string(),
                s.times.map(|(a, _)| a.to_string()),
                s.times.and_then(|(_, b)| b).map(|b| b.to_string()),
            )
        })
    }

    fn at(
        first: &str,
        last: &str,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Option<(String, String, Option<String>, Option<String>)> {
        Some((
            first.into(),
            last.into(),
            start.map(str::to_string),
            end.map(str::to_string),
        ))
    }

    #[test]
    fn every_schedule_line_on_the_listing() {
        // Recurring programmes and lists of sessions are skips.
        for text in [
            "Every Monday | 10am–11am",
            "First Sunday of the month | 10am–5pm",
            "Saturdays | 11.30am & 1.30pm",
            "Daily | 11am, 12pm, 1pm, 2pm & 3pm",
            "Daily | Various times",
            "Open Daily | 10am–5pm",
            "Sundays | 11.30am",
            "Last Friday of the month | 8am–8.45am",
            "Mon & Fri | 11.30am & 1.30pm",
            "Daily | 10am–5pm",
            "Various dates in 2026 | 6.30pm & 8.30pm",
            "Third Sunday of the month | 11am-4pm",
            "Various dates | 8pm–11pm",
            "6–8 & 11–15 Nov | 5.45pm-9pm",
            "1 Dec–3 Dec, 8 Dec–10 Dec & 15 Dec–17 Dec| Various times",
            "Sat 3 Apr & Sun 4 Apr 2027 | 10am-5pm",
        ] {
            assert_eq!(when(text), None, "{text}");
        }
        assert_eq!(
            when("Tue 6 Oct | 6pm-8.30pm"),
            at(
                "2026-10-06",
                "2026-10-06",
                Some("18:00:00"),
                Some("20:30:00")
            )
        );
        assert_eq!(
            when("Sat 31 Oct | 6.30pm-7.45pm"),
            at(
                "2026-10-31",
                "2026-10-31",
                Some("18:30:00"),
                Some("19:45:00")
            )
        );
        assert_eq!(
            when("Thu 26 Nov | 1pm-10pm"),
            at(
                "2026-11-26",
                "2026-11-26",
                Some("13:00:00"),
                Some("22:00:00")
            )
        );
        // Of several sessions, the first.
        assert_eq!(
            when("Sat 12 Dec | 7pm & 8.30pm"),
            at("2026-12-12", "2026-12-12", Some("19:00:00"), None)
        );
        assert_eq!(
            when("Sun 2 May 2027 | 10am–6pm"),
            at(
                "2027-05-02",
                "2027-05-02",
                Some("10:00:00"),
                Some("18:00:00")
            )
        );
        assert_eq!(
            when("20 Nov 2026 - 7 Feb 2027"),
            at("2026-11-20", "2027-02-07", None, None)
        );
        // Ranges parse; `dated` later drops the ones that aren't exhibitions.
        assert_eq!(
            when("Sat 28 Nov– Tue 22 Dec | Various times"),
            at("2026-11-28", "2026-12-22", None, None)
        );
        assert_eq!(
            when("Sat 19 - Wed 23 Dec 2026 | Various Times"),
            at("2026-12-19", "2026-12-23", None, None)
        );
        assert_eq!(
            when("Sat 28 Aug - Mon 30 Aug 2027 | 10am-5pm"),
            at(
                "2027-08-28",
                "2027-08-30",
                Some("10:00:00"),
                Some("17:00:00")
            )
        );
    }

    #[test]
    fn unreadable_dates_and_times_are_errors() {
        for text in [
            "31 Feb | 7pm",
            "Tue 6 Oct | at dusk",
            "6 Oct 2026 - 2 Oct 2026",
        ] {
            assert!(parse_when(text, d(LISTED_ON)).is_err(), "{text}");
        }
    }

    #[test]
    fn only_exhibitions_span_several_days() {
        let listed_on = d(LISTED_ON);
        let range = Some("Sat 28 Aug - Mon 30 Aug 2027 | 10am-5pm");
        assert_eq!(dated(Some("Family Fun"), range, listed_on).unwrap(), None);
        let exhibition = dated(Some("Exhibition"), range, listed_on)
            .unwrap()
            .unwrap();
        assert_eq!(exhibition.times, None);
        assert!(
            dated(Some("After Hours"), Some("Tue 6 Oct | 6pm"), listed_on)
                .unwrap()
                .is_some()
        );
        assert_eq!(dated(Some("Exhibition"), None, listed_on).unwrap(), None);
    }

    #[test]
    fn category_rules() {
        assert_eq!(
            category(Some("Exhibition"), "Peace Doves", None),
            Some(Category::Exhibition)
        );
        assert_eq!(
            category(
                Some("Film and TV"),
                "The Art of Directing",
                Some("Film Talk Adjani Salmon | Event | Old Royal Naval College")
            ),
            Some(Category::Talk)
        );
        assert_eq!(
            category(
                Some("After Hours"),
                "Murder Mystery Halloween Edition",
                Some("Murder Mystery Halloween Edition - Old Royal Naval College")
            ),
            None
        );
    }

    fn payload(label: &str, when: &str) -> Value {
        json!({
            "path": "/whats-on/x/",
            "title": "A Talk",
            "label": label,
            "when": when,
            "url": "https://ornc.org/whats-on/x/",
            "listed_on": LISTED_ON,
            "price_text": "Tickets: £25 (Concessions £20)",
        })
    }

    #[test]
    fn times_are_london_wall_clock() {
        let e = normalise_payload(&payload("Film and TV", "Tue 6 Oct | 6pm-8.30pm"))
            .unwrap()
            .unwrap();
        assert_eq!(e.category, Category::Talk);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-06T17:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-06T19:30:00+00:00");
        assert!(!e.all_day);
        // After the clocks go back: GMT.
        let e = normalise_payload(&payload("Film and TV", "Thu 26 Nov | 1pm-10pm"))
            .unwrap()
            .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-11-26T13:00:00+00:00");
    }

    #[test]
    fn untimed_days_and_exhibitions_are_all_day() {
        let e = normalise_payload(&payload("Film and TV", "Tue 6 Oct | Various times"))
            .unwrap()
            .unwrap();
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-05T23:00:00+00:00");
        assert_eq!(e.ends_at, None);
        let e = normalise_payload(&payload(
            "Exhibition",
            "20 Nov 2026 - 7 Feb 2027 | 10am-5pm",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.category, Category::Exhibition);
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-11-20T00:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2027-02-07T00:00:00+00:00");
    }

    #[test]
    fn skips_and_errors() {
        assert!(
            normalise_payload(&payload("Film and TV", "Daily | 10am–5pm"))
                .unwrap()
                .is_none()
        );
        assert!(normalise_payload(&payload("Film and TV", "Tue 6 Oct | at dusk")).is_err());
    }

    #[test]
    fn only_on_site_event_links_are_cards() {
        assert_eq!(
            event_path("https://ornc.org/whats-on/peace-doves-by-peter-walker/").as_deref(),
            Some("/whats-on/peace-doves-by-peter-walker/")
        );
        assert_eq!(
            event_path("https://www.ornc.org/whats-on/x").as_deref(),
            Some("/whats-on/x/")
        );
        for href in [
            "https://ornc.org/whats-on/",
            "https://elsewhere.example/whats-on/x/",
            "https://ornc.org/news/x/",
            "https://ornc.org/whats-on/x/?utm=1",
            "/whats-on/x/",
        ] {
            assert_eq!(event_path(href), None, "{href}");
        }
    }
}
