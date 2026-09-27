//! ICA (Institute of Contemporary Arts, The Mall, SW1) — talks, events and
//! exhibitions from the site's server-rendered programme listings.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture) disallows only
//!   `/views/`, `/open-records-generator/` and dev paths, so the listings
//!   and detail pages are allowed.
//! * No JSON-LD at all, so CSS: two listings, `/talks` and `/exhibitions`
//!   (a `div.category-section` per tab, `div.item` cards with `div.title`,
//!   `div.date`, `div.description` and an `img`). `/upcoming` and `/live`
//!   are not read: they are mostly film screenings and concerts, which are
//!   out of scope.
//! * On `/talks`, the `#residencies` and `#youth-programme` tabs repeat
//!   cards from `#all-events`; those cards are participant programmes
//!   (residencies, the young artists' course), not public events, and are
//!   skipped. `films` cards linking to `/films/…` are skipped too.
//! * Listing date lines: "Wed, 30 September" (no year: resolved against the
//!   London date of the fetch, `listed_on`), "7 October – 16 December 2026",
//!   "7 October 2026 – 10 July 2027", "16 – 17 October 2026". Ranges and
//!   "From <day>" are all-day (London midnights, `ends_at` the last day,
//!   none for an open "From" run); runs longer than [`MAX_RANGE_DAYS`] are
//!   skipped.
//! * A single day has no time on the listing, so its detail page is read
//!   (at most [`MAX_DETAILS`] a run): `.performance-list .performance`
//!   gives "Thu, 01 Oct 2026" + "07:00 pm", timed in London wall clock. A
//!   single day whose detail page has no time for that day is a parse
//!   error, never silently all-day. Ranges never read detail pages (an
//!   exhibition's performance list is hourly timed-entry slots).
//! * Categories: cards in the `/exhibitions` `#exhibitions` tab →
//!   exhibition. Everything else by title: film screenings ("Artist's Film
//!   Picks", "Film & Video") and members-only events (previews, curator
//!   tours) are skipped; book launches → talk; otherwise the title's
//!   keywords decide and anything else (performances) is community.
//! * Price: the detail page's `#price-range` text when it names an amount.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::{parse_date_range, parse_time_range};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, map_category,
    parse_price, words,
};

pub const KEY: &str = "ica";
/// Longer runs are ongoing programmes, not exhibitions.
pub const MAX_RANGE_DAYS: i64 = 366;
/// Detail pages read per run (single-day events only).
pub const MAX_DETAILS: usize = 20;
pub const TALKS_PATH: &str = "/talks";
pub const EXHIBITIONS_PATH: &str = "/exhibitions";
const VENUE_NAME: &str = "ICA";
const VENUE_ADDRESS: &str = "The Mall, London SW1Y 5AH";
/// `/talks` tabs whose cards are participant programmes, not events.
const PROGRAMME_TABS: [&str; 2] = ["residencies", "youth-programme"];

/// Which listing a card came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Talks,
    Exhibitions,
}

impl Section {
    fn prefix(self) -> &'static str {
        match self {
            Section::Talks => "/talks/",
            Section::Exhibitions => "/exhibitions/",
        }
    }
}

pub struct Ica {
    base_url: Url,
}

impl Ica {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }

    fn url(&self, path: &str) -> Result<Url, SourceError> {
        self.base_url
            .join(path)
            .map_err(|e| SourceError::Config(e.to_string()))
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// A title split by `<br>` ("Book Launch <br> The Whole Routine") joined
/// as "Book Launch: The Whole Routine"; a part already ending in ":" is
/// joined with a space.
fn title_text(e: ElementRef<'_>) -> String {
    let mut out = String::new();
    for part in e.text().map(clean_text).filter(|p| !p.is_empty()) {
        if !out.is_empty() {
            out.push_str(if out.ends_with(':') { " " } else { ": " });
        }
        out.push_str(&part);
    }
    out
}

/// The card path (`/talks/<slug>`) of a link, absolute or relative: a
/// single slug segment under the section's prefix.
pub fn card_path(href: &str, section: Section) -> Option<String> {
    let path = match Url::parse(href) {
        Ok(u) => u.path().to_string(),
        Err(_) => href.split(['?', '#']).next()?.to_string(),
    };
    let slug = path.strip_prefix(section.prefix())?.trim_end_matches('/');
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then(|| format!("{}{slug}", section.prefix()))
}

/// The in-scope cards of one listing page, in page order, one per path.
/// Each card records its tab (`#all-events`, `#exhibitions`, …).
pub fn parse_listing(html: &str, section: Section) -> Vec<Value> {
    let doc = Html::parse_document(html);
    let tab_sel = selector("div.category-section[id]");
    let item_sel = selector("div.item");
    let link_sel = selector("a[href]");
    let title_sel = selector("div.title");
    let date_sel = selector("div.date");
    let desc_sel = selector("div.description");
    let img_sel = selector("img[src]");
    let path_of = |item: ElementRef<'_>| {
        item.select(&link_sel)
            .next()
            .and_then(|a| a.value().attr("href"))
            .and_then(|h| card_path(h, section))
    };
    let tabs: Vec<ElementRef<'_>> = doc.select(&tab_sel).collect();
    let tab_id = |t: &ElementRef<'_>| t.value().id().unwrap_or_default().to_string();
    let programmes: HashSet<String> = tabs
        .iter()
        .filter(|t| PROGRAMME_TABS.contains(&tab_id(t).as_str()))
        .flat_map(|t| t.select(&item_sel).filter_map(path_of))
        .collect();
    let mut cards: Vec<Value> = Vec::new();
    for tab in tabs
        .iter()
        .filter(|t| !PROGRAMME_TABS.contains(&tab_id(t).as_str()))
    {
        for item in tab.select(&item_sel) {
            let Some(path) = path_of(item) else {
                continue;
            };
            if programmes.contains(&path) || cards.iter().any(|c| c["path"] == path.as_str()) {
                continue;
            }
            let Some(title) = item
                .select(&title_sel)
                .next()
                .map(title_text)
                .filter(|t| !t.is_empty())
            else {
                continue;
            };
            let text = |sel: &Selector| {
                item.select(sel)
                    .next()
                    .map(element_text)
                    .filter(|t| !t.is_empty())
            };
            let image = item
                .select(&img_sel)
                .next()
                .and_then(|img| img.value().attr("src"))
                .map(str::to_string);
            cards.push(json!({
                "path": path,
                "tab": tab_id(tab),
                "title": title,
                "date_text": text(&date_sel),
                "description": text(&desc_sel),
                "image": image,
            }));
        }
    }
    cards
}

/// What a detail page says: its performances (date and time lines) and
/// its price text.
pub fn parse_detail(html: &str) -> Value {
    let doc = Html::parse_document(html);
    let date_sel = selector("div.date");
    let time_sel = selector("div.time");
    let performances: Vec<Value> = doc
        .select(&selector(".performance-list .performance"))
        .map(|p| {
            let text = |sel: &Selector| p.select(sel).next().map(element_text);
            json!({"date": text(&date_sel), "time": text(&time_sel)})
        })
        .collect();
    let price = doc
        .select(&selector("#price-range"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty());
    json!({"performances": performances, "price_text": price})
}

/// The stored payload for a card; `listed_on` is the London date of the
/// fetch, kept for year inference; `detail` is [`parse_detail`]'s output
/// for single-day cards.
pub fn raw_event(
    card: &Value,
    section: Section,
    site: &Url,
    listed_on: NaiveDate,
    detail: Option<Value>,
) -> RawEvent {
    let path = card["path"].as_str().unwrap_or_default().to_string();
    let url = site
        .join(&path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| path.clone());
    let image = card["image"]
        .as_str()
        .and_then(|src| site.join(src).ok())
        .map(|u| u.to_string());
    let section = match section {
        Section::Talks => "talks",
        Section::Exhibitions => "exhibitions",
    };
    RawEvent {
        source_event_id: path,
        source_url: Some(url.clone()),
        payload: json!({
            "url": url,
            "section": section,
            "card": card,
            "image_url": image,
            "listed_on": listed_on.to_string(),
            "detail": detail,
        }),
    }
}

/// When a listing date line says an event happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// All day from the first to the last day (`None`: an open "From" run).
    Days(NaiveDate, Option<NaiveDate>),
    /// A single day; its time is on the detail page.
    Day(NaiveDate),
}

/// Parse a listing date line ("Wed, 30 September", "16 – 17 October 2026",
/// "7 October 2026 – 10 July 2027", "From 1 October 2026").
pub fn parse_when(text: &str, listed_on: NaiveDate) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {text:?}"));
    let text = clean_text(text);
    if let Some(rest) = text.to_lowercase().strip_prefix("from ") {
        let (day, _) = parse_date_range(rest, listed_on)?.ok_or_else(err)?;
        return Ok(When::Days(day, None));
    }
    let (first, last) = parse_date_range(&text, listed_on)?.ok_or_else(err)?;
    Ok(if first == last {
        When::Day(first)
    } else {
        When::Days(first, Some(last))
    })
}

/// Category for a card; `None` means out of scope.
pub fn category(card: &Value, section: &str) -> Option<Category> {
    let title = card["title"].as_str().unwrap_or_default();
    let slug = card["path"].as_str().unwrap_or_default();
    let w = words(title);
    let has = |names: &[&str]| names.iter().any(|n| w.iter().any(|x| x == n));
    if has(&["film", "films", "screening", "screenings", "cinema"])
        || has(&["member", "members"])
        || slug.contains("/members-")
    {
        return None;
    }
    if section == "exhibitions" && card["tab"] == "exhibitions" {
        return Some(Category::Exhibition);
    }
    if w.windows(2).any(|p| p[0] == "book" && p[1] == "launch") {
        return Some(Category::Talk);
    }
    Some(map_category(&[title]).unwrap_or(Category::Community))
}

/// Whether a card needs its detail page: an in-scope single day.
pub fn needs_detail(card: &Value, section: Section, listed_on: NaiveDate) -> bool {
    let section = match section {
        Section::Talks => "talks",
        Section::Exhibitions => "exhibitions",
    };
    category(card, section).is_some()
        && card["date_text"]
            .as_str()
            .is_some_and(|d| matches!(parse_when(d, listed_on), Ok(When::Day(_))))
}

/// The London wall-clock time the detail page gives for `day`.
fn detail_time(detail: &Value, day: NaiveDate, listed_on: NaiveDate) -> Option<NaiveTime> {
    detail["performances"].as_array()?.iter().find_map(|p| {
        let (d, _) = parse_date_range(p["date"].as_str()?, listed_on).ok()??;
        if d != day {
            return None;
        }
        parse_time_range(p["time"].as_str()?).map(|(t, _)| t)
    })
}

fn london_midnight(d: NaiveDate) -> DateTime<Utc> {
    london_to_utc(d.and_time(NaiveTime::MIN))
}

/// Normalise a payload built by [`raw_event`].
pub fn normalise_payload(p: &Value) -> Result<Option<NewEvent>, SourceError> {
    let card = &p["card"];
    let title = card["title"]
        .as_str()
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("card without a title".into()))?;
    let Some(category) = category(card, p["section"].as_str().unwrap_or_default()) else {
        return Ok(None);
    };
    let listed_on = p["listed_on"]
        .as_str()
        .and_then(|s| s.parse::<NaiveDate>().ok())
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no listed_on")))?;
    let date_text = card["date_text"]
        .as_str()
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no date line")))?;
    let (starts_at, ends_at, all_day) = match parse_when(date_text, listed_on)? {
        When::Days(first, last) => {
            let last = last.filter(|l| *l > first);
            if last.is_some_and(|l| (l - first).num_days() > MAX_RANGE_DAYS) {
                return Ok(None);
            }
            (london_midnight(first), last.map(london_midnight), true)
        }
        When::Day(day) => {
            let time = detail_time(&p["detail"], day, listed_on).ok_or_else(|| {
                SourceError::Parse(format!("{title:?}: no time for {day} on the detail page"))
            })?;
            (london_to_utc(day.and_time(time)), None, false)
        }
    };
    let price = p["detail"]["price_text"]
        .as_str()
        .filter(|t| t.contains('£'))
        .map(parse_price)
        .unwrap_or_default();
    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        title,
        description: clean_description(card["description"].as_str()),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        price,
        url: p["url"].as_str().map(str::to_string),
        image_url: p["image_url"].as_str().map(str::to_string),
        category,
        tags: Vec::new(),
    }))
}

#[async_trait]
impl Source for Ica {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listed_on = london_date(Utc::now());
        let talks = ctx.get_text(&self.url(TALKS_PATH)?).await?;
        let mut cards: Vec<(Value, Section)> = parse_listing(&talks, Section::Talks)
            .into_iter()
            .map(|c| (c, Section::Talks))
            .collect();
        if cards.is_empty() {
            return Err(SourceError::Parse(format!("no cards on {TALKS_PATH}")));
        }
        match ctx.get_text(&self.url(EXHIBITIONS_PATH)?).await {
            Ok(html) => cards.extend(
                parse_listing(&html, Section::Exhibitions)
                    .into_iter()
                    .map(|c| (c, Section::Exhibitions)),
            ),
            Err(e) => ctx.report_error(format!("{EXHIBITIONS_PATH}: {e}")),
        }
        let mut out: Vec<RawEvent> = Vec::new();
        let mut details = 0;
        for (card, section) in &cards {
            let mut detail = None;
            if needs_detail(card, *section, listed_on) {
                let path = card["path"].as_str().unwrap_or_default();
                if details >= MAX_DETAILS {
                    ctx.report_error(format!("{path}: over the {MAX_DETAILS} detail-page cap"));
                    continue;
                }
                details += 1;
                match ctx.get_text(&self.url(path)?).await {
                    Ok(html) => detail = Some(parse_detail(&html)),
                    Err(e) => {
                        ctx.report_error(format!("{path}: {e}"));
                        continue;
                    }
                }
            }
            out.push(raw_event(card, *section, &self.base_url, listed_on, detail));
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

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn payload(section: &str, tab: &str, title: &str, date: &str, detail: Value) -> Value {
        json!({
            "url": "https://www.ica.art/talks/x",
            "section": section,
            "card": {"path": "/talks/x", "tab": tab, "title": title, "date_text": date,
                     "description": null, "image": null},
            "image_url": null,
            "listed_on": "2026-09-27",
            "detail": detail,
        })
    }

    fn at(date: &str, time: &str) -> Value {
        json!({"performances": [{"date": date, "time": time}], "price_text": null})
    }

    #[test]
    fn date_lines() {
        let on = d(2026, 9, 27);
        let w = |s: &str| parse_when(s, on).unwrap();
        assert_eq!(w("Wed, 30 September"), When::Day(d(2026, 9, 30)));
        assert_eq!(w("Tue, 23 February 2027"), When::Day(d(2027, 2, 23)));
        assert_eq!(
            w("7 October – 16 December 2026"),
            When::Days(d(2026, 10, 7), Some(d(2026, 12, 16)))
        );
        assert_eq!(
            w("7 October 2026 – 10 July 2027"),
            When::Days(d(2026, 10, 7), Some(d(2027, 7, 10)))
        );
        assert_eq!(
            w("16 – 17 October 2026"),
            When::Days(d(2026, 10, 16), Some(d(2026, 10, 17)))
        );
        assert_eq!(w("From 1 October 2026"), When::Days(d(2026, 10, 1), None));
        assert!(parse_when("Coming soon", on).is_err());
    }

    #[test]
    fn year_less_dates_roll_into_next_year() {
        assert_eq!(
            parse_when("Thu, 14 January", d(2026, 11, 20)).unwrap(),
            When::Day(d(2027, 1, 14))
        );
    }

    #[test]
    fn card_paths() {
        assert_eq!(
            card_path("https://www.ica.art/talks/my-tragedy", Section::Talks).as_deref(),
            Some("/talks/my-tragedy")
        );
        assert_eq!(
            card_path("/exhibitions/autumn-knight/", Section::Exhibitions).as_deref(),
            Some("/exhibitions/autumn-knight")
        );
        assert_eq!(card_path("/films/edges-of-cinema", Section::Talks), None);
        assert_eq!(card_path("/talks/a/b", Section::Talks), None);
    }

    #[test]
    fn categories() {
        let c = |section: &str, tab: &str, path: &str, title: &str| {
            category(&json!({"path": path, "tab": tab, "title": title}), section)
        };
        assert_eq!(
            c(
                "exhibitions",
                "exhibitions",
                "/exhibitions/autumn-knight",
                "Autumn Knight: I Can't Complain"
            ),
            Some(Category::Exhibition)
        );
        assert_eq!(
            c(
                "exhibitions",
                "current-programme",
                "/exhibitions/p",
                "party of one: Performance by Autumn Knight"
            ),
            Some(Category::Community)
        );
        assert_eq!(
            c(
                "talks",
                "all-events",
                "/talks/b",
                "Book Launch: Wet Proof by Mary Stephenson"
            ),
            Some(Category::Talk)
        );
        assert_eq!(
            c(
                "exhibitions",
                "current-programme",
                "/exhibitions/f",
                "Artist’s Film Picks: Persepolis"
            ),
            None
        );
        assert_eq!(
            c(
                "exhibitions",
                "current-programme",
                "/exhibitions/members-exhibition-preview-x",
                "Exhibition Preview: X"
            ),
            None
        );
        assert_eq!(
            c(
                "exhibitions",
                "current-programme",
                "/exhibitions/t",
                "Members' Curator Tour: X"
            ),
            None
        );
    }

    #[test]
    fn a_single_day_takes_its_time_from_the_detail_page() {
        let ev = normalise_payload(&payload(
            "talks",
            "all-events",
            "Book Launch: The Whole Routine",
            "Wed, 30 September",
            at("Wed, 30 Sep 2026", "06:00 pm"),
        ))
        .unwrap()
        .unwrap();
        assert!(!ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-30T17:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn timed_event_crosses_the_clock_change() {
        let ev = normalise_payload(&payload(
            "exhibitions",
            "current-programme",
            "party done: Performance by Samra Mayanja",
            "Fri, 6 November",
            at("Fri, 06 Nov 2026", "06:30 pm"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-11-06T18:30:00+00:00");
    }

    #[test]
    fn a_single_day_without_a_detail_time_is_an_error() {
        let other_day = payload(
            "talks",
            "all-events",
            "Talk",
            "Wed, 30 September",
            at("Thu, 01 Oct 2026", "06:00 pm"),
        );
        assert!(normalise_payload(&other_day).is_err());
        let no_detail = payload(
            "talks",
            "all-events",
            "Talk",
            "Wed, 30 September",
            Value::Null,
        );
        let err = normalise_payload(&no_detail).unwrap_err();
        assert!(err.to_string().contains("no time"), "{err}");
    }

    #[test]
    fn ranges_and_from_dates_are_all_day() {
        let ev = normalise_payload(&payload(
            "exhibitions",
            "exhibitions",
            "Autumn Knight: I Can't Complain",
            "29 September – 29 November 2026",
            Value::Null,
        ))
        .unwrap()
        .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.category, Category::Exhibition);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-28T23:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2026-11-29T00:00:00+00:00")
        );
        let from = normalise_payload(&payload(
            "exhibitions",
            "exhibitions",
            "A Show",
            "From 1 October 2026",
            Value::Null,
        ))
        .unwrap()
        .unwrap();
        assert!(from.all_day);
        assert_eq!(from.starts_at.to_rfc3339(), "2026-09-30T23:00:00+00:00");
        assert_eq!(from.ends_at, None);
    }

    #[test]
    fn long_runs_and_missing_dates() {
        let long = payload(
            "talks",
            "all-events",
            "Collective in Residence",
            "1 January 2026 – 1 January 2028",
            Value::Null,
        );
        assert_eq!(normalise_payload(&long).unwrap(), None);
        let mut no_date = payload("talks", "all-events", "Talk: X", "", Value::Null);
        no_date["card"]["date_text"] = Value::Null;
        let err = normalise_payload(&no_date).unwrap_err();
        assert!(err.to_string().contains("no date"), "{err}");
    }
}
