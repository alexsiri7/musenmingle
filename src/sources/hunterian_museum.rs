//! Hunterian Museum (Royal College of Surgeons, Lincoln's Inn Fields, WC2)
//! — talks, workshops and exhibitions from CSS selectors on its Craft CMS
//! pages.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): the
//!   `User-agent: *` group disallows only `/cpresources/`, `/vendor/`,
//!   `/.env` and `/cache/`.
//! * There is no JSON-LD `Event` (listing and pages carry only
//!   `Museum`/`Organization`/`BreadcrumbList`). `/whats-on/` links each item
//!   from a card (`li.card h3 a.card__link`) with an absolute URL:
//!   `/events/<slug>` or `/exhibitions/<slug>`. The path without its leading
//!   `/` is the `source_event_id`, so the two can't collide. At most
//!   [`MAX_DETAIL_PAGES`] pages a run.
//! * Each page has the title (`h1`), a standfirst (the description), and a
//!   facts block (`div.border-y p`): a date line, then a price ("£8",
//!   "Free, drop in") or opening days. Exhibitions state no price there but
//!   their body says "included with free entry", read with
//!   `describes_free_entry`. Date lines are one day with London
//!   wall-clock times (`29th October 2026<br>19:00–20:15`) or a date-only
//!   range (`10th September–31st October 2026`), stored `all_day`. A page
//!   with no date line is a recurring programme ("every Wednesday
//!   afternoon") and is skipped; anything else is a parse error.
//! * `/exhibitions/` pages are exhibitions. Event pages have no category, so
//!   it comes from keywords in the title, standfirst and first body block
//!   (where "before the talk begins" marks a talk); the rest of the
//!   programme (family activity days, first-aid training) is hands-on, so
//!   unmatched events are workshops.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, describes_free_entry, london_to_utc, map_category,
    mentions_late_opening, parse_price,
};

pub const KEY: &str = "hunterian-museum";
/// Per-run cap on event page fetches (the listing shows about 10).
pub const MAX_DETAIL_PAGES: usize = 30;
const LISTING_PATH: &str = "/whats-on/";
/// Cards link the live site absolutely, whatever host the listing came from.
const SITE_HOST: &str = "hunterianmuseum.org";
const EVENT_PREFIX: &str = "/events/";
const EXHIBITION_PREFIX: &str = "/exhibitions/";
const VENUE_NAME: &str = "Hunterian Museum";
const VENUE_ADDRESS: &str =
    "Royal College of Surgeons of England, 38–43 Lincoln's Inn Fields, London WC2A 3PE";
const VENUE_LAT: f64 = 51.515480;
const VENUE_LNG: f64 = -0.115480;

pub struct HunterianMuseum {
    base_url: Url,
    max_detail_pages: usize,
}

impl HunterianMuseum {
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

/// The event and exhibition page paths linked from the listing's cards, in
/// page order, de-duplicated. `page_url` is the address the listing was
/// fetched from; links must stay on its host or the live site's.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<String> {
    let doc = Html::parse_document(html);
    let mut out: Vec<String> = Vec::new();
    for a in doc.select(&selector("li.card h3 a.card__link[href]")) {
        let Some(Ok(url)) = a.value().attr("href").map(|h| page_url.join(h)) else {
            continue;
        };
        let on_site = url.host_str() == page_url.host_str() || url.host_str() == Some(SITE_HOST);
        let valid = on_site
            && url.query().is_none()
            && [EVENT_PREFIX, EXHIBITION_PREFIX].iter().any(|prefix| {
                url.path()
                    .strip_prefix(prefix)
                    .is_some_and(|slug| !slug.is_empty())
            });
        if valid && !out.iter().any(|p| p == url.path()) {
            out.push(url.path().to_string());
        }
    }
    out
}

/// Parse an event or exhibition page (fetched from `page_url`) into a
/// [`RawEvent`]. `None` for a page without a title.
pub fn parse_detail(html: &str, page_url: &Url) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let title = doc.select(&selector("main h1")).next().map(element_text)?;
    let standfirst = doc
        .select(&selector("main div.text-h3.font-serif"))
        .next()
        .map(|e| e.inner_html());
    let body = doc
        .select(&selector("main div.prose"))
        .next()
        .map(|e| e.inner_html());
    let facts: Vec<String> = doc
        .select(&selector("main div.border-y p"))
        .map(element_text)
        .filter(|t| !t.is_empty())
        .collect();
    let image = doc
        .select(&selector(r#"meta[property="og:image"]"#))
        .next()
        .and_then(|m| m.value().attr("content"));
    Some(RawEvent {
        source_event_id: page_url.path().trim_start_matches('/').to_string(),
        source_url: Some(page_url.to_string()),
        payload: json!({
            "url": page_url.as_str(),
            "exhibition": page_url.path().starts_with(EXHIBITION_PREFIX),
            "title": title,
            "standfirst": standfirst,
            "body": body,
            "facts": facts,
            "image": image,
        }),
    })
}

/// When an item happens, from its date line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// First and last day (equal for a single day), no time given.
    Days(NaiveDate, NaiveDate),
    /// A day with London wall-clock start and (if given) end times.
    Timed(NaiveDate, NaiveTime, Option<NaiveTime>),
}

/// A facts line naming a year is the date line; the others are prices or
/// opening days.
fn is_date_line(line: &str) -> bool {
    line.split(|c: char| !c.is_ascii_digit())
        .any(|n| n.len() == 4)
}

/// "29th October 2026", "10th September" or "10th": the missing month and
/// year come from `end` (the range's last day).
fn parse_day(s: &str, end: Option<NaiveDate>) -> Option<NaiveDate> {
    let mut tokens: Vec<String> = s.split_whitespace().map(str::to_string).collect();
    let day = tokens.first_mut()?;
    *day = day
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .to_string();
    match (tokens.len(), end) {
        (3, _) => {}
        (2, Some(end)) => tokens.push(end.year().to_string()),
        (1, Some(end)) => tokens.push(end.format("%B %Y").to_string()),
        _ => return None,
    }
    NaiveDate::parse_from_str(&tokens.join(" "), "%d %B %Y").ok()
}

/// Parse a page's date line (see the module docs for the two forms).
pub fn parse_when(line: &str) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {line:?}"));
    let line = clean_text(line);
    let (dates, times) = match line.rsplit_once(' ') {
        Some((dates, times)) if times.contains(':') => (dates, Some(times)),
        _ => (line.as_str(), None),
    };
    let (first, last) = match dates.split_once(['–', '—']) {
        Some((a, b)) => {
            let last = parse_day(b, None).ok_or_else(err)?;
            (parse_day(a, Some(last)).ok_or_else(err)?, last)
        }
        None => {
            let day = parse_day(dates, None).ok_or_else(err)?;
            (day, day)
        }
    };
    let Some(times) = times else {
        return if first <= last {
            Ok(When::Days(first, last))
        } else {
            Err(err())
        };
    };
    if first != last {
        return Err(err());
    }
    let time = |s: &str| NaiveTime::parse_from_str(s.trim(), "%H:%M").map_err(|_| err());
    match times.split_once(['–', '—', '-']) {
        Some((start, end)) => {
            let (start, end) = (time(start)?, time(end)?);
            if end <= start {
                return Err(err());
            }
            Ok(When::Timed(first, start, Some(end)))
        }
        None => Ok(When::Timed(first, time(times)?, None)),
    }
}

/// Normalise a Hunterian Museum [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("page without a title".into()))?;
    let facts: Vec<&str> = payload
        .get("facts")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let (date_lines, other_facts): (Vec<&str>, Vec<&str>) =
        facts.into_iter().partition(|l| is_date_line(l));
    let Some(date_line) = date_lines.first() else {
        return Ok(None);
    };
    let (starts_at, ends_at, all_day) = match parse_when(date_line)? {
        When::Days(first, last) => (
            london_to_utc(first.and_time(NaiveTime::MIN)),
            (last > first).then(|| london_to_utc(last.and_time(NaiveTime::MIN))),
            true,
        ),
        When::Timed(day, start, end) => (
            london_to_utc(day.and_time(start)),
            end.map(|e| london_to_utc(day.and_time(e))),
            false,
        ),
    };

    let exhibition = payload.get("exhibition").and_then(Value::as_bool) == Some(true);
    let standfirst = clean_description(text("standfirst"));
    let body = clean_description(text("body"));
    let category = if exhibition {
        Category::Exhibition
    } else {
        map_category(&[
            title.as_str(),
            standfirst.as_deref().unwrap_or_default(),
            body.as_deref().unwrap_or_default(),
        ])
        .unwrap_or(Category::Workshop)
    };
    let other_facts = other_facts.join("; ");
    let mut price = parse_price(&other_facts);
    if price == Price::default() && body.as_deref().is_some_and(describes_free_entry) {
        price = parse_price("Free");
    }
    let tags = if exhibition && mentions_late_opening(&other_facts) {
        vec!["late opening".to_string()]
    } else {
        Vec::new()
    };

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: standfirst,
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day,
        price,
        url: text("url").map(str::to_string),
        image_url: text("image").map(str::to_string),
        category,
        tags,
    }))
}

#[async_trait]
impl Source for HunterianMuseum {
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
                    None => ctx.report_error(format!("{path}: no title")),
                },
                Err(e) => ctx.report_error(format!("{path}: {e}")),
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

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn hm(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn payload(title: &str, standfirst: &str, facts: &[&str], exhibition: bool) -> Value {
        json!({
            "url": "https://hunterianmuseum.org/events/x",
            "exhibition": exhibition,
            "title": title,
            "standfirst": standfirst,
            "facts": facts,
        })
    }

    #[test]
    fn parses_timed_days_and_date_ranges() {
        assert_eq!(
            parse_when("29th October 2026 19:00–20:15").unwrap(),
            When::Timed(day(2026, 10, 29), hm(19, 0), Some(hm(20, 15)))
        );
        assert_eq!(
            parse_when("1st November 2026 11:00").unwrap(),
            When::Timed(day(2026, 11, 1), hm(11, 0), None)
        );
        assert_eq!(
            parse_when("10th September–31st October 2026").unwrap(),
            When::Days(day(2026, 9, 10), day(2026, 10, 31))
        );
        assert_eq!(
            parse_when("2nd–23rd November 2026").unwrap(),
            When::Days(day(2026, 11, 2), day(2026, 11, 23))
        );
        assert_eq!(
            parse_when("22nd November 2026").unwrap(),
            When::Days(day(2026, 11, 22), day(2026, 11, 22))
        );
    }

    #[test]
    fn unrecognised_date_lines_are_parse_errors() {
        for line in [
            "Every Wednesday 2026",
            "29th October 2026 7pm",
            "31st October–10th September 2026",
            "10th–12th October 2026 10:00–16:00",
            "29th October 2026 20:15–19:00",
        ] {
            let err = parse_when(line).unwrap_err().to_string();
            assert!(err.contains("unrecognised date line"), "{line}: {err}");
        }
    }

    #[test]
    fn london_times_across_the_clock_change() {
        let talk = |date: &str| {
            normalise_payload(&payload("A talk", "", &[date, "£8"], false))
                .unwrap()
                .unwrap()
        };
        let bst = talk("22nd October 2026 19:00–21:00");
        assert_eq!(bst.starts_at.to_rfc3339(), "2026-10-22T18:00:00+00:00");
        assert_eq!(
            bst.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-22T20:00:00+00:00")
        );
        assert!(!bst.all_day);
        let gmt = talk("29th October 2026 19:00–20:15");
        assert_eq!(gmt.starts_at.to_rfc3339(), "2026-10-29T19:00:00+00:00");
    }

    #[test]
    fn exhibition_ranges_are_all_day() {
        let ev = normalise_payload(&payload(
            "The Operating Theatre",
            "",
            &["10th September–31st October 2026", "Wednesday - Saturday"],
            true,
        ))
        .unwrap()
        .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.category, Category::Exhibition);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-09T23:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-31T00:00:00+00:00")
        );
        assert!(ev.tags.is_empty());
        assert_eq!(ev.price, Price::default());
    }

    #[test]
    fn late_opening_exhibitions_are_tagged() {
        let tags = |facts: &[&str], exhibition: bool| {
            normalise_payload(&payload("The Operating Theatre", "", facts, exhibition))
                .unwrap()
                .unwrap()
                .tags
        };
        let late = [
            "1st–30th November 2026",
            "Late openings every Thursday until 20:00",
        ];
        assert_eq!(tags(&late, true), ["late opening"]);
        let talk = ["3rd November 2026 19:00", "Late opening"];
        assert!(tags(&talk, false).is_empty());
    }

    #[test]
    fn free_entry_in_the_body_is_a_free_price() {
        let mut ev = payload("Display", "", &["1st–30th November 2026"], true);
        ev["body"] = json!("<p>Included with free entry to the Hunterian Museum.</p>");
        assert!(normalise_payload(&ev).unwrap().unwrap().price.is_free);
    }

    #[test]
    fn a_page_without_a_date_is_skipped() {
        let tour = payload(
            "Wednesday Curator's Museum Highlights",
            "",
            &["Free"],
            false,
        );
        assert!(normalise_payload(&tour).unwrap().is_none());
    }

    #[test]
    fn categories_from_keywords_default_to_workshop() {
        let category = |title: &str, standfirst: &str| {
            normalise_payload(&payload(
                title,
                standfirst,
                &["3rd November 2026 11:00"],
                false,
            ))
            .unwrap()
            .unwrap()
            .category
        };
        assert_eq!(
            category("Amputation", "A talk by Dawn Kemp"),
            Category::Talk
        );
        assert_eq!(category("Stop the Bleed Training", ""), Category::Workshop);
        let mut evening = payload("Corpse stealing", "", &["22nd October 2026 19:00"], false);
        evening["body"] = json!("<p>A display, before the talk begins.</p>");
        let evening = normalise_payload(&evening).unwrap().unwrap();
        assert_eq!(evening.category, Category::Talk);
    }
}
