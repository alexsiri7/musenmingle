//! William Morris Gallery (Walthamstow) — hand-written scraper over the
//! "What's on" listing and the detail page of every listed event.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   with an empty `Disallow:`, no Crawl-delay.
//! * The only JSON-LD is Yoast's WebPage/ImageObject graph (no `Event`), so
//!   CSS selectors are used.
//! * `/whats-on/` shows featured events (`a.whats_on_hero`, where the current
//!   exhibition appears and nowhere else) and a grid (`#response a.card`) of
//!   at most [`GRID_PAGE`] cards. The site's "Load more" button fetches the
//!   rest from `/wp-json/williammorris/v1/filter?id=<page id>&offset=<n>`, a
//!   JSON string of more cards (or of a "No more items to see" notice with
//!   none), which is followed while it returns cards. Cards are de-duplicated
//!   by path; every card's detail page is fetched, since only it has times.
//! * A detail page has the title (`h2.single_event_title`), an optional
//!   subtitle (the session's theme, or "Multiple dates"), the dates
//!   (`p.single_event_type_dates`: "Saturday 10 October 2026" or "Saturday 3
//!   October 2026 - Sunday 28 March 2027"), sidebar blocks headed "Timings"
//!   (session times, "1:00pm to 4:00pm.", in `p.bottom_p`), "Fees" and
//!   "Other information", and the event's categories as `event-category-*`
//!   classes on the `<article>`. The sidebar nests `<p>` in `<p>`, which the
//!   parser splits into siblings, so blocks are read paragraph by paragraph.
//! * Times are London wall-clock; of several sessions ("10:00am to 11:00am.",
//!   "11:30am to 12:30pm.") the first is the event's time. A day with no
//!   session times is stored `all_day`, and so are exhibitions (both ends
//!   London midnight). Anything but an exhibition spanning more than one day
//!   is a run of sessions and skipped.
//! * Category from the article's classes: exhibitions first; online and
//!   off-site events skipped; then talks, workshops; tours, films and
//!   training courses skipped; late events, family, young people's, over-60s
//!   and special events → community; anything else skipped.
//! * Price is the first paragraph of "Fees", else of "Other information"
//!   ("FREE admission.", then "Suggested donation of £5." in the next
//!   paragraph, which is not a price).
//! * Every event is placed at the Gallery.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::parse_time_range;
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, parse_price};

pub const KEY: &str = "william-morris-gallery";
/// The most cards the listing's grid (or one "Load more" batch) holds before
/// more have to be fetched.
pub const GRID_PAGE: usize = 10;
/// Upper bound on "Load more" batches fetched per run.
pub const MAX_MORE_PAGES: usize = 5;
/// Upper bound on detail pages fetched per run (≈ 60 s at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 30;
const LISTING_PATH: &str = "/whats-on/";
const FILTER_PATH: &str = "/wp-json/williammorris/v1/filter";
const VENUE_NAME: &str = "William Morris Gallery";
const VENUE_ADDRESS: &str = "Lloyd Park, Forest Road, Walthamstow, London E17 4PP";
/// William Morris Gallery (OSM way 181770911).
const VENUE_LAT: f64 = 51.5913;
const VENUE_LNG: f64 = -0.0203;
const NO_SUBTITLE: &str = "Multiple dates";

pub struct WilliamMorrisGallery {
    base_url: Url,
}

impl WilliamMorrisGallery {
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

/// `/event/<slug>/` from a card's `href`.
fn event_path(href: &str) -> Option<String> {
    let url = Url::parse(href).ok()?;
    let slug = url.path().strip_prefix("/event/")?.strip_suffix('/')?;
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    valid.then(|| url.path().to_string())
}

/// Event paths of the cards matched by `card_selector`, plus a description
/// of each card whose link isn't an event page.
fn card_paths(doc: &Html, card_selector: &str) -> (Vec<String>, Vec<String>) {
    let mut paths = Vec::new();
    let mut rejected = Vec::new();
    for card in doc.select(&selector(card_selector)) {
        let href = card.value().attr("href").unwrap_or_default();
        match event_path(href) {
            Some(path) => paths.push(path),
            None => rejected.push(format!("unusable listing card link {href:?}")),
        }
    }
    (paths, rejected)
}

#[derive(Debug, serde::Serialize)]
pub struct Listing {
    /// Event paths, featured first, de-duplicated in page order.
    pub paths: Vec<String>,
    /// Cards in the grid, which decides whether "Load more" has any.
    pub grid_cards: usize,
    /// The id the "Load more" endpoint needs (`form#filter_form[data-id]`).
    pub page_id: Option<String>,
    pub rejected: Vec<String>,
}

pub fn parse_listing(html: &str) -> Listing {
    let doc = Html::parse_document(html);
    let (featured, mut rejected) = card_paths(&doc, "a.whats_on_hero");
    let (grid, grid_rejected) = card_paths(&doc, "#response a.card");
    rejected.extend(grid_rejected);
    let grid_cards = grid.len();
    let mut paths: Vec<String> = Vec::new();
    for path in featured.into_iter().chain(grid) {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    let page_id = doc
        .select(&selector("form#filter_form[data-id]"))
        .next()
        .and_then(|f| f.value().attr("data-id"))
        .map(str::to_string);
    Listing {
        paths,
        grid_cards,
        page_id,
        rejected,
    }
}

/// The cards of one "Load more" response (a JSON string of card HTML).
pub fn parse_more(body: &str) -> Result<(Vec<String>, Vec<String>), SourceError> {
    let html: String = serde_json::from_str(body)
        .map_err(|e| SourceError::Parse(format!("unreadable \"Load more\" response: {e}")))?;
    Ok(card_paths(&Html::parse_fragment(&html), "a.card"))
}

/// One event's detail page, as printed.
#[derive(Debug, serde::Serialize)]
pub struct Detail {
    pub title: String,
    pub subtitle: Option<String>,
    pub dates_text: Option<String>,
    /// Session times from the "Timings" block, in page order.
    pub sessions: Vec<String>,
    /// The `event-category-*` class suffixes of the article.
    pub categories: Vec<String>,
    /// First paragraph of "Fees", else of "Other information".
    pub price_text: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
}

/// The non-empty paragraphs of the sidebar block headed `heading`.
fn sidebar_paragraphs(doc: &Html, heading: &str) -> Vec<String> {
    doc.select(&selector("aside div.event_detail"))
        .find(|block| {
            block
                .select(&selector("h4"))
                .next()
                .is_some_and(|h| element_text(h).eq_ignore_ascii_case(heading))
        })
        .map(|block| {
            block
                .select(&selector("p"))
                .map(element_text)
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a detail page; `None` when it has no title.
pub fn parse_detail(html: &str) -> Option<Detail> {
    let doc = Html::parse_document(html);
    let first_text = |s: &str| {
        doc.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let title = first_text("h2.single_event_title")?;
    let categories = doc
        .select(&selector("article.event"))
        .next()
        .and_then(|a| a.value().attr("class"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|c| c.strip_prefix("event-category-"))
        .map(str::to_string)
        .collect();
    let sessions = doc
        .select(&selector("aside div.event_detail p.bottom_p"))
        .map(element_text)
        .filter(|t| !t.is_empty())
        .collect();
    let price_text = [
        sidebar_paragraphs(&doc, "Fees"),
        sidebar_paragraphs(&doc, "Other information"),
    ]
    .into_iter()
    .find_map(|ps| ps.into_iter().next());
    Some(Detail {
        title,
        subtitle: first_text("h3.single_event_subtitle"),
        dates_text: first_text("p.single_event_type_dates"),
        sessions,
        categories,
        price_text,
        description: first_text("div.event_description"),
        image_url: doc
            .select(&selector(".event_header_image img[src]"))
            .next()
            .and_then(|e| e.value().attr("src"))
            .map(str::to_string),
    })
}

/// "Saturday 3 October 2026".
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%A %d %B %Y").ok()
}

/// A date or date range into its first and last day.
pub fn parse_dates(s: &str) -> Option<(NaiveDate, NaiveDate)> {
    match s.split_once(['-', '–']) {
        Some((a, b)) => Some((parse_date(a)?, parse_date(b)?)),
        None => parse_date(s).map(|d| (d, d)),
    }
}

/// "1:00pm to 4:00pm." into start and optional end.
pub fn parse_session(s: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    parse_time_range(&s.trim().trim_end_matches('.').replace(" to ", "–"))
}

/// The in-scope category for an article's category classes (a suffix such
/// as `-2` on a duplicate slug is ignored), or `None` to skip it.
pub fn category<S: AsRef<str>>(classes: &[S]) -> Option<Category> {
    let has = |slugs: &[&str]| {
        classes.iter().any(|c| {
            let c = c.as_ref().trim_end_matches(|ch: char| ch.is_ascii_digit());
            let c = c.strip_suffix('-').unwrap_or(c);
            slugs.contains(&c)
        })
    };
    if has(&[
        "exhibition-type",
        "current-exhibitions",
        "upcoming-exhibitions",
    ]) {
        Some(Category::Exhibition)
    } else if has(&["online", "off-site"]) {
        None
    } else if has(&["talks-and-discussions"]) {
        Some(Category::Talk)
    } else if has(&["workshops"]) {
        Some(Category::Workshop)
    } else if has(&["tours", "film", "training-course"]) {
        None
    } else if has(&[
        "late-event",
        "families",
        "young-people",
        "over-60s",
        "special-events",
    ]) {
        Some(Category::Community)
    } else {
        None
    }
}

/// Normalise a William Morris Gallery [`RawEvent`] payload (a [`Detail`]
/// plus its absolute `url` and `image_url`).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let strings = |key: &str| -> Vec<&str> {
        payload
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    };
    let base_title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event without title".into()))?;
    let title = match text("subtitle").map(clean_text) {
        Some(sub) if !sub.is_empty() && !sub.eq_ignore_ascii_case(NO_SUBTITLE) => {
            format!("{base_title}: {sub}")
        }
        _ => base_title,
    };
    let Some(dates_text) = text("dates_text") else {
        return Ok(None);
    };
    let (first, last) = parse_dates(dates_text)
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: bad dates {dates_text:?}")))?;
    let Some(category) = category(&strings("categories")) else {
        return Ok(None);
    };
    let exhibition = category == Category::Exhibition;
    if !exhibition && last != first {
        return Ok(None);
    }
    let session = match strings("sessions").first() {
        Some(s) if !exhibition => Some(
            parse_session(s)
                .ok_or_else(|| SourceError::Parse(format!("{title:?}: bad session {s:?}")))?,
        ),
        _ => None,
    };
    let (starts_at, ends_at) = match session {
        Some((start, end)) => (
            london_to_utc(first.and_time(start)),
            end.filter(|e| *e > start)
                .map(|e| london_to_utc(first.and_time(e))),
        ),
        None => (
            london_to_utc(first.and_time(NaiveTime::MIN)),
            (last != first).then(|| london_to_utc(last.and_time(NaiveTime::MIN))),
        ),
    };

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(text("description")),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day: session.is_none(),
        price: text("price_text").map(parse_price).unwrap_or_default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: vec![],
    }))
}

/// The [`RawEvent`] for the detail page of `path`, fetched from `page_url`.
pub fn detail_event(path: &str, page_url: &Url, detail: &Detail) -> RawEvent {
    let mut payload = serde_json::to_value(detail).expect("detail serialises");
    payload["url"] = json!(page_url.as_str());
    payload["image_url"] = json!(
        detail
            .image_url
            .as_deref()
            .and_then(|i| page_url.join(i).ok())
            .map(String::from)
    );
    RawEvent {
        source_event_id: path.trim_matches('/').to_string(),
        source_url: Some(page_url.to_string()),
        payload,
    }
}

impl WilliamMorrisGallery {
    fn url(&self, path: &str) -> Result<Url, SourceError> {
        self.base_url
            .join(path)
            .map_err(|e| SourceError::Config(e.to_string()))
    }

    /// Paths of the grid cards past the first [`GRID_PAGE`], from the
    /// "Load more" endpoint.
    async fn more_paths(
        &self,
        ctx: &FetchContext,
        page_id: &str,
    ) -> Result<Vec<String>, SourceError> {
        let mut paths = Vec::new();
        let mut offset = GRID_PAGE;
        for _ in 0..MAX_MORE_PAGES {
            let mut url = self.url(FILTER_PATH)?;
            url.query_pairs_mut()
                .append_pair("id", page_id)
                .append_pair("offset", &offset.to_string());
            let (batch, rejected) = parse_more(&ctx.get_text(&url).await?)?;
            for card in rejected {
                ctx.report_error(card);
            }
            if batch.is_empty() {
                break;
            }
            offset += batch.len();
            paths.extend(batch);
        }
        Ok(paths)
    }
}

#[async_trait]
impl Source for WilliamMorrisGallery {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listing_url = self.url(LISTING_PATH)?;
        let Listing {
            mut paths,
            grid_cards,
            page_id,
            rejected,
        } = parse_listing(&ctx.get_text(&listing_url).await?);
        for card in rejected {
            ctx.report_error(card);
        }
        if paths.is_empty() {
            return Err(SourceError::Parse(
                "no event cards found on the listing".into(),
            ));
        }
        if grid_cards >= GRID_PAGE {
            match page_id {
                Some(id) => {
                    for path in self.more_paths(ctx, &id).await? {
                        if !paths.contains(&path) {
                            paths.push(path);
                        }
                    }
                }
                None => ctx.report_error("full listing grid but no page id to load more with"),
            }
        }

        let mut raws = Vec::new();
        for path in paths.iter().take(MAX_DETAIL_PAGES) {
            let url = self.url(path)?;
            let html = match ctx.get_text(&url).await {
                Ok(html) => html,
                Err(e) => {
                    ctx.report_error(format!("{path}: {e}"));
                    continue;
                }
            };
            match parse_detail(&html) {
                Some(detail) => raws.push(detail_event(path, &url, &detail)),
                None => ctx.report_error(format!("{path}: detail page without a title")),
            }
        }
        Ok(raws)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(dates: &str, sessions: &[&str], categories: &[&str]) -> Value {
        json!({
            "title": "Mini Morris",
            "dates_text": dates,
            "sessions": sessions,
            "categories": categories,
        })
    }

    fn normalise(payload: &Value) -> Option<NewEvent> {
        normalise_payload(payload).unwrap()
    }

    #[test]
    fn dates_and_sessions_parse() {
        assert_eq!(
            parse_dates("Saturday 3 October 2026 - Sunday 28 March 2027"),
            Some((
                NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
                NaiveDate::from_ymd_opt(2027, 3, 28).unwrap()
            ))
        );
        let day = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        assert_eq!(parse_dates("Thursday 15 October 2026"), Some((day, day)));
        assert_eq!(parse_dates("Autumn 2026"), None);
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert_eq!(
            parse_session("1:00pm to 4:00pm."),
            Some((t(13, 0), Some(t(16, 0))))
        );
        assert_eq!(
            parse_session("11:30am to 12:30pm."),
            Some((t(11, 30), Some(t(12, 30))))
        );
        assert_eq!(parse_session("7pm"), Some((t(19, 0), None)));
        assert_eq!(parse_session("all afternoon"), None);
    }

    #[test]
    fn sessions_are_london_wall_clock_and_the_first_is_the_time() {
        let bst = normalise(&event(
            "Thursday 15 October 2026",
            &["10:00am to 11:00am.", "11:30am to 12:30pm."],
            &["families", "workshops"],
        ))
        .unwrap();
        assert_eq!(bst.starts_at.to_rfc3339(), "2026-10-15T09:00:00+00:00");
        assert_eq!(
            bst.ends_at.unwrap().to_rfc3339(),
            "2026-10-15T10:00:00+00:00"
        );
        assert!(!bst.all_day);
        // Clocks go back on 25 October.
        let gmt = normalise(&event(
            "Thursday 19 November 2026",
            &["10:00am to 11:00am."],
            &["workshops"],
        ))
        .unwrap();
        assert_eq!(gmt.starts_at.to_rfc3339(), "2026-11-19T10:00:00+00:00");
    }

    #[test]
    fn untimed_days_and_exhibitions_are_all_day() {
        let day = normalise(&event("Saturday 10 October 2026", &[], &["workshops"])).unwrap();
        assert!(day.all_day);
        assert_eq!(day.starts_at.to_rfc3339(), "2026-10-09T23:00:00+00:00");
        assert_eq!(day.ends_at, None);

        let show = normalise(&event(
            "Saturday 3 October 2026 - Sunday 28 March 2027",
            &["10:00am to 5:00pm."],
            &["all-ages", "exhibition-type-2"],
        ))
        .unwrap();
        assert_eq!(show.category, Category::Exhibition);
        assert!(show.all_day);
        assert_eq!(show.starts_at.to_rfc3339(), "2026-10-02T23:00:00+00:00");
        assert_eq!(
            show.ends_at.unwrap().to_rfc3339(),
            "2027-03-28T00:00:00+00:00"
        );
    }

    #[test]
    fn runs_of_sessions_undated_and_uncategorised_events_are_skipped() {
        let run = event(
            "Sunday 27 September 2026 - Sunday 14 March 2027",
            &[],
            &["wellbeing", "workshops"],
        );
        assert!(normalise(&run).is_none());
        let mut undated = event("", &[], &["workshops"]);
        undated["dates_text"] = Value::Null;
        assert!(normalise(&undated).is_none());
        assert!(normalise(&event("Saturday 10 October 2026", &[], &["audience"])).is_none());
    }

    #[test]
    fn bad_dates_or_sessions_are_errors() {
        for payload in [
            event("Autumn 2026", &[], &["workshops"]),
            event("Saturday 10 October 2026", &["teatime"], &["workshops"]),
            json!({"dates_text": "Saturday 10 October 2026"}),
        ] {
            assert!(normalise_payload(&payload).is_err(), "{payload}");
        }
    }

    #[test]
    fn subtitles_join_the_title_unless_they_say_multiple_dates() {
        let mut payload = event("Thursday 15 October 2026", &[], &["workshops"]);
        payload["subtitle"] = json!(" Feathered Friends");
        assert_eq!(
            normalise(&payload).unwrap().title,
            "Mini Morris: Feathered Friends"
        );
        payload["subtitle"] = json!("Multiple dates");
        assert_eq!(normalise(&payload).unwrap().title, "Mini Morris");
    }

    #[test]
    fn category_rules() {
        let c = |classes: &[&str]| category(classes);
        assert_eq!(
            c(&["all-ages", "exhibition-type-2", "upcoming-exhibitions"]),
            Some(Category::Exhibition)
        );
        assert_eq!(c(&["online", "talks-and-discussions"]), None);
        assert_eq!(c(&["off-site", "workshops"]), None);
        assert_eq!(
            c(&["adults", "talks-and-discussions"]),
            Some(Category::Talk)
        );
        assert_eq!(
            c(&["families", "free-events", "workshops"]),
            Some(Category::Workshop)
        );
        assert_eq!(c(&["families", "tours"]), None);
        assert_eq!(c(&["film", "late-event"]), None);
        assert_eq!(c(&["adults", "late-event"]), Some(Category::Community));
        assert_eq!(c(&["families"]), Some(Category::Community));
        assert_eq!(c(&["adults", "wellbeing"]), None);
    }
}
