//! Handel Hendrix House (25 Brook Street, Mayfair, W1) — exhibitions and
//! talks from CSS selectors on its Craft CMS pages.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): the
//!   `User-agent: *` group disallows only `/cpresources/`, `/vendor/`,
//!   `/.env` and `/cache/`.
//! * There is no JSON-LD `Event` (pages carry only `WebPage`,
//!   `LocalBusiness` and `BreadcrumbList`). `/whats-on` links each item from
//!   a card (`div.listing-card`) with an absolute `/events/<slug>` URL; the
//!   path without its leading `/` is the `source_event_id`. At most
//!   [`MAX_DETAIL_PAGES`] pages a run.
//! * Each page has the title (`h1`), sometimes an eyebrow above it ("New
//!   Exhibition", "Georgian Cooking"), a facts column (`div.border-b` spans)
//!   and the body text (the description). The facts are a date line, then a
//!   time line and a price ("£25", "Included in general admission") or the
//!   place. Date lines are one day (`Sat 21st<br>Nov 2026`, times from the
//!   time line as London wall-clock) or a date-only range (`Open 17th
//!   September 2026 - 14th March 2027`), stored `all_day`. A page whose
//!   facts name no year is a recurring or open-ended programme ("Every
//!   Thursday", "Select Sundays", "From 19th June") and is skipped.
//! * Most of the programme is concerts, which are out of scope. The
//!   category comes from keywords in the eyebrow and title only (concert
//!   bodies mention "readings" and "musical conversation"), plus a body
//!   that describes a demonstration (the food historians' Georgian cooking
//!   evenings), a talk. Anything else is skipped; `qa_scope` tells the
//!   scraper check.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_to_utc, map_category, parse_price, words,
};

pub const KEY: &str = "handel-hendrix";
/// Per-run cap on event page fetches (the listing shows about 17).
pub const MAX_DETAIL_PAGES: usize = 30;
const LISTING_PATH: &str = "/whats-on";
/// Cards link the live site absolutely, whatever host the listing came from.
const SITE_HOST: &str = "handelhendrix.org";
const EVENT_PREFIX: &str = "/events/";
const VENUE_NAME: &str = "Handel Hendrix House";
const VENUE_ADDRESS: &str = "25 Brook Street, Mayfair, London W1K 4HB";
const VENUE_LAT: f64 = 51.512780;
const VENUE_LNG: f64 = -0.148120;

pub struct HandelHendrix {
    base_url: Url,
    max_detail_pages: usize,
}

impl HandelHendrix {
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

/// The event page paths linked from the listing's cards, in page order,
/// de-duplicated. `page_url` is the address the listing was fetched from;
/// links must stay on its host or the live site's.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<String> {
    let doc = Html::parse_document(html);
    let mut out: Vec<String> = Vec::new();
    for a in doc.select(&selector("div.listing-card a[href]")) {
        let Some(Ok(url)) = a.value().attr("href").map(|h| page_url.join(h)) else {
            continue;
        };
        let on_site = url.host_str() == page_url.host_str() || url.host_str() == Some(SITE_HOST);
        let valid = on_site
            && url.query().is_none()
            && url
                .path()
                .strip_prefix(EVENT_PREFIX)
                .is_some_and(|slug| !slug.is_empty() && !slug.contains('/'));
        if valid && !out.iter().any(|p| p == url.path()) {
            out.push(url.path().to_string());
        }
    }
    out
}

/// Parse an event page (fetched from `page_url`) into a [`RawEvent`].
/// `None` for a page without a title.
pub fn parse_detail(html: &str, page_url: &Url) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let title = doc.select(&selector("main h1")).next().map(element_text)?;
    let eyebrow = doc
        .select(&selector("main div.border-b-2 > p"))
        .next()
        .map(element_text);
    let facts: Vec<String> = doc
        .select(&selector("main div.border-b.border-black > span"))
        .map(element_text)
        .filter(|t| !t.is_empty())
        .collect();
    let body = doc
        .select(&selector(
            "main section.section--text-image-two-one div.prose",
        ))
        .next()
        .map(|e| e.inner_html());
    Some(RawEvent {
        source_event_id: page_url.path().trim_start_matches('/').to_string(),
        source_url: Some(page_url.to_string()),
        payload: json!({
            "url": page_url.as_str(),
            "title": title,
            "eyebrow": eyebrow,
            "facts": facts,
            "body": body,
        }),
    })
}

/// A facts line naming a year is the date line.
fn is_date_line(line: &str) -> bool {
    line.split(|c: char| !c.is_ascii_digit())
        .any(|n| n.len() == 4)
}

/// A facts line starting with a clock time ("18:00", "17:00 & 19:00").
fn is_time_line(line: &str) -> bool {
    line.split_whitespace()
        .next()
        .is_some_and(|w| w.contains(':') && w.starts_with(|c: char| c.is_ascii_digit()))
}

/// "Sat 21st Nov 2026", "Tue 29th Sept 2026", "Open 17th September 2026",
/// "17th September" or "17th": words before the day are dropped, and the
/// missing month and year come from `end` (the range's last day).
fn parse_day(s: &str, end: Option<NaiveDate>) -> Option<NaiveDate> {
    let mut tokens: Vec<String> = s
        .split_whitespace()
        .skip_while(|w| !w.starts_with(|c: char| c.is_ascii_digit()))
        // chrono knows "Sep" and "September", the site also prints "Sept"
        .map(|w| if w == "Sept" { "Sep" } else { w }.to_string())
        .collect();
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

/// First and last day of a date line (equal for a single day).
pub fn parse_dates(line: &str) -> Result<(NaiveDate, NaiveDate), SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {line:?}"));
    let line = clean_text(line);
    match line.split_once(['-', '–', '—']) {
        Some((a, b)) => {
            let last = parse_day(b, None).ok_or_else(err)?;
            let first = parse_day(a, Some(last)).ok_or_else(err)?;
            if first <= last {
                Ok((first, last))
            } else {
                Err(err())
            }
        }
        None => {
            let day = parse_day(&line, None).ok_or_else(err)?;
            Ok((day, day))
        }
    }
}

/// London wall-clock start and (if given) end of a time line: "18:00" or
/// "19:00 - 21:00".
pub fn parse_times(line: &str) -> Result<(NaiveTime, Option<NaiveTime>), SourceError> {
    let err = || SourceError::Parse(format!("unrecognised time line {line:?}"));
    let time = |s: &str| NaiveTime::parse_from_str(s.trim(), "%H:%M").map_err(|_| err());
    match line.split_once(['-', '–', '—']) {
        Some((start, end)) => {
            let (start, end) = (time(start)?, time(end)?);
            if end <= start {
                return Err(err());
            }
            Ok((start, Some(end)))
        }
        None => Ok((time(line)?, None)),
    }
}

fn describes_demonstration(body: &str) -> bool {
    words(body).iter().any(|w| w.starts_with("demonstrat"))
}

/// Normalise a Handel Hendrix House [`RawEvent`] payload.
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
    let Some(date_line) = facts.iter().copied().find(|l| is_date_line(l)) else {
        return Ok(None);
    };
    let body = clean_description(text("body"));
    let category =
        map_category(&[text("eyebrow").unwrap_or_default(), title.as_str()]).or_else(|| {
            body.as_deref()
                .is_some_and(describes_demonstration)
                .then_some(Category::Talk)
        });
    let Some(category) = category else {
        return Ok(None);
    };

    let (first, last) = parse_dates(date_line)?;
    let time_line = facts.iter().copied().find(|l| is_time_line(l));
    let (starts_at, ends_at, all_day) = match time_line {
        Some(line) => {
            if first != last {
                return Err(SourceError::Parse(format!(
                    "times {line:?} on a date range {date_line:?}"
                )));
            }
            let (start, end) = parse_times(line)?;
            (
                london_to_utc(first.and_time(start)),
                end.map(|e| london_to_utc(first.and_time(e))),
                false,
            )
        }
        None => (
            london_to_utc(first.and_time(NaiveTime::MIN)),
            (last > first).then(|| london_to_utc(last.and_time(NaiveTime::MIN))),
            true,
        ),
    };
    let other_facts: Vec<&str> = facts
        .iter()
        .copied()
        .filter(|l| *l != date_line && Some(*l) != time_line)
        .collect();

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: body,
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day,
        price: parse_price(&other_facts.join("; ")),
        url: text("url").map(str::to_string),
        image_url: None,
        category,
        tags: Vec::new(),
    }))
}

#[async_trait]
impl Source for HandelHendrix {
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

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only exhibitions, talks, workshops, fairs and community events: pages \
             whose eyebrow or title names one of those (lecture, conversation, \
             reading, class, display and the like), or whose description describes \
             a demonstration. Concerts, recitals and the rest of the music programme \
             (salons, sessions, children's concerts, ensembles, anniversary \
             celebrations) are left out on purpose, as are recurring or open-ended \
             programmes whose dates name no year (\"Every Thursday\", \"From 19th June\").",
        )
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

    fn payload(title: &str, eyebrow: Option<&str>, facts: &[&str], body: &str) -> Value {
        json!({
            "url": "https://handelhendrix.org/events/x",
            "title": title,
            "eyebrow": eyebrow,
            "facts": facts,
            "body": body,
        })
    }

    #[test]
    fn parses_single_days_and_ranges() {
        let one = |d| (d, d);
        assert_eq!(
            parse_dates("Sat 21st Nov 2026").unwrap(),
            one(day(2026, 11, 21))
        );
        assert_eq!(
            parse_dates("Tue 29th Sept 2026").unwrap(),
            one(day(2026, 9, 29))
        );
        assert_eq!(
            parse_dates("Tue 13th October 2026").unwrap(),
            one(day(2026, 10, 13))
        );
        assert_eq!(
            parse_dates("Open 17th September 2026 - 14th March 2027").unwrap(),
            (day(2026, 9, 17), day(2027, 3, 14))
        );
        assert_eq!(
            parse_dates("2nd–23rd November 2026").unwrap(),
            (day(2026, 11, 2), day(2026, 11, 23))
        );
    }

    #[test]
    fn unrecognised_date_lines_are_parse_errors() {
        for line in [
            "Sat 21st Nob 2026",
            "Every Thursday 2026",
            "14th March 2027 - 17th September 2026",
        ] {
            let err = parse_dates(line).unwrap_err().to_string();
            assert!(err.contains("unrecognised date line"), "{line}: {err}");
        }
    }

    #[test]
    fn parses_time_lines() {
        assert_eq!(parse_times("18:00").unwrap(), (hm(18, 0), None));
        assert_eq!(
            parse_times("19:00 - 21:00").unwrap(),
            (hm(19, 0), Some(hm(21, 0)))
        );
        for line in ["17:00 & 19:00", "21:00 - 19:00", "7pm"] {
            let err = parse_times(line).unwrap_err().to_string();
            assert!(err.contains("unrecognised time line"), "{line}: {err}");
        }
    }

    #[test]
    fn london_times_across_the_clock_change() {
        let talk = |date: &str| {
            normalise_payload(&payload("A talk", None, &[date, "18:00", "£25"], ""))
                .unwrap()
                .unwrap()
        };
        let bst = talk("Thu 22nd Oct 2026");
        assert_eq!(bst.starts_at.to_rfc3339(), "2026-10-22T17:00:00+00:00");
        assert!(!bst.all_day);
        let gmt = talk("Sat 21st Nov 2026");
        assert_eq!(gmt.starts_at.to_rfc3339(), "2026-11-21T18:00:00+00:00");
        assert_eq!(gmt.price, parse_price("£25"));
    }

    #[test]
    fn exhibition_ranges_are_all_day() {
        let ev = normalise_payload(&payload(
            "The Hallelujah House",
            Some("New Exhibition"),
            &[
                "Open 17th September 2026 - 14th March 2027",
                "Included in general admission",
            ],
            "",
        ))
        .unwrap()
        .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.category, Category::Exhibition);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-16T23:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2027-03-14T00:00:00+00:00")
        );
        assert!(!ev.price.is_free);
    }

    #[test]
    fn times_on_a_range_are_a_parse_error() {
        let ev = payload(
            "Display",
            Some("Exhibition"),
            &["2nd–23rd November 2026", "10:00"],
            "",
        );
        assert!(normalise_payload(&ev).is_err());
    }

    #[test]
    fn programmes_without_a_year_are_skipped() {
        for facts in [
            &["Every Thursday", "14:00"][..],
            &["From 19th June", "Included in general admission"],
        ] {
            let ev = payload("Talks", Some("New Exhibition"), facts, "");
            assert!(normalise_payload(&ev).unwrap().is_none(), "{facts:?}");
        }
    }

    #[test]
    fn concerts_are_skipped_before_their_times_are_read() {
        let concert = payload(
            "The Jimi Sessions with Andy Cortes",
            None,
            &["Thu 15th Oct 2026", "18:30 & 19:30", "£25"],
            "<p>A genuine musical conversation direct to tape.</p>",
        );
        assert!(normalise_payload(&concert).unwrap().is_none());
    }

    #[test]
    fn a_demonstration_is_a_talk() {
        let ev = payload(
            "Feasting for Christmas!",
            Some("Georgian Cooking"),
            &["Sat 21st Nov 2026", "18:00", "£25"],
            "<p>Food historians demonstrate how the Georgians prepared.</p>",
        );
        let ev = normalise_payload(&ev).unwrap().unwrap();
        assert_eq!(ev.category, Category::Talk);
    }

    #[test]
    fn the_qa_scope_names_what_is_left_out() {
        let scope = HandelHendrix::new(Url::parse("https://x.test/").unwrap())
            .qa_scope()
            .unwrap();
        for word in [
            "exhibitions",
            "talks",
            "demonstration",
            "Concerts",
            "salons",
            "no year",
        ] {
            assert!(scope.contains(word), "{word}");
        }
    }
}
