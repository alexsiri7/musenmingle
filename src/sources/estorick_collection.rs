//! Estorick Collection of Modern Italian Art (Canonbury, N1) — listing-only
//! CSS scraper over `/events`, `/exhibitions` (current) and
//! `/exhibitions/in-the/future`.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   with an empty `Disallow:`.
//! * There is no JSON-LD. Each listing card is a title link
//!   (`a[href^="/events/"] > b`, or for the current exhibition an `h2` link)
//!   followed in the same block by `c-tag` labels ("TALK", "ADULT ART
//!   CLASS", "EXHIBITION", …) and `<p>` lines: a date (`27 September
//!   2026`), for events a London wall-clock time (`10:00 - 12:00` or
//!   `18:30`), and a summary. Exhibitions carry a date-only range
//!   (`16 September 2026 - 20 December 2026`), stored `all_day`. The card
//!   block is found from the title link by walking up (at most
//!   [`MAX_CARD_DEPTH`] levels) to the first ancestor with a date line, so
//!   selectors don't depend on the generated grid classes. A card without a
//!   date line is a parse error. Three requests a run, no detail pages; the
//!   summary is the description.
//! * Category from the labels: "EXHIBITION" → exhibition; "TALK", "TOUR",
//!   "BOOK LAUNCH", "SYMPOSIUM" → talk; "ADULT ART CLASS", "LIFE DRAWING
//!   CLASS", "WORKSHOP" → workshop; "UNDER 5S" (early-years play) is skipped;
//!   otherwise title keywords ("Book Presentation", "Book Launch", then
//!   `map_category`: "Panel Discussion", "Symposium"), else community (special events, families, lates).

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, map_category};

pub const KEY: &str = "estorick-collection";
/// The listings read each run, in order.
pub const LISTING_PATHS: [&str; 3] = ["/events", "/exhibitions", "/exhibitions/in-the/future"];
/// How far up from a title link the card block may be.
pub const MAX_CARD_DEPTH: usize = 4;
const VENUE_NAME: &str = "Estorick Collection";
const VENUE_ADDRESS: &str = "39a Canonbury Square, London N1 2AN";
const VENUE_LAT: f64 = 51.5437;
const VENUE_LNG: f64 = -0.1003;

pub struct EstorickCollection {
    base_url: Url,
}

impl EstorickCollection {
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

/// When an item happens, from its date and time lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// First and last day (equal for a single day), no time given.
    Days(NaiveDate, NaiveDate),
    /// A day with a London wall-clock start and (if given) end time.
    Timed(NaiveDate, NaiveTime, Option<NaiveTime>),
}

fn parse_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(clean_text(s).as_str(), "%d %B %Y").ok()
}

/// A date line: one day or a range of days.
fn parse_days(line: &str) -> Option<(NaiveDate, NaiveDate)> {
    match line.split_once(" - ").or_else(|| line.split_once(" – ")) {
        Some((a, b)) => Some((parse_day(a)?, parse_day(b)?)),
        None => parse_day(line).map(|d| (d, d)),
    }
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// A time line: `10:00 - 12:00` or `18:30`.
fn parse_times(line: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    match line.split_once(['-', '–']) {
        Some((a, b)) => Some((parse_time(a)?, Some(parse_time(b)?))),
        None => Some((parse_time(line)?, None)),
    }
}

fn is_date_line(line: &str) -> bool {
    parse_days(line).is_some()
}

/// Parse a card's date line and optional time line.
pub fn parse_when(date: &str, time: Option<&str>) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date/time {date:?} {time:?}"));
    let (first, last) = parse_days(date).ok_or_else(err)?;
    if last < first {
        return Err(err());
    }
    match time {
        None => Ok(When::Days(first, last)),
        Some(_) if first != last => Err(err()),
        Some(t) => {
            let (start, end) = parse_times(t).ok_or_else(err)?;
            if end.is_some_and(|e| e <= start) {
                return Err(err());
            }
            Ok(When::Timed(first, start, end))
        }
    }
}

/// The cards of a listing page fetched from `page_url`: items whose link
/// starts with `/events/` or `/exhibitions/` (one path segment, not the
/// tag/filter pages), de-duplicated by path. A card whose block has no
/// date line is returned as an error.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<Result<RawEvent, SourceError>> {
    let doc = Html::parse_document(html);
    let p_sel = selector("p");
    let tag_sel = selector("a.c-tag");
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for a in doc.select(&selector(
        r#"main a[href^="/events/"], main a[href^="/exhibitions/"]"#,
    )) {
        let Some(href) = a.value().attr("href") else {
            continue;
        };
        let is_title = a.select(&selector("b")).next().is_some()
            || a.parent()
                .and_then(ElementRef::wrap)
                .is_some_and(|p| p.value().name() == "h2");
        let slug = href
            .strip_prefix("/events/")
            .or_else(|| href.strip_prefix("/exhibitions/"))
            .unwrap_or_default();
        if !is_title
            || slug.is_empty()
            || slug.contains(['/', '?', '#'])
            || seen.iter().any(|s| s == href)
        {
            continue;
        }
        seen.push(href.to_string());
        let title = element_text(a);
        let card = a
            .ancestors()
            .filter_map(ElementRef::wrap)
            .take(MAX_CARD_DEPTH)
            .find(|e| e.select(&p_sel).any(|p| is_date_line(&element_text(p))));
        let Some(card) = card else {
            out.push(Err(SourceError::Parse(format!("{href}: no date line"))));
            continue;
        };
        let lines: Vec<String> = card
            .select(&p_sel)
            .map(element_text)
            .filter(|t| !t.is_empty())
            .collect();
        let date = lines.iter().find(|l| is_date_line(l)).cloned();
        let time = lines.iter().find(|l| parse_times(l).is_some()).cloned();
        let summary = lines
            .iter()
            .find(|l| !is_date_line(l) && parse_times(l).is_none())
            .cloned();
        let tags: Vec<String> = card.select(&tag_sel).map(element_text).collect();
        let image = card
            .select(&selector("img[src]"))
            .next()
            .and_then(|i| i.value().attr("src"))
            .and_then(|s| page_url.join(s).ok())
            .map(|u| u.to_string());
        let url = match page_url.join(href) {
            Ok(u) => u,
            Err(e) => {
                out.push(Err(SourceError::Parse(format!("{href}: {e}"))));
                continue;
            }
        };
        out.push(Ok(RawEvent {
            source_event_id: href.trim_start_matches('/').to_string(),
            source_url: Some(url.to_string()),
            payload: json!({
                "url": url.as_str(),
                "title": title,
                "date": date,
                "time": time,
                "summary": summary,
                "tags": tags,
                "image": image,
            }),
        }));
    }
    out
}

/// The category for a card's labels and title; `None` skips it.
pub fn category(tags: &[String], title: &str) -> Option<Category> {
    let has = |label: &str| tags.iter().any(|t| t.eq_ignore_ascii_case(label));
    if has("UNDER 5S") {
        return None;
    }
    if has("EXHIBITION") {
        return Some(Category::Exhibition);
    }
    if ["TALK", "TOUR", "BOOK LAUNCH", "SYMPOSIUM"]
        .iter()
        .any(|l| has(l))
    {
        return Some(Category::Talk);
    }
    if ["ADULT ART CLASS", "LIFE DRAWING CLASS", "WORKSHOP"]
        .iter()
        .any(|l| has(l))
    {
        return Some(Category::Workshop);
    }
    let lower = title.to_lowercase();
    if ["presentation", "book launch"]
        .iter()
        .any(|k| lower.contains(k))
    {
        return Some(Category::Talk);
    }
    Some(map_category(&[title]).unwrap_or(Category::Community))
}

/// Normalise an Estorick Collection [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("card without a title".into()))?;
    let tags: Vec<String> = payload
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(clean_text).collect())
        .unwrap_or_default();
    let Some(category) = category(&tags, &title) else {
        return Ok(None);
    };
    let date = text("date").ok_or_else(|| SourceError::Parse(format!("{title:?}: no date")))?;
    let (starts_at, ends_at, all_day) = match parse_when(date, text("time"))? {
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
    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        title,
        description: clean_description(text("summary")),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day,
        price: Price::default(),
        url: text("url").map(str::to_string),
        image_url: text("image").map(str::to_string),
        category,
        tags: tags
            .iter()
            .map(|t| t.to_lowercase())
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
        for (i, path) in LISTING_PATHS.iter().enumerate() {
            let url = self
                .base_url
                .join(path)
                .map_err(|e| SourceError::Config(e.to_string()))?;
            let items = match ctx.get_text(&url).await {
                Ok(html) => parse_listing(&html, &url),
                // The events listing is the main page: its failure fails the run.
                Err(e) if i == 0 => return Err(e.into()),
                Err(e) => {
                    ctx.report_error(format!("{path}: {e}"));
                    continue;
                }
            };
            if i == 0 && items.is_empty() {
                return Err(SourceError::Parse("no event cards on /events".into()));
            }
            for item in items {
                match item {
                    Ok(raw) if out.iter().any(|r| r.source_event_id == raw.source_event_id) => {}
                    Ok(raw) => out.push(raw),
                    Err(e) => ctx.report_error(e.to_string()),
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

    fn hm(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn parses_dates_times_and_ranges() {
        assert_eq!(
            parse_when("27 September 2026", Some("10:00 - 12:00")).unwrap(),
            When::Timed(d(2026, 9, 27), hm(10, 0), Some(hm(12, 0)))
        );
        assert_eq!(
            parse_when("7 October 2026", Some("18:30")).unwrap(),
            When::Timed(d(2026, 10, 7), hm(18, 30), None)
        );
        assert_eq!(
            parse_when("16 September 2026 - 20 December 2026", None).unwrap(),
            When::Days(d(2026, 9, 16), d(2026, 12, 20))
        );
        assert_eq!(
            parse_when("23 November 2026", None).unwrap(),
            When::Days(d(2026, 11, 23), d(2026, 11, 23))
        );
    }

    #[test]
    fn bad_dates_are_errors() {
        for (date, time) in [
            ("Every Thursday", None),
            ("20 December 2026 - 16 September 2026", None),
            ("1 October 2026 - 3 October 2026", Some("10:00")),
            ("1 October 2026", Some("12:00 - 10:00")),
            ("1 October 2026", Some("7pm")),
        ] {
            assert!(parse_when(date, time).is_err(), "{date} {time:?}");
        }
    }

    fn norm(title: &str, tags: &[&str], date: &str, time: Option<&str>) -> Option<NewEvent> {
        normalise_payload(&json!({
            "url": "https://www.estorickcollection.com/events/x",
            "title": title, "date": date, "time": time, "summary": "About it.", "tags": tags,
        }))
        .unwrap()
    }

    #[test]
    fn times_are_london_wall_clock_across_the_clock_change() {
        let bst = norm("Talk", &["TALK"], "15 October 2026", Some("18:00 - 20:00")).unwrap();
        assert_eq!(bst.starts_at.to_rfc3339(), "2026-10-15T17:00:00+00:00");
        assert_eq!(
            bst.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-15T19:00:00+00:00")
        );
        assert!(!bst.all_day);
        let gmt = norm(
            "Class",
            &["LIFE DRAWING CLASS"],
            "18 November 2026",
            Some("18:30 - 20:30"),
        )
        .unwrap();
        assert_eq!(gmt.starts_at.to_rfc3339(), "2026-11-18T18:30:00+00:00");
        let day = norm("Symposium", &[], "23 November 2026", None).unwrap();
        assert!(day.all_day);
        assert_eq!(day.starts_at.to_rfc3339(), "2026-11-23T00:00:00+00:00");
        assert_eq!(day.ends_at, None);
    }

    #[test]
    fn categories_from_labels_then_title() {
        let cat = |title: &str, tags: &[&str]| {
            norm(title, tags, "1 October 2026", Some("10:00")).map(|e| e.category)
        };
        assert_eq!(cat("Mini Marvels", &["FAMILIES", "UNDER 5S"]), None);
        assert_eq!(cat("Show", &["EXHIBITION"]), Some(Category::Exhibition));
        assert_eq!(cat("Out of Hours Tour", &["TOUR"]), Some(Category::Talk));
        assert_eq!(
            cat("Life Drawing", &["ADULT ART CLASS"]),
            Some(Category::Workshop)
        );
        assert_eq!(
            cat(
                "Uncelebrated Venice - Book Presentation",
                &["SPECIAL EVENT"]
            ),
            Some(Category::Talk)
        );
        assert_eq!(
            cat("Summer Party", &["SPECIAL EVENT"]),
            Some(Category::Community)
        );
    }
}
