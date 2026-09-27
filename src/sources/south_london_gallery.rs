//! South London Gallery (Peckham Road, SE5) — events and exhibitions from
//! the site's server-rendered WordPress listings.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   with an empty `Disallow:`, so everything is allowed.
//! * No `Event` JSON-LD (only Yoast's `WebPage` graph), and the category RSS
//!   feed carries publish dates, so CSS on two listings, no detail pages
//!   (2 requests a run): `/whats-on/events/events-film-talks/` and
//!   `/whats-on/exhibitions/`. The events listing's `page/2/` repeats page
//!   one, so only the first page is read.
//! * A card is an `article.tease` with `h2.post-title`, `span.datetime`, an
//!   `img` and a "Find out more" link to `/events/<slug>/` or
//!   `/exhibitions/<slug>/`. The listings carry no description.
//! * Date lines: "WED 7 OCT 2026, 6:30-8:00pm", "THU 1 OCT, 6-9PM" (no
//!   year: resolved against the London date of the fetch, `listed_on`, with
//!   `chisenhale_gallery::infer_date`), "26 Sep – 25 Oct 2026," and "10 Sep - 22 Nov 2026".
//!   A single day with a time is timed in London wall clock; a single day
//!   without one, a "From <day>" and every range are all-day (London
//!   midnights, `ends_at` the last day, none for a single day or an open
//!   "From" run). Recurring lines ("EVERY SAT & SUN") are skipped; runs
//!   longer than [`MAX_RANGE_DAYS`] too.
//! * Categories: exhibitions → exhibition, except online-only selling shows
//!   (skipped). Events by title: film screenings, children's / family
//!   sessions and fundraising raffles are skipped; otherwise the title's
//!   keywords decide ("Talk: …" → talk, "Teachers' CPDL Workshop" →
//!   workshop) and anything else (a book launch party) is community.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::{parse_date_range, parse_time_range};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_text, dedupe_key, london_date, london_to_utc, map_category, words};

pub const KEY: &str = "south-london-gallery";
/// Longer runs are ongoing displays, not exhibitions.
pub const MAX_RANGE_DAYS: i64 = 366;
pub const EVENTS_PATH: &str = "/whats-on/events/events-film-talks/";
pub const EXHIBITIONS_PATH: &str = "/whats-on/exhibitions/";
const VENUE_NAME: &str = "South London Gallery";
const VENUE_ADDRESS: &str = "65-67 Peckham Road, London SE5 8UH";

/// Which listing a card came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Events,
    Exhibitions,
}

impl Section {
    fn prefix(self) -> &'static str {
        match self {
            Section::Events => "/events/",
            Section::Exhibitions => "/exhibitions/",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Section::Events => "events",
            Section::Exhibitions => "exhibitions",
        }
    }
}

pub struct SouthLondonGallery {
    base_url: Url,
}

impl SouthLondonGallery {
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

/// The card path (`/events/<slug>`) of a link, absolute or relative: a
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
    valid.then(|| format!("{}{slug}/", section.prefix()))
}

/// The cards of one listing page, in page order, one per path.
pub fn parse_listing(html: &str, section: Section) -> Vec<Value> {
    let doc = Html::parse_document(html);
    let title_sel = selector("h2.post-title");
    let date_sel = selector("span.datetime");
    let link_sel = selector("a[href]");
    let img_sel = selector("img[src]");
    let mut cards: Vec<Value> = Vec::new();
    for card in doc.select(&selector("article.tease")) {
        let Some(path) = card
            .select(&link_sel)
            .filter_map(|a| a.value().attr("href"))
            .find_map(|h| card_path(h, section))
        else {
            continue;
        };
        let Some(title) = card
            .select(&title_sel)
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        if cards.iter().any(|c| c["path"] == path.as_str()) {
            continue;
        }
        let date_text = card
            .select(&date_sel)
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty());
        let image = card
            .select(&img_sel)
            .next()
            .and_then(|img| img.value().attr("src"))
            .map(str::to_string);
        cards.push(json!({
            "path": path,
            "title": title,
            "date_text": date_text,
            "image": image,
        }));
    }
    cards
}

/// The stored payload for a card; `listed_on` is the London date of the
/// fetch, kept for year inference.
pub fn raw_event(card: &Value, section: Section, site: &Url, listed_on: NaiveDate) -> RawEvent {
    let path = card["path"].as_str().unwrap_or_default().to_string();
    let url = site
        .join(&path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| path.clone());
    let image = card["image"]
        .as_str()
        .and_then(|src| site.join(src).ok())
        .map(|u| u.to_string());
    RawEvent {
        source_event_id: path,
        source_url: Some(url.clone()),
        payload: json!({
            "url": url,
            "section": section.name(),
            "card": card,
            "image_url": image,
            "listed_on": listed_on.to_string(),
        }),
    }
}

/// When a date line says an event happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// All day from the first to the last day (`None`: a single day or an
    /// open "From" run).
    Days(NaiveDate, Option<NaiveDate>),
    /// A day, London wall-clock start and optional end.
    Timed(NaiveDate, NaiveTime, Option<NaiveTime>),
    /// A recurring slot ("EVERY SAT & SUN"): out of scope.
    Recurring,
}

/// Parse a date line ("WED 7 OCT 2026, 6:30-8:00pm", "THU 1 OCT, 6-9PM",
/// "26 Sep – 25 Oct 2026,", "From 1 Oct 2026").
pub fn parse_when(text: &str, listed_on: NaiveDate) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {text:?}"));
    let text = clean_text(text);
    let (date_part, time_part) = match text.split_once(',') {
        Some((d, t)) => (d.trim(), t.trim()),
        None => (text.trim(), ""),
    };
    let lower = date_part.to_lowercase();
    if lower.starts_with("every") {
        return Ok(When::Recurring);
    }
    if let Some(rest) = lower.strip_prefix("from ") {
        let (day, _) = parse_date_range(rest, listed_on)?.ok_or_else(err)?;
        return Ok(When::Days(day, None));
    }
    let (first, last) = parse_date_range(date_part, listed_on)?.ok_or_else(err)?;
    if first != last {
        return Ok(When::Days(first, Some(last)));
    }
    if time_part.is_empty() {
        return Ok(When::Days(first, None));
    }
    let (from, to) = parse_time_range(time_part).ok_or_else(err)?;
    Ok(When::Timed(first, from, to))
}

/// Category for an events-listing title; `None` means out of scope.
pub fn event_category(title: &str) -> Option<Category> {
    let w = words(title);
    let has = |names: &[&str]| names.iter().any(|n| w.iter().any(|x| x == n));
    if has(&[
        "film",
        "films",
        "screening",
        "screenings",
        "children",
        "childrens",
        "family",
        "families",
        "kids",
        "raffle",
    ]) {
        None
    } else {
        Some(map_category(&[title]).unwrap_or(Category::Community))
    }
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
    let category = if p["section"] == "exhibitions" {
        if words(&title).iter().any(|w| w == "online") {
            return Ok(None);
        }
        Category::Exhibition
    } else {
        match event_category(&title) {
            Some(c) => c,
            None => return Ok(None),
        }
    };
    let listed_on = p["listed_on"]
        .as_str()
        .and_then(|s| s.parse::<NaiveDate>().ok())
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no listed_on")))?;
    let date_text = card["date_text"]
        .as_str()
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no date line")))?;
    let (starts_at, ends_at, all_day) = match parse_when(date_text, listed_on)? {
        When::Recurring => return Ok(None),
        When::Days(first, last) => {
            let last = last.filter(|l| *l > first);
            if last.is_some_and(|l| (l - first).num_days() > MAX_RANGE_DAYS) {
                return Ok(None);
            }
            (london_midnight(first), last.map(london_midnight), true)
        }
        When::Timed(day, from, to) => {
            let s = london_to_utc(day.and_time(from));
            let e = to
                .map(|t| london_to_utc(day.and_time(t)))
                .filter(|e| *e > s);
            (s, e, false)
        }
    };
    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        title,
        description: None,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        price: Price::default(),
        url: p["url"].as_str().map(str::to_string),
        image_url: p["image_url"].as_str().map(str::to_string),
        category,
        tags: Vec::new(),
    }))
}

#[async_trait]
impl Source for SouthLondonGallery {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listed_on = london_date(Utc::now());
        let mut out: Vec<RawEvent> = Vec::new();
        let events = ctx.get_text(&self.url(EVENTS_PATH)?).await?;
        let cards = parse_listing(&events, Section::Events);
        if cards.is_empty() {
            return Err(SourceError::Parse(format!(
                "no event cards on {EVENTS_PATH}"
            )));
        }
        out.extend(
            cards
                .iter()
                .map(|c| raw_event(c, Section::Events, &self.base_url, listed_on)),
        );
        match ctx.get_text(&self.url(EXHIBITIONS_PATH)?).await {
            Ok(html) => out.extend(
                parse_listing(&html, Section::Exhibitions)
                    .iter()
                    .map(|c| raw_event(c, Section::Exhibitions, &self.base_url, listed_on)),
            ),
            Err(e) => ctx.report_error(format!("{EXHIBITIONS_PATH}: {e}")),
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

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn payload(section: &str, title: &str, date: &str) -> Value {
        json!({
            "url": "https://www.southlondongallery.org/events/x/",
            "section": section,
            "card": {"path": "/events/x/", "title": title, "date_text": date, "image": null},
            "image_url": null,
            "listed_on": "2026-09-27",
        })
    }

    #[test]
    fn date_lines() {
        let on = d(2026, 9, 27);
        let w = |s: &str| parse_when(s, on).unwrap();
        assert_eq!(
            w("WED 7 OCT 2026, 6:30-8:00pm"),
            When::Timed(d(2026, 10, 7), t(18, 30), Some(t(20, 0)))
        );
        assert_eq!(
            w("THU 1 OCT, 6-9PM"),
            When::Timed(d(2026, 10, 1), t(18, 0), Some(t(21, 0)))
        );
        assert_eq!(
            w("Sat 26 SEP 2026, 11am-5pm"),
            When::Timed(d(2026, 9, 26), t(11, 0), Some(t(17, 0)))
        );
        assert_eq!(
            w("26 Sep – 25 Oct 2026,"),
            When::Days(d(2026, 9, 26), Some(d(2026, 10, 25)))
        );
        assert_eq!(
            w("10 Sep - 22 Nov 2026"),
            When::Days(d(2026, 9, 10), Some(d(2026, 11, 22)))
        );
        assert_eq!(w("From 1 Oct 2026"), When::Days(d(2026, 10, 1), None));
        assert_eq!(w("SAT 3 OCT"), When::Days(d(2026, 10, 3), None));
        assert_eq!(w("EVERY SAT & SUN, 12-6PM"), When::Recurring);
        assert!(parse_when("Coming soon", on).is_err());
    }

    #[test]
    fn year_less_dates_roll_into_next_year() {
        let on = d(2026, 11, 20);
        assert_eq!(
            parse_when("THU 14 JAN, 7-9PM", on).unwrap(),
            When::Timed(d(2027, 1, 14), t(19, 0), Some(t(21, 0)))
        );
    }

    #[test]
    fn card_paths() {
        assert_eq!(
            card_path(
                "https://www.southlondongallery.org/events/talk-weight-of-matter/",
                Section::Events
            )
            .as_deref(),
            Some("/events/talk-weight-of-matter/")
        );
        assert_eq!(
            card_path("/exhibitions/x", Section::Exhibitions).as_deref(),
            Some("/exhibitions/x/")
        );
        assert_eq!(card_path("/whats-on/events/", Section::Events), None);
        assert_eq!(card_path("/events/a/b/", Section::Events), None);
    }

    #[test]
    fn categories() {
        assert_eq!(
            event_category("Talk: Weight of Matter"),
            Some(Category::Talk)
        );
        assert_eq!(
            event_category("Teachers’ CPDL Workshop: Monika Sosnowska"),
            Some(Category::Workshop)
        );
        assert_eq!(event_category("FILM SCREENING: PSYCHOBUILDINGS"), None);
        assert_eq!(event_category("Children’s Garden Trail"), None);
        assert_eq!(event_category("SLG Forever Raffle"), None);
        assert_eq!(
            event_category("Celebrating Fifteen Years of Publishing with And Other Stories"),
            Some(Category::Community)
        );
    }

    #[test]
    fn timed_event_crosses_the_clock_change() {
        let ev = normalise_payload(&payload("events", "Talk: Late", "THU 29 OCT, 6:30-8PM"))
            .unwrap()
            .unwrap();
        assert!(!ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-10-29T18:30:00+00:00");
        assert_eq!(
            ev.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2026-10-29T20:00:00+00:00")
        );
    }

    #[test]
    fn a_day_without_a_time_is_all_day() {
        let ev = normalise_payload(&payload("events", "Talk: Day", "SAT 3 OCT 2026"))
            .unwrap()
            .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-10-02T23:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn open_ended_from_date_is_all_day_without_end() {
        let ev = normalise_payload(&payload("exhibitions", "A Show", "From 1 Oct 2026"))
            .unwrap()
            .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.category, Category::Exhibition);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-30T23:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn exhibition_range_is_all_day_to_the_last_day() {
        let ev = normalise_payload(&payload(
            "exhibitions",
            "Monika Sosnowska: Weight of Matter",
            "10 Sep - 22 Nov 2026",
        ))
        .unwrap()
        .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-09T23:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2026-11-22T00:00:00+00:00")
        );
    }

    #[test]
    fn skips_and_errors() {
        let online = payload(
            "exhibitions",
            "SLG Forever at Christie’s | Online Selling Exhibition",
            "5 Jun - 30 Sep 2026",
        );
        assert_eq!(normalise_payload(&online).unwrap(), None);
        let recurring = payload("events", "Talk: Weekly", "EVERY SAT & SUN, 12-6PM");
        assert_eq!(normalise_payload(&recurring).unwrap(), None);
        let long = payload("exhibitions", "Long", "1 Jan 2026 - 1 Jan 2028");
        assert_eq!(normalise_payload(&long).unwrap(), None);
        let mut no_date = payload("events", "Talk: X", "");
        no_date["card"]["date_text"] = Value::Null;
        let err = normalise_payload(&no_date).unwrap_err();
        assert!(err.to_string().contains("no date"), "{err}");
    }
}
