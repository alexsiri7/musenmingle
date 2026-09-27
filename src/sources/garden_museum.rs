//! Garden Museum (Lambeth) — hand-written scraper over the what's-on listing's
//! embedded JSON and the event pages' HTML.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): Yoast's
//!   `User-agent: *` / `Disallow:` (empty), so everything is allowed.
//! * The only JSON-LD is a Yoast `WebPage` graph with no `Event`. The
//!   `/whats-on/` cards are drawn by JavaScript, but from data the server
//!   embeds in the page as `<script id="json-data" type="application/json">`
//!   (an array of `{title, link, image_url, primary_type_slug, date_range,
//!   dates, is_free, …}`), so no JavaScript is needed; that array is the
//!   index. Its `dates` are unreliable as ends (single days carry the next
//!   day as well), so dates are not taken from it.
//! * Every in-scope item's page (`/whats-on/<slug>/`) is fetched, up to
//!   [`MAX_DETAIL_PAGES`] per run (the listing is soonest first), for the
//!   header date line (`6 Oct 2026, 6:30pm - 7:30pm`, `8 Oct - 20 Dec 2026`,
//!   `10 Oct - 14 Oct 2026, 11am - 1:45pm`), the sidebar location and
//!   "Booking information", the description (standfirst + body text) and
//!   `og:image`. Times are London
//!   wall-clock times.
//! * Categories come from the listing's event types: exhibitions → exhibition,
//!   talks → talk, workshops → workshop, festivals and lates → community.
//!   Items whose only type is `livestreams` (online) or another type are
//!   skipped without fetching their page. A talk or workshop spanning several
//!   days with no time is a series overview whose sessions are listed on
//!   their own ("Garden/Art/Garden"), and is skipped (`Ok(None)`).
//! * Price: the listing's `is_free` flag decides free entry. Otherwise only
//!   booking lines with a currency amount count, minus livestream tickets;
//!   "Friends go free!" on a ticketed exhibition must not read as free.
//!   Multi-date workshops ("Saturday 10th or Wednesday 14th") are stored as a
//!   single range from the first session's start to the last one's end.
//! * Venue: the museum, unless the sidebar names another place ("South
//!   London" for a bike ride), which is kept as the venue name without an
//!   address or coordinates.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, parse_price};

pub const KEY: &str = "garden-museum";
/// Upper bound on event pages fetched per run (≈ 2 min at 1 req / 2 s). The
/// listing had 57 items on 2026-09-26; it is sorted soonest first, so any
/// overflow is the furthest-out events, picked up on later runs.
pub const MAX_DETAIL_PAGES: usize = 60;
const LISTING_PATH: &str = "/whats-on/";
const SITE_HOSTS: [&str; 2] = ["www.gardenmuseum.org.uk", "gardenmuseum.org.uk"];
const VENUE_NAME: &str = "Garden Museum";
const VENUE_ADDRESS: &str = "5 Lambeth Palace Road, London SE1 7LB";
/// Approximate location of the building.
const VENUE_LAT: f64 = 51.4947;
const VENUE_LNG: f64 = -0.1199;
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

pub struct GardenMuseum {
    base_url: Url,
    max_detail_pages: usize,
}

impl GardenMuseum {
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

/// One entry of the listing's embedded JSON.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ListingItem {
    /// `/whats-on/<slug>/`.
    pub path: String,
    pub title: String,
    /// The listing's `primary_type_slug`s ("talks", "livestreams", …).
    pub types: Vec<String>,
    pub audience: Vec<String>,
    pub is_free: bool,
    pub image_url: Option<String>,
}

impl ListingItem {
    pub fn slug(&self) -> &str {
        self.path
            .trim_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
    }
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the listing page's `script#json-data` array, in document order,
/// de-duplicated by path. A page without the script yields no items; a
/// script that is not a JSON array is an error.
pub fn parse_listing(html: &str) -> Result<Vec<ListingItem>, SourceError> {
    let doc = Html::parse_document(html);
    let Some(script) = doc.select(&selector("script#json-data")).next() else {
        return Ok(Vec::new());
    };
    let text: String = script.text().collect();
    let data: Value = serde_json::from_str(text.trim())
        .map_err(|e| SourceError::Parse(format!("listing json-data: {e}")))?;
    let items = data
        .as_array()
        .ok_or_else(|| SourceError::Parse("listing json-data is not an array".into()))?;
    let mut out: Vec<ListingItem> = Vec::new();
    for item in items {
        let Some(Ok(u)) = item.get("link").and_then(Value::as_str).map(Url::parse) else {
            continue;
        };
        if !u.host_str().is_some_and(|h| SITE_HOSTS.contains(&h)) || u.query().is_some() {
            continue;
        }
        let Some(slug) = u
            .path()
            .strip_prefix("/whats-on/")
            .and_then(|s| s.strip_suffix('/'))
        else {
            continue;
        };
        let valid = !slug.is_empty()
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        let path = format!("/whats-on/{slug}/");
        if !valid || out.iter().any(|i| i.path == path) {
            continue;
        }
        out.push(ListingItem {
            path,
            title: clean_text(item.get("title").and_then(Value::as_str).unwrap_or("")),
            types: str_list(item.get("primary_type_slug").unwrap_or(&Value::Null)),
            audience: str_list(item.get("audience").unwrap_or(&Value::Null)),
            is_free: item.get("is_free").and_then(Value::as_bool) == Some(true),
            image_url: item
                .get("image_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        });
    }
    Ok(out)
}

/// Our category for a listing's event types (`None`: out of scope, e.g. a
/// livestream-only talk). The most specific type wins.
pub fn category_for_types<S: AsRef<str>>(types: &[S]) -> Option<Category> {
    let has = |t: &str| types.iter().any(|x| x.as_ref() == t);
    [
        ("exhibitions", Category::Exhibition),
        ("talks", Category::Talk),
        ("workshops", Category::Workshop),
        ("festivals", Category::Community),
        ("lates", Category::Community),
    ]
    .into_iter()
    .find(|(t, _)| has(t))
    .map(|(_, c)| c)
}

/// The sidebar's "Booking information" lines: the elements after its `h3`
/// up to the next separator, split at `<br>`.
fn booking_lines(sidebar: ElementRef<'_>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_booking = false;
    for child in sidebar.children().filter_map(ElementRef::wrap) {
        let name = child.value().name();
        if name == "h3" {
            in_booking = element_text(child)
                .to_lowercase()
                .contains("booking information");
            continue;
        }
        if !in_booking {
            continue;
        }
        if name != "p" {
            break;
        }
        let html = child.inner_html();
        let lower = html.to_lowercase();
        let mut rest = &html[..];
        let mut lower_rest = &lower[..];
        loop {
            let (piece, next) = match lower_rest.find("<br") {
                Some(i) => {
                    let end = lower_rest[i..]
                        .find('>')
                        .map_or(lower_rest.len(), |j| i + j + 1);
                    (&rest[..i], Some(end))
                }
                None => (rest, None),
            };
            let line = clean_text(piece);
            if !line.is_empty() {
                lines.push(line);
            }
            let Some(end) = next else { break };
            rest = &rest[end..];
            lower_rest = &lower_rest[end..];
        }
    }
    lines
}

/// The sidebar paragraph right after the bold date line: the location
/// ("Garden Museum", "South London"). Absent on some pages.
fn location_line(sidebar: ElementRef<'_>) -> Option<String> {
    let mut children = sidebar.children().filter_map(ElementRef::wrap);
    children.find(|e| {
        e.value().name() == "p"
            && e.value()
                .has_class("fw-sb", scraper::CaseSensitivity::CaseSensitive)
    })?;
    children
        .next()
        .filter(|e| e.value().name() == "p")
        .map(element_text)
        .filter(|t| !t.is_empty())
}

/// Parse one event page into a [`RawEvent`] (None if it has no title). `url`
/// is the address the page was fetched from; `item` is its listing entry.
pub fn parse_detail(html: &str, url: &Url, item: &ListingItem) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let first_text = |s: &str| {
        doc.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let title = first_text("h1.page--header__title")?;
    let date_text = first_text("p.page--header__date");
    let sidebar = doc.select(&selector(".page-introduction__sidebar")).next();
    let location = sidebar.and_then(location_line);
    let booking = sidebar.map(booking_lines).unwrap_or_default();
    // The standfirst (on some pages only) followed by the body text.
    let description_parts: Vec<String> = doc
        .select(&selector(
            ".page-introduction__intro, .page-introduction__text",
        ))
        .map(|e| format!("<p>{}</p>", e.inner_html()))
        .collect();
    let description = (!description_parts.is_empty()).then(|| description_parts.join("\n"));
    let image_url = doc
        .select(&selector(r#"meta[property="og:image"]"#))
        .next()
        .and_then(|e| e.value().attr("content"))
        .map(str::to_string)
        .or_else(|| item.image_url.clone());
    let slug = url
        .path()
        .trim_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    Some(RawEvent {
        source_event_id: slug,
        source_url: Some(url.to_string()),
        payload: json!({
            "url": url.as_str(),
            "title": title,
            "types": item.types,
            "audience": item.audience,
            "listing_is_free": item.is_free,
            "date_text": date_text,
            "location": location,
            "booking": booking,
            "description": description,
            "image_url": image_url,
        }),
    })
}

/// A parsed header date line: first and last day, with optional start and
/// end times (applying to the first and last day respectively).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateLine {
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
}

/// Parse a header date line: `6 Oct 2026, 6:30pm - 7:30pm`,
/// `8 Oct - 20 Dec 2026`, `2 - 30 Oct 2026`, `10 Oct - 14 Oct 2026, 11am`.
/// The last day must carry its year; a start without one takes the end's
/// year (the year before if that would put it after the end).
pub fn parse_date_line(text: &str) -> Result<DateLine, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {text:?}"));
    let lower = clean_text(text).to_lowercase();
    let (dates, times) = match lower.split_once(',') {
        Some((d, t)) => (d.trim(), Some(t.trim())),
        None => (lower.trim(), None),
    };
    let (start, end) = match split_range(dates) {
        Some((s, e)) => (Some(s), e),
        None => (None, dates),
    };
    let (end_day, end_month, Some(end_year)) = parse_day(end).ok_or_else(err)? else {
        return Err(err());
    };
    let Some(end_month) = end_month else {
        return Err(err());
    };
    let last_day = NaiveDate::from_ymd_opt(end_year, end_month, end_day).ok_or_else(err)?;
    let first_day = match start {
        None => last_day,
        Some(s) => {
            let (day, month, year) = parse_day(s).ok_or_else(err)?;
            let month = month.unwrap_or(end_month);
            match year {
                Some(y) => NaiveDate::from_ymd_opt(y, month, day).ok_or_else(err)?,
                None => {
                    let d = NaiveDate::from_ymd_opt(last_day.year(), month, day).ok_or_else(err)?;
                    if d > last_day {
                        NaiveDate::from_ymd_opt(last_day.year() - 1, month, day).ok_or_else(err)?
                    } else {
                        d
                    }
                }
            }
        }
    };
    if first_day > last_day {
        return Err(err());
    }
    let (start_time, end_time) = match times {
        None => (None, None),
        Some(t) => match split_range(t) {
            Some((a, b)) => (
                Some(parse_time(a).ok_or_else(err)?),
                Some(parse_time(b).ok_or_else(err)?),
            ),
            None => (Some(parse_time(t).ok_or_else(err)?), None),
        },
    };
    Ok(DateLine {
        first_day,
        last_day,
        start_time,
        end_time,
    })
}

fn split_range(s: &str) -> Option<(&str, &str)> {
    ["-", "–", "—"]
        .iter()
        .find_map(|sep| s.split_once(sep))
        .map(|(a, b)| (a.trim(), b.trim()))
}

/// "[tue] 7 [oct [2026]]" (lower-cased) → (day, month, year).
fn parse_day(s: &str) -> Option<(u32, Option<u32>, Option<i32>)> {
    let mut tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens
        .first()
        .is_some_and(|t| t.chars().all(|c| c.is_ascii_alphabetic()))
    {
        tokens.remove(0);
    }
    let (day, month, year) = match tokens.as_slice() {
        [d] => (d, None, None),
        [d, m] => (d, Some(m), None),
        [d, m, y] => (d, Some(m), Some(y.parse().ok()?)),
        _ => return None,
    };
    let day: u32 = day
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .parse()
        .ok()?;
    let month = match month {
        Some(m) => Some(MONTHS.iter().position(|p| m.get(..3) == Some(*p))? as u32 + 1),
        None => None,
    };
    Some((day, month, year))
}

/// "6:30pm", "11am", "12pm" (noon), "12am" (midnight).
fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim().replace(' ', "");
    let (clock, pm) = if let Some(c) = s.strip_suffix("pm") {
        (c, true)
    } else {
        (s.strip_suffix("am")?, false)
    };
    let (h, m) = match clock.split_once([':', '.']) {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (clock.parse::<u32>().ok()?, 0),
    };
    if !(1..=12).contains(&h) {
        return None;
    }
    let h = match (h, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, true) => h + 12,
        (h, false) => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Price from the booking lines. The listing's `is_free` flag decides free
/// entry; otherwise only lines with a currency amount count, minus
/// livestream tickets, so "Friends go free!" never reads as free.
pub fn price_from_booking(lines: &[String], listing_is_free: bool) -> Price {
    if listing_is_free {
        return parse_price("Free");
    }
    let kept: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|l| {
            let lower = l.to_lowercase();
            !lower.contains("livestream") && !lower.contains("live stream")
        })
        .filter(|l| parse_price(l).currency.is_some())
        .collect();
    if kept.is_empty() {
        return Price::default();
    }
    let mut price = parse_price(&kept.join("; "));
    price.is_free = false;
    price
}

/// Normalise a Garden Museum [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event page without title".into()))?;
    let types = str_list(payload.get("types").unwrap_or(&Value::Null));
    let Some(category) = category_for_types(&types) else {
        return Ok(None);
    };
    let location = payload
        .get("location")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|l| !l.is_empty());
    if location
        .as_deref()
        .is_some_and(|l| l.to_lowercase().contains("online"))
    {
        return Ok(None);
    }
    let date_text = payload
        .get("date_text")
        .and_then(Value::as_str)
        .ok_or_else(|| SourceError::Parse("event page without a date line".into()))?;
    let line = parse_date_line(date_text)?;
    if category != Category::Exhibition
        && line.first_day != line.last_day
        && line.start_time.is_none()
    {
        // A series overview; its sessions are listed individually.
        return Ok(None);
    }
    let starts_at = london_to_utc(
        line.first_day
            .and_time(line.start_time.unwrap_or(NaiveTime::MIN)),
    );
    let ends_at = match (line.start_time, line.end_time) {
        (_, Some(end)) => Some(london_to_utc(line.last_day.and_time(end))),
        // Sessions on several days with only a start time.
        (Some(start), None) => {
            (line.first_day != line.last_day).then(|| london_to_utc(line.last_day.and_time(start)))
        }
        (None, None) => Some(london_to_utc(line.last_day.and_time(NaiveTime::MIN))),
    };
    let booking = str_list(payload.get("booking").unwrap_or(&Value::Null));
    let listing_is_free = payload
        .get("listing_is_free")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let at_museum = location
        .as_deref()
        .is_none_or(|l| l.to_lowercase().contains("garden museum"));
    let (venue_name, address, lat, lng) = if at_museum {
        (
            Some(VENUE_NAME.to_string()),
            Some(VENUE_ADDRESS.to_string()),
            Some(VENUE_LAT),
            Some(VENUE_LNG),
        )
    } else {
        (location.clone(), None, None, None)
    };
    let mut tags = vec!["gardens".to_string()];
    if str_list(payload.get("audience").unwrap_or(&Value::Null))
        .iter()
        .any(|a| a == "family")
    {
        tags.push("family".to_string());
    }

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, venue_name.as_deref()),
        description: clean_description(payload.get("description").and_then(Value::as_str)),
        title,
        venue_name,
        address,
        lat,
        lng,
        starts_at,
        ends_at: ends_at.filter(|e| *e > starts_at),
        all_day: line.start_time.is_none() && line.end_time.is_none(),
        price: price_from_booking(&booking, listing_is_free),
        url: payload
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        image_url: payload
            .get("image_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        category,
        tags,
    }))
}

#[async_trait]
impl Source for GardenMuseum {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let items = parse_listing(&ctx.get_text(&url).await?)?;
        if items.is_empty() {
            return Err(SourceError::Parse(
                "no events found in the listing's json-data".into(),
            ));
        }
        let mut out = Vec::new();
        let in_scope = items
            .iter()
            .filter(|i| category_for_types(&i.types).is_some());
        for item in in_scope.take(self.max_detail_pages) {
            let path = &item.path;
            let url = match self.base_url.join(path) {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad event path {path}: {e}"));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html, &url, item) {
                    Some(raw) => out.push(raw),
                    None => ctx.report_error(format!("{path}: no page title")),
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
    use rust_decimal::Decimal;

    fn t(h: u32, m: u32) -> Option<NaiveTime> {
        NaiveTime::from_hms_opt(h, m, 0)
    }

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn line(a: &str, b: &str, st: Option<NaiveTime>, et: Option<NaiveTime>) -> DateLine {
        DateLine {
            first_day: d(a),
            last_day: d(b),
            start_time: st,
            end_time: et,
        }
    }

    #[test]
    fn parses_header_date_lines() {
        let cases = [
            (
                "6 Oct 2026, 6:30pm - 7:30pm",
                line("2026-10-06", "2026-10-06", t(18, 30), t(19, 30)),
            ),
            (
                "18 Oct 2026, 10am - 12pm",
                line("2026-10-18", "2026-10-18", t(10, 0), t(12, 0)),
            ),
            (
                "10 Oct - 14 Oct 2026, 11am - 1:45pm",
                line("2026-10-10", "2026-10-14", t(11, 0), t(13, 45)),
            ),
            (
                "8 Oct - 20 Dec 2026",
                line("2026-10-08", "2026-12-20", None, None),
            ),
            (
                "2 - 30 Oct 2026",
                line("2026-10-02", "2026-10-30", None, None),
            ),
            (
                "14 Nov - 10 Jan 2027",
                line("2026-11-14", "2027-01-10", None, None),
            ),
            (
                "29 Sep 2026, 7pm",
                line("2026-09-29", "2026-09-29", t(19, 0), None),
            ),
            (
                "1 Dec 2026, 12am - 12:30am",
                line("2026-12-01", "2026-12-01", t(0, 0), t(0, 30)),
            ),
            (
                "Sun 18 October 2026, 11am – 4pm",
                line("2026-10-18", "2026-10-18", t(11, 0), t(16, 0)),
            ),
        ];
        for (text, want) in cases {
            assert_eq!(parse_date_line(text).unwrap(), want, "{text}");
        }
    }

    #[test]
    fn unrecognised_date_lines_are_errors() {
        for text in [
            "Autumn 2026",
            "7 Oct",
            "7 Oct - 8 Nov",
            "20 Dec 2026 - 8 Oct 2026",
            "6 Oct 2026, 18:30",
            "6 Oct 2026, 13pm",
            "",
        ] {
            assert!(parse_date_line(text).is_err(), "{text:?}");
        }
    }

    fn payload(types: &[&str], date_text: &str) -> Value {
        json!({
            "title": "Example",
            "url": "https://www.gardenmuseum.org.uk/whats-on/example/",
            "types": types,
            "date_text": date_text,
            "location": "Garden Museum",
            "booking": ["£10 Standard"],
            "listing_is_free": false,
        })
    }

    #[test]
    fn series_overviews_and_livestream_only_items_are_skipped() {
        assert!(
            normalise_payload(&payload(&["talks"], "15 Sep - 1 Dec 2026"))
                .unwrap()
                .is_none()
        );
        assert!(
            normalise_payload(&payload(&["livestreams"], "6 Oct 2026, 6:30pm - 7:30pm"))
                .unwrap()
                .is_none()
        );
        let mut online = payload(&["talks"], "6 Oct 2026, 6:30pm - 7:30pm");
        online["location"] = json!("Online");
        assert!(normalise_payload(&online).unwrap().is_none());
        // An exhibition's date-only range is kept.
        let e = normalise_payload(&payload(&["exhibitions"], "8 Oct - 20 Dec 2026"))
            .unwrap()
            .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-07T23:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-12-20T00:00:00+00:00");
    }

    #[test]
    fn a_page_without_a_date_line_is_an_error() {
        let mut p = payload(&["talks"], "");
        p["date_text"] = Value::Null;
        assert!(normalise_payload(&p).is_err());
    }

    #[test]
    fn categories_prefer_the_specific_type() {
        assert_eq!(
            category_for_types(&["festivals", "talks"]),
            Some(Category::Talk)
        );
        assert_eq!(
            category_for_types(&["livestreams", "talks"]),
            Some(Category::Talk)
        );
        assert_eq!(category_for_types(&["lates"]), Some(Category::Community));
        assert_eq!(category_for_types(&["livestreams"]), None);
        assert_eq!(category_for_types::<&str>(&[]), None);
    }

    #[test]
    fn booking_prices_ignore_friends_and_livestreams() {
        let lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Ticketed exhibition whose only price wording is "Friends go free!".
        let p = price_from_booking(
            &lines(&[
                "Tickets include entry to the whole museum and gardens",
                "Friends go free! Become a Friend",
            ]),
            false,
        );
        assert_eq!(p, Price::default());
        let p = price_from_booking(
            &lines(&["£25 Standard", "£20 Friends", "£10 Livestream"]),
            false,
        );
        assert!(!p.is_free);
        assert_eq!(p.min, Some(Decimal::from(20)));
        assert_eq!(p.max, Some(Decimal::from(25)));
        let p = price_from_booking(&lines(&["Free. No booking required"]), true);
        assert!(p.is_free);
    }
}
