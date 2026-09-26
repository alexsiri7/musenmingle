//! The Courtauld — exhibitions at the Courtauld Gallery (Somerset House) and
//! public talks at the Courtauld's Vernon Square campus.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): Yoast's
//!   `User-agent: *` / `Disallow:` (empty), so everything is allowed.
//! * `/whats-on/` renders its full listing with JavaScript: the page's own
//!   script asks `GET /wp-json/unt/v1/fetch-post-data?listing_type=whats-on`
//!   for the ids of the current programme (a plain JSON array) and then POSTs
//!   them to a render endpoint. We only issue GETs: the id list, then the
//!   standard WordPress REST API
//!   (`/wp-json/wp/v2/events?include=<ids>&_fields=id,link,class_list`) for
//!   each item's permalink and taxonomy classes, then the server-rendered
//!   detail pages. No page has `Event` JSON-LD (only a Yoast `WebPage`
//!   graph), so detail pages are read with CSS selectors.
//! * Scope is decided from the REST `class_list` before any detail page is
//!   fetched: online items, short courses, young people's and members'
//!   programmes are skipped; gallery exhibitions/displays become
//!   exhibitions and lectures/research seminars/conferences become talks.
//!   The venue comes from the location class (`locations-gallery` →
//!   Somerset House, `locations-vernon-square` → Vernon Square); items with
//!   neither are skipped rather than placed at the wrong address.
//! * The detail sidebar has one row per fact, each marked by an inline SVG
//!   icon (calendar, clock, ticket, pin); rows are identified by the icon's
//!   `viewBox`, not by position (exhibitions have no ticket row).
//! * Dates: `2 October 2026 – 10 January 2027` / `2 Oct 2026 - 10 Jan 2027`
//!   ranges (parsed with the Whitechapel Gallery range parser) are stored as
//!   London midnight of the first and last day. A single day
//!   (`2 Dec 2026`) with an `18:00 - 19:30` time row gets London wall-clock
//!   start and end times; if the time row is unparseable the day's midnight
//!   is used. Open-ended text ("Open daily", "Until …") is a skip.
//! * Price comes only from the ticket row ("Free, booking essential",
//!   "£35.00 …"); exhibitions have none (tickets are sold by a JavaScript
//!   booking system), so their price is unknown.
//! * `og:image` is the event image, except on pages without one, where it is
//!   the generic Courtauld social card; that logo is dropped.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::whitechapel_gallery::parse_date_range;
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, parse_price};

pub const KEY: &str = "courtauld";
/// Upper bound on detail pages fetched per run (≈ 90 s at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 45;
/// WordPress REST `per_page` maximum.
const REST_PAGE_SIZE: usize = 100;
const IDS_PATH: &str = "/wp-json/unt/v1/fetch-post-data?listing_type=whats-on";
const REST_PATH: &str = "/wp-json/wp/v2/events";
const SITE_HOST: &str = "courtauld.ac.uk";
/// File name of the generic social card used as `og:image` on pages without
/// an image of their own.
const LOGO_IMAGE: &str = "Social-Courtauld";

/// Where a listing item takes place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Venue {
    Gallery,
    VernonSquare,
}

impl Venue {
    fn key(self) -> &'static str {
        match self {
            Venue::Gallery => "gallery",
            Venue::VernonSquare => "vernon-square",
        }
    }

    fn from_key(s: &str) -> Option<Self> {
        match s {
            "gallery" => Some(Venue::Gallery),
            "vernon-square" => Some(Venue::VernonSquare),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Venue::Gallery => "The Courtauld Gallery",
            Venue::VernonSquare => "The Courtauld, Vernon Square",
        }
    }

    fn address(self) -> &'static str {
        match self {
            Venue::Gallery => "Somerset House, Strand, London WC2R 0RN",
            Venue::VernonSquare => "Vernon Square, Penton Rise, London WC1X 9EW",
        }
    }

    /// Approximate location of the building.
    fn lat_lng(self) -> (f64, f64) {
        match self {
            Venue::Gallery => (51.5111, -0.1174),
            Venue::VernonSquare => (51.5298, -0.1164),
        }
    }
}

/// One in-scope item of the current programme.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ListingItem {
    pub id: u64,
    /// Path of the detail page (`/whats-on/<slug>/`).
    pub path: String,
    pub category: Category,
    #[serde(serialize_with = "ser_venue")]
    pub venue: Venue,
}

fn ser_venue<S: serde::Serializer>(v: &Venue, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(v.key())
}

pub struct Courtauld {
    base_url: Url,
}

impl Courtauld {
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

/// Parse the `fetch-post-data` response: the ids of the current programme.
pub fn parse_ids(body: &Value) -> Result<Vec<u64>, SourceError> {
    let items = body
        .as_array()
        .ok_or_else(|| SourceError::Parse("programme id list is not an array".into()))?;
    let mut ids: Vec<u64> = Vec::new();
    for v in items {
        let id = v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .ok_or_else(|| SourceError::Parse(format!("bad programme id {v}")))?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Decide from an item's WordPress `class_list` whether it is in scope, and
/// as what: `None` for online items, courses, young people's and members'
/// programmes, unrecognised genres and unknown locations.
pub fn classify(classes: &[&str]) -> Option<(Category, Venue)> {
    let has = |c: &str| classes.contains(&c);
    let genre = |g: &str| has(&format!("event-genres-{g}"));
    if has("locations-online")
        || has("audiences-members")
        || has("audiences-young-people")
        || [
            "online",
            "short-courses",
            "young-people",
            "public-programmes",
            "friends",
        ]
        .iter()
        .any(|g| genre(g))
    {
        return None;
    }
    let venue = if has("locations-vernon-square") {
        Venue::VernonSquare
    } else if has("locations-gallery") {
        Venue::Gallery
    } else {
        return None;
    };
    let is_talk = ["lecture", "research", "talks", "conferences"]
        .iter()
        .any(|g| genre(g));
    let is_exhibition = ["gallery", "display", "exhibition", "exhibitions"]
        .iter()
        .any(|g| genre(g));
    let category = if is_talk {
        Category::Talk
    } else if venue == Venue::Gallery
        && (is_exhibition || !classes.iter().any(|c| c.starts_with("event-genres-")))
    {
        // Gallery items without any genre are exhibitions too (the Vanessa
        // Bell display had only location classes).
        Category::Exhibition
    } else {
        return None;
    };
    Some((category, venue))
}

/// Parse the REST `events` response into in-scope listing items, keeping
/// the order of `ids` (the site's programme order).
pub fn parse_rest(body: &Value, ids: &[u64]) -> Result<Vec<ListingItem>, SourceError> {
    let items = body
        .as_array()
        .ok_or_else(|| SourceError::Parse("REST events response is not an array".into()))?;
    let mut found: Vec<ListingItem> = Vec::new();
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let Some(Ok(link)) = item.get("link").and_then(Value::as_str).map(Url::parse) else {
            continue;
        };
        if link.host_str() != Some(SITE_HOST) || !link.path().starts_with("/whats-on/") {
            continue;
        }
        let classes: Vec<&str> = item
            .get("class_list")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if let Some((category, venue)) = classify(&classes) {
            found.push(ListingItem {
                id,
                path: link.path().to_string(),
                category,
                venue,
            });
        }
    }
    found.sort_by_key(|i| ids.iter().position(|x| *x == i.id).unwrap_or(usize::MAX));
    Ok(found)
}

/// Which sidebar row an inline SVG icon marks.
fn row_kind(view_box: &str) -> Option<&'static str> {
    match view_box {
        "0 0 14 15" => Some("date"),
        "0 0 15 15" => Some("time"),
        "0 0 15 12" => Some("price"),
        "0 0 11 15" => Some("location"),
        _ => None,
    }
}

/// Whether a paragraph is entirely bold (the exhibitions' leading
/// "<date> / <room>" info line, which repeats the sidebar).
fn is_bold_info_line(p: ElementRef<'_>) -> bool {
    let bold: String = p
        .select(&selector("strong, b"))
        .map(element_text)
        .collect::<Vec<_>>()
        .join(" ");
    let text = element_text(p);
    !text.is_empty() && clean_text(&bold) == text
}

/// The intro block's HTML, without a leading all-bold info line.
fn description_html(doc: &Html) -> Option<String> {
    let intro = doc.select(&selector(".m-intro .m-entity__intro")).next()?;
    let mut parts: Vec<String> = Vec::new();
    for (i, child) in intro.children().filter_map(ElementRef::wrap).enumerate() {
        if i == 0 && child.value().name() == "p" && is_bold_info_line(child) {
            continue;
        }
        parts.push(child.html());
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Parse one detail page into a [`RawEvent`] (None if it has no title).
/// `url` is the address the page was fetched from.
pub fn parse_detail(html: &str, url: &Url, item: &ListingItem) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let title = doc
        .select(&selector("article.post-type-events h1"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty())?;
    let mut rows = serde_json::Map::new();
    for row in doc.select(&selector(".m-event-sidebar .detail--bold")) {
        let Some(kind) = row
            .select(&selector("svg"))
            .next()
            .and_then(|svg| svg.value().attr("viewBox"))
            .and_then(row_kind)
        else {
            continue;
        };
        let text = element_text(row);
        if !text.is_empty() && !rows.contains_key(kind) {
            rows.insert(kind.to_string(), Value::String(text));
        }
    }
    let row = |k: &str| rows.get(k).and_then(Value::as_str).map(str::to_string);
    let date_text = row("date");
    let image_url = doc
        .select(&selector(r#"meta[property="og:image"]"#))
        .next()
        .and_then(|e| e.value().attr("content"))
        .filter(|u| !u.contains(LOGO_IMAGE))
        .map(str::to_string);
    Some(RawEvent {
        source_event_id: item.id.to_string(),
        source_url: Some(url.to_string()),
        payload: json!({
            "url": url.as_str(),
            "title": title,
            "category": item.category.as_str(),
            "venue": item.venue.key(),
            "date_text": date_text,
            "time_text": row("time"),
            "price_text": row("price"),
            "location_text": row("location"),
            "description": description_html(&doc),
            "image_url": image_url,
        }),
    })
}

/// A single day such as `2 Dec 2026` or `Tuesday 2 December 2026`.
pub fn parse_single_date(text: &str) -> Option<NaiveDate> {
    let mut tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.first().is_some_and(|t| {
        t.trim_end_matches(',')
            .chars()
            .all(|c| c.is_ascii_alphabetic())
    }) {
        tokens.remove(0);
    }
    let [d, m, y] = tokens.as_slice() else {
        return None;
    };
    let month = m.get(..3)?;
    NaiveDate::parse_from_str(&format!("{d} {month} {y}"), "%d %b %Y").ok()
}

/// `18:00 - 19:30` → (start, Some(end)); `18:00` → (start, None).
pub fn parse_time_range(text: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    let clock = |s: &str| {
        let s = s.trim();
        let (h, m) = s.split_once([':', '.'])?;
        let m: String = m.chars().take_while(char::is_ascii_digit).collect();
        if m.len() != 2 {
            return None;
        }
        NaiveTime::from_hms_opt(h.trim().parse().ok()?, m.parse().ok()?, 0)
    };
    match text.split_once(['-', '–', '—']) {
        Some((a, b)) => Some((clock(a)?, Some(clock(b)?))),
        None => Some((clock(text)?, None)),
    }
}

/// Normalise a Courtauld [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let s = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = s("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("detail page without title".into()))?;
    let venue = s("venue")
        .and_then(Venue::from_key)
        .ok_or_else(|| SourceError::Parse("payload without venue".into()))?;
    let category = match s("category") {
        Some("talk") => Category::Talk,
        Some("exhibition") => Category::Exhibition,
        other => return Err(SourceError::Parse(format!("bad category {other:?}"))),
    };
    let Some(date_text) = s("date_text") else {
        return Ok(None);
    };
    let lower = date_text.to_lowercase();
    if ["until", "from", "ongoing", "open"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return Ok(None);
    }
    let (starts_at, ends_at) = if let Some(day) = parse_single_date(date_text) {
        match s("time_text").and_then(parse_time_range) {
            Some((start, end)) => (
                london_to_utc(day.and_time(start)),
                end.map(|e| london_to_utc(day.and_time(e))),
            ),
            None => (london_to_utc(day.and_time(NaiveTime::MIN)), None),
        }
    } else {
        let Some((first, last)) = parse_date_range(date_text)? else {
            return Ok(None);
        };
        (
            london_to_utc(first.and_time(NaiveTime::MIN)),
            Some(london_to_utc(last.and_time(NaiveTime::MIN))),
        )
    };
    let price: Price = s("price_text").map(parse_price).unwrap_or_default();
    let (lat, lng) = venue.lat_lng();
    let mut tags = vec!["art".to_string()];
    if category == Category::Talk {
        tags.push("art history".to_string());
    }

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(venue.name())),
        description: clean_description(s("description")),
        title,
        venue_name: Some(venue.name().to_string()),
        address: Some(venue.address().to_string()),
        lat: Some(lat),
        lng: Some(lng),
        starts_at,
        ends_at: ends_at.filter(|e| *e > starts_at),
        price,
        url: s("url").map(str::to_string),
        image_url: s("image_url").map(str::to_string),
        category,
        tags,
    }))
}

#[async_trait]
impl Source for Courtauld {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let join = |p: &str| {
            self.base_url
                .join(p)
                .map_err(|e| SourceError::Config(e.to_string()))
        };
        let ids = parse_ids(&ctx.get_json::<Value>(&join(IDS_PATH)?).await?)?;
        if ids.is_empty() {
            return Err(SourceError::Parse("the programme id list is empty".into()));
        }
        let mut items: Vec<ListingItem> = Vec::new();
        for chunk in ids.chunks(REST_PAGE_SIZE) {
            let include: Vec<String> = chunk.iter().map(u64::to_string).collect();
            let mut url = join(REST_PATH)?;
            url.query_pairs_mut()
                .append_pair("include", &include.join(","))
                .append_pair("per_page", &REST_PAGE_SIZE.to_string())
                .append_pair("_fields", "id,link,class_list");
            items.extend(parse_rest(&ctx.get_json::<Value>(&url).await?, &ids)?);
        }
        if items.is_empty() {
            // ~40 of ~50 items are in scope; none means the classes changed.
            return Err(SourceError::Parse(
                "no exhibitions or talks among the programme items".into(),
            ));
        }
        let mut out = Vec::new();
        for item in items.iter().take(MAX_DETAIL_PAGES) {
            let url = match self.base_url.join(&item.path) {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad detail path {}: {e}", item.path));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html, &url, item) {
                    Some(raw) => out.push(raw),
                    None => ctx.report_error(format!("{}: no page title", item.path)),
                },
                Err(e) => ctx.report_error(format!("{}: {e}", item.path)),
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

    fn payload(date: &str, time: Option<&str>, category: &str) -> Value {
        json!({
            "title": "A talk",
            "category": category,
            "venue": "vernon-square",
            "date_text": date,
            "time_text": time,
        })
    }

    fn times(date: &str, time: Option<&str>, category: &str) -> (String, Option<String>) {
        let e = normalise_payload(&payload(date, time, category))
            .unwrap()
            .unwrap();
        (e.starts_at.to_rfc3339(), e.ends_at.map(|t| t.to_rfc3339()))
    }

    #[test]
    fn single_day_talks_use_london_wall_clock() {
        // GMT
        assert_eq!(
            times("2 Dec 2026", Some("18:00 - 19:30"), "talk"),
            (
                "2026-12-02T18:00:00+00:00".into(),
                Some("2026-12-02T19:30:00+00:00".into())
            )
        );
        // BST
        assert_eq!(
            times("Tuesday 6 October 2026", Some("17:30–19:00"), "talk"),
            (
                "2026-10-06T16:30:00+00:00".into(),
                Some("2026-10-06T18:00:00+00:00".into())
            )
        );
        assert_eq!(
            times("6 Oct 2026", Some("18:00"), "talk"),
            ("2026-10-06T17:00:00+00:00".into(), None)
        );
        // Unparseable time: the day's midnight.
        assert_eq!(
            times("6 Oct 2026", Some("Evening"), "talk"),
            ("2026-10-05T23:00:00+00:00".into(), None)
        );
    }

    #[test]
    fn ranges_are_whole_days() {
        assert_eq!(
            times(
                "2 October 2026 – 10 January 2027",
                Some("10:00 – 18:00 (last entry 17:15)"),
                "exhibition"
            ),
            (
                "2026-10-01T23:00:00+00:00".into(),
                Some("2027-01-10T00:00:00+00:00".into())
            )
        );
        assert_eq!(
            times(
                "13 Nov - 14 Nov 2026",
                Some("Friday 17:30–20:00; Saturday 10:15–19:30"),
                "talk"
            ),
            (
                "2026-11-13T00:00:00+00:00".into(),
                Some("2026-11-14T00:00:00+00:00".into())
            )
        );
    }

    #[test]
    fn open_ended_and_missing_dates_are_skips() {
        for text in ["Open daily, 10:00 - 18:00", "Until 10 Jan 2027", "Ongoing"] {
            assert!(
                normalise_payload(&payload(text, None, "exhibition"))
                    .unwrap()
                    .is_none(),
                "{text}"
            );
        }
        let mut p = payload("x", None, "talk");
        p["date_text"] = Value::Null;
        assert!(normalise_payload(&p).unwrap().is_none());
    }

    #[test]
    fn unrecognised_dates_are_errors() {
        for text in ["Autumn 2026", "2 Dec"] {
            assert!(
                normalise_payload(&payload(text, None, "talk")).is_err(),
                "{text}"
            );
        }
    }

    #[test]
    fn classifies_programme_items() {
        let c = |s: &str| classify(&s.split_whitespace().collect::<Vec<_>>());
        assert_eq!(
            c("event-genres-gallery event-genres-exhibition locations-gallery"),
            Some((Category::Exhibition, Venue::Gallery))
        );
        assert_eq!(
            c("locations-gallery locations-the-project-space"),
            Some((Category::Exhibition, Venue::Gallery))
        );
        assert_eq!(
            c("event-genres-lecture event-genres-research locations-vernon-square"),
            Some((Category::Talk, Venue::VernonSquare))
        );
        for skipped in [
            "event-genres-short-courses locations-online",
            "event-genres-short-courses locations-lecture-theatre-1 locations-vernon-square",
            "event-genres-public-programmes event-genres-young-people locations-gallery audiences-young-people",
            "event-genres-friends locations-gallery audiences-members",
            "event-genres-lecture event-genres-research",
            "event-genres-lecture locations-online",
        ] {
            assert_eq!(c(skipped), None, "{skipped}");
        }
    }

    #[test]
    fn parses_single_dates_and_times() {
        assert_eq!(
            parse_single_date("2 Dec 2026"),
            NaiveDate::from_ymd_opt(2026, 12, 2)
        );
        assert_eq!(
            parse_single_date("Sat 12 September 2026"),
            NaiveDate::from_ymd_opt(2026, 9, 12)
        );
        assert_eq!(parse_single_date("2 Oct 2026 - 10 Jan 2027"), None);
        assert_eq!(parse_time_range("7pm"), None);
    }
}
