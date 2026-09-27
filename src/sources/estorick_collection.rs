//! Estorick Collection of Modern Italian Art (Canonbury Square, N1) —
//! events and exhibitions from the site's server-rendered listings.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   with an empty `Disallow:`, so everything is allowed.
//! * No JSON-LD anywhere, so CSS on three listing pages, no detail pages
//!   (3 requests a run): `/events` (every upcoming event, no pagination),
//!   `/exhibitions` (the current show) and `/exhibitions/in-the/future`.
//! * A card is the link to `/events/<slug>` or `/exhibitions/<slug>` (one
//!   path segment: the `/events/under/…` tag links, `/exhibitions/by/…`
//!   artist index and "Read more..." duplicates are not cards) plus the
//!   `<p>`s of the nearest enclosing block that has any. The paragraphs are
//!   told apart by content, not by the spacing classes: a date line
//!   ("27 September 2026", "16 September 2026 - 20 December 2026",
//!   "From 5 March 2027"), a time line ("10:00 - 12:00", "18:30", London
//!   wall clock) and the description (the first other paragraph). Tags are
//!   the card's `.c-tag` links; the image is the card's `img`.
//! * Dates: a single day with a time is timed (no end when only the start
//!   is given); a single day without a time, a "From" date and every range
//!   are all-day (London midnights, `ends_at` the last day, none for a
//!   single day or an open "From" run). Runs longer than [`MAX_RANGE_DAYS`]
//!   are skipped.
//! * Categories: exhibitions → exhibition. Events by their tags, in order:
//!   FAMILIES / UNDER 5S are skipped (children's sessions, whatever else
//!   they are tagged with); TALK / BOOK LAUNCH / ITALIAN TALK / TOUR →
//!   talk; ADULT ART CLASS / LIFE DRAWING CLASS → workshop; SPECIAL EVENT /
//!   LATE THURSDAYS → community; otherwise the title's keywords decide
//!   (a "Symposium" is a talk), and an event nothing matches is skipped.
//!   Artist tags stay as tags.

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, map_category};

pub const KEY: &str = "estorick-collection";
/// Longer runs are ongoing displays, not exhibitions.
pub const MAX_RANGE_DAYS: i64 = 366;
pub const EVENTS_PATH: &str = "/events";
pub const EXHIBITIONS_PATH: &str = "/exhibitions";
pub const FUTURE_EXHIBITIONS_PATH: &str = "/exhibitions/in-the/future";
const VENUE_NAME: &str = "Estorick Collection";
const VENUE_ADDRESS: &str = "39a Canonbury Square, London N1 2AN";

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

pub struct EstorickCollection {
    base_url: Url,
}

impl EstorickCollection {
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

/// The card path (`/events/<slug>`) of a link, if it is one: a single slug
/// segment under the section's prefix.
pub fn card_path(href: &str, section: Section) -> Option<String> {
    let path = href.split(['?', '#']).next()?;
    let slug = path.strip_prefix(section.prefix())?.trim_end_matches('/');
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then(|| format!("{}{slug}", section.prefix()))
}

const MONTH_FORMATS: [&str; 2] = ["%d %B %Y", "%d %b %Y"];

fn parse_day(s: &str) -> Option<NaiveDate> {
    let s = clean_text(s);
    MONTH_FORMATS
        .iter()
        .find_map(|f| NaiveDate::parse_from_str(&s, f).ok())
}

/// A date line: a day, a range of days ("16 September - 20 December 2026"
/// takes the year from the end) or an open "From <day>". Returns the first
/// day, the last day (if a range) and whether it is an open "From" run.
pub fn parse_date_text(text: &str) -> Option<(NaiveDate, Option<NaiveDate>, bool)> {
    let text = clean_text(text).replace(['–', '—'], "-");
    let (text, from) = match text.strip_prefix("From ") {
        Some(rest) => (rest.to_string(), true),
        None => (text, false),
    };
    match text.split_once(" - ") {
        None => parse_day(&text).map(|d| (d, None, from)),
        Some(_) if from => None,
        Some((a, b)) => {
            let end = parse_day(b)?;
            let start = parse_day(a).or_else(|| {
                let d = parse_day(&format!("{} {}", a.trim(), end.year()))?;
                if d > end {
                    d.with_year(d.year() - 1)
                } else {
                    Some(d)
                }
            })?;
            (start <= end).then_some((start, Some(end), false))
        }
    }
}

/// A time line: `10:00 - 12:00` or a lone start `18:30` (the whole line).
pub fn parse_time_text(text: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    let text = clean_text(text).replace(['–', '—'], "-");
    let t = |s: &str| NaiveTime::parse_from_str(s.trim(), "%H:%M").ok();
    match text.split_once('-') {
        Some((a, b)) => Some((t(a)?, Some(t(b)?))),
        None => Some((t(&text)?, None)),
    }
}

/// The cards of one listing page, in page order, one per path.
pub fn parse_listing(html: &str, section: Section) -> Vec<Value> {
    let doc = Html::parse_document(html);
    let p_sel = selector("p");
    let tag_sel = selector(".c-tag");
    let img_sel = selector("img[src]");
    let mut cards: Vec<Value> = Vec::new();
    for link in doc.select(&selector("main a[href]")) {
        let Some(path) = link
            .value()
            .attr("href")
            .and_then(|h| card_path(h, section))
        else {
            continue;
        };
        let title = element_text(link);
        if title.is_empty()
            || title.to_lowercase().starts_with("read more")
            || cards.iter().any(|c| c["path"] == path.as_str())
        {
            continue;
        }
        let Some(block) = link
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|e| e.select(&p_sel).next().is_some())
        else {
            continue;
        };
        let (mut date_text, mut time_text, mut description) = (None, None, None);
        for p in block.select(&p_sel) {
            let text = element_text(p);
            if text.is_empty() {
                continue;
            }
            if date_text.is_none() && parse_date_text(&text).is_some() {
                date_text = Some(text);
            } else if time_text.is_none() && parse_time_text(&text).is_some() {
                time_text = Some(text);
            } else if description.is_none() {
                description = Some(text);
            }
        }
        let tags: Vec<String> = block
            .select(&tag_sel)
            .map(element_text)
            .filter(|t| !t.is_empty())
            .collect();
        let image = block
            .parent()
            .and_then(ElementRef::wrap)
            .and_then(|card| card.select(&img_sel).next())
            .and_then(|img| img.value().attr("src"))
            .map(str::to_string);
        cards.push(json!({
            "path": path,
            "title": title,
            "date_text": date_text,
            "time_text": time_text,
            "description": description,
            "tags": tags,
            "image": image,
        }));
    }
    cards
}

/// The stored payload for a card.
pub fn raw_event(card: &Value, section: Section, site: &Url) -> RawEvent {
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
        }),
    }
}

/// Category for an event card's tags (upper-case as on the page) and
/// title; `None` means out of scope.
pub fn event_category(tags: &[String], title: &str) -> Option<Category> {
    let has = |names: &[&str]| {
        tags.iter()
            .any(|t| names.iter().any(|n| t.eq_ignore_ascii_case(n)))
    };
    if has(&["FAMILIES", "UNDER 5S"]) {
        None
    } else if has(&["TALK", "BOOK LAUNCH", "ITALIAN TALK", "TOUR"]) {
        Some(Category::Talk)
    } else if has(&["ADULT ART CLASS", "LIFE DRAWING CLASS"]) {
        Some(Category::Workshop)
    } else if has(&["SPECIAL EVENT", "LATE THURSDAYS"]) {
        Some(Category::Community)
    } else {
        map_category(&[title])
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
    let tags: Vec<String> = card["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let category = if p["section"] == "exhibitions" {
        Category::Exhibition
    } else {
        match event_category(&tags, &title) {
            Some(c) => c,
            None => return Ok(None),
        }
    };
    let date_text = card["date_text"].as_str().unwrap_or_default();
    let (start, end, _from) = parse_date_text(date_text)
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no date line")))?;
    let end = end.filter(|e| *e > start);
    if end.is_some_and(|e| (e - start).num_days() > MAX_RANGE_DAYS) {
        return Ok(None);
    }
    let time = card["time_text"].as_str().and_then(parse_time_text);
    let (starts_at, ends_at, all_day) = match (end, time) {
        (None, Some((from, to))) => {
            let s = london_to_utc(start.and_time(from));
            let e = to
                .map(|t| london_to_utc(start.and_time(t)))
                .filter(|e| *e > s);
            (s, e, false)
        }
        _ => (london_midnight(start), end.map(london_midnight), true),
    };
    Ok(Some(NewEvent {
        sessions: Vec::new(),
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
        price: Price::default(),
        url: p["url"].as_str().map(str::to_string),
        image_url: p["image_url"].as_str().map(str::to_string),
        category,
        tags: tags
            .iter()
            .map(|t| clean_text(t).to_lowercase())
            .filter(|t| !t.is_empty())
            .collect(),
    }))
}

#[async_trait]
impl Source for EstorickCollection {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let mut out: Vec<RawEvent> = Vec::new();
        let events = ctx.get_text(&self.url(EVENTS_PATH)?).await?;
        let cards = parse_listing(&events, Section::Events);
        if cards.is_empty() {
            return Err(SourceError::Parse("no event cards on /events".into()));
        }
        out.extend(
            cards
                .iter()
                .map(|c| raw_event(c, Section::Events, &self.base_url)),
        );
        for path in [EXHIBITIONS_PATH, FUTURE_EXHIBITIONS_PATH] {
            let html = match ctx.get_text(&self.url(path)?).await {
                Ok(h) => h,
                Err(e) => {
                    ctx.report_error(format!("{path}: {e}"));
                    continue;
                }
            };
            for card in parse_listing(&html, Section::Exhibitions) {
                let raw = raw_event(&card, Section::Exhibitions, &self.base_url);
                if !out.iter().any(|r| r.source_event_id == raw.source_event_id) {
                    out.push(raw);
                }
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

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn payload(section: &str, date: &str, time: Option<&str>, tags: &[&str]) -> Value {
        json!({
            "url": "https://www.estorickcollection.com/events/x",
            "section": section,
            "card": {"path": "/events/x", "title": "Thing", "date_text": date,
                     "time_text": time, "description": null, "tags": tags, "image": null},
            "image_url": null,
        })
    }

    #[test]
    fn date_lines() {
        assert_eq!(
            parse_date_text("27 September 2026"),
            Some((d(2026, 9, 27), None, false))
        );
        assert_eq!(
            parse_date_text("16 September 2026 - 20 December 2026"),
            Some((d(2026, 9, 16), Some(d(2026, 12, 20)), false))
        );
        assert_eq!(
            parse_date_text("20 November - 10 January 2027"),
            Some((d(2026, 11, 20), Some(d(2027, 1, 10)), false))
        );
        assert_eq!(
            parse_date_text("From 5 March 2027"),
            Some((d(2027, 3, 5), None, true))
        );
        assert_eq!(parse_date_text("10:00 - 12:00"), None);
        assert_eq!(parse_date_text("Wanda Wulz (1903–1984) and more"), None);
    }

    #[test]
    fn time_lines() {
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert_eq!(
            parse_time_text("10:00 - 12:00"),
            Some((t(10, 0), Some(t(12, 0))))
        );
        assert_eq!(parse_time_text("18:30"), Some((t(18, 30), None)));
        assert_eq!(parse_time_text("27 September 2026"), None);
        assert_eq!(parse_time_text("Join us at 18:30"), None);
    }

    #[test]
    fn card_paths() {
        assert_eq!(
            card_path("/events/uncelebrated-venice", Section::Events).as_deref(),
            Some("/events/uncelebrated-venice")
        );
        assert_eq!(card_path("/events/under/talk", Section::Events), None);
        assert_eq!(card_path("/events", Section::Events), None);
        assert_eq!(
            card_path("/exhibitions/by/afro", Section::Exhibitions),
            None
        );
    }

    #[test]
    fn categories() {
        let c = |tags: &[&str], title| {
            event_category(
                &tags.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                title,
            )
        };
        assert_eq!(
            c(
                &["FAMILIES", "UNDER 5S", "WANDA AND MARION WULZ"],
                "Family Art Day"
            ),
            None
        );
        assert_eq!(c(&["TALK", "TOUR"], "Unlocked"), Some(Category::Talk));
        assert_eq!(c(&["TOUR"], "Out of Hours Tour"), Some(Category::Talk));
        assert_eq!(
            c(&["ADULT ART CLASS", "LIFE DRAWING CLASS"], "Life Drawing"),
            Some(Category::Workshop)
        );
        assert_eq!(c(&["SPECIAL EVENT"], "Launch"), Some(Category::Community));
        assert_eq!(
            c(&[], "Casting Modernity - Symposium"),
            Some(Category::Talk)
        );
    }

    #[test]
    fn start_only_time_has_no_end() {
        let ev = normalise_payload(&payload(
            "events",
            "7 October 2026",
            Some("18:30"),
            &["TOUR"],
        ))
        .unwrap()
        .unwrap();
        assert!(!ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-10-07T17:30:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn a_day_without_a_time_is_all_day() {
        let ev = normalise_payload(&payload("events", "12 November 2026", None, &["TALK"]))
            .unwrap()
            .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-11-12T00:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn open_ended_from_date_is_all_day_without_end() {
        let ev = normalise_payload(&payload("exhibitions", "From 1 October 2026", None, &[]))
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
            "16 September 2026 - 20 December 2026",
            None,
            &["EXHIBITION"],
        ))
        .unwrap()
        .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-15T23:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2026-12-20T00:00:00+00:00")
        );
    }

    #[test]
    fn missing_date_is_an_error_and_long_runs_are_skipped() {
        let err = normalise_payload(&payload("events", "", None, &["TALK"])).unwrap_err();
        assert!(err.to_string().contains("no date"), "{err}");
        let long = payload("exhibitions", "1 January 2026 - 1 January 2028", None, &[]);
        assert_eq!(normalise_payload(&long).unwrap(), None);
    }
}
