//! Mall Galleries — hand-written scraper for the art-society annual
//! exhibitions at the Mall Galleries (Federation of British Artists).
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): Drupal's default
//!   file; only `/core/`, `/profiles/`, `/admin/`, `/search/`, `/user/…`
//!   and similar are disallowed, so the homepage and
//!   `/exhibitions-events/<slug>` pages are allowed.
//! * `/exhibitions-events` (where `/whats-on` meta-refreshes) server-renders
//!   only the current exhibition; its upcoming list is a React app fed by
//!   CSRF-token POST endpoints, which we don't use. The homepage's
//!   "Exhibitions & Events" block server-renders the current exhibition plus
//!   the upcoming ones (`article.o-event-exhibition-page` teasers), so it is
//!   the listing. Selectors are scoped to that block: the news slider below
//!   uses the same teaser markup.
//! * No JSON-LD at all, so every detail page is read with CSS selectors for
//!   the title, the dates, the admission line and the description (the
//!   intro text block if there is one, else all text blocks: see
//!   `description_html`).
//! * Dates: the header's `<time datetime="2026-10-01T12:00:00Z">` elements.
//!   The time of day is a placeholder (always 12:00Z), so only the date part
//!   is used; both ends are stored as London midnight of their day, as for
//!   the other galleries. The opening hours stay in the payload only.
//! * Price comes from the header's admission paragraph ("Admission £7. Free
//!   for Friends of Mall Galleries and under 25s…" → £7; "Free Admission" →
//!   free).
//! * Every item is placed at the Mall Galleries; the rooms ("North, East &
//!   West Galleries") are kept in the payload only.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_to_utc, map_category, parse_price,
};

pub const KEY: &str = "mall-galleries";
/// Upper bound on detail pages fetched per run (≈ 30 s at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 15;
const LISTING_PATH: &str = "/";
const SITE: &str = "https://www.mallgalleries.org.uk";
const VENUE_NAME: &str = "Mall Galleries";
const VENUE_ADDRESS: &str = "The Mall, London SW1Y 5BD";
/// Approximate location of the building.
const VENUE_LAT: f64 = 51.5063;
const VENUE_LNG: f64 = -0.1310;

/// One teaser of the homepage's "Exhibitions & Events" block.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ListingItem {
    /// `/exhibitions-events/<slug>`
    pub path: String,
    /// The teaser's label ("Exhibition"), if any.
    pub label: Option<String>,
}

pub struct MallGalleries {
    base_url: Url,
}

impl MallGalleries {
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

/// Extract the exhibition pages linked from the homepage's "Exhibitions &
/// Events" block, in document order, de-duplicated.
pub fn parse_listing(html: &str) -> Vec<ListingItem> {
    let doc = Html::parse_document(html);
    let base = Url::parse(SITE).expect("valid url");
    let mut out: Vec<ListingItem> = Vec::new();
    for article in doc.select(&selector(
        ".m-entity__upcoming-exhibitions article.o-event-exhibition-page",
    )) {
        let label = article
            .select(&selector(".o-teaser__thumb--label"))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty());
        for a in article.select(&selector("a[href]")) {
            let Some(Ok(u)) = a.value().attr("href").map(|h| base.join(h)) else {
                continue;
            };
            if u.host_str() != Some("www.mallgalleries.org.uk") || u.query().is_some() {
                continue;
            }
            let Some(slug) = u.path().strip_prefix("/exhibitions-events/") else {
                continue;
            };
            let valid = !slug.is_empty()
                && slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            let path = format!("/exhibitions-events/{slug}");
            if valid && !out.iter().any(|i| i.path == path) {
                out.push(ListingItem {
                    path,
                    label: label.clone(),
                });
            }
        }
    }
    out
}

/// The page's description. Pages with an intro text block (a plain
/// `section.m-entity` text block) use only that: their other text blocks
/// are artwork captions ("Top row, Left to Right: …"). Pages without one
/// use all their text blocks (heading + body), in order.
fn description_html(doc: &Html) -> Option<String> {
    let (intro, blocks): (Vec<ElementRef<'_>>, Vec<ElementRef<'_>>) = doc
        .select(&selector(r#"section[aria-label="text content block"]"#))
        .partition(|s| !s.value().classes().any(|c| c == "m-entity__text__wrapper"));
    let chosen = if intro.is_empty() { blocks } else { intro };
    let parts: Vec<String> = chosen
        .into_iter()
        .filter_map(|s| s.select(&selector(".m-entity__text")).next())
        .map(|e| e.inner_html())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Parse one detail page into a [`RawEvent`] (None if it has no title).
/// `url` is the address the page was fetched from; its last path segment is
/// the stable source id.
pub fn parse_detail(html: &str, url: &Url, label: Option<&str>) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let header = doc
        .select(&selector(".o-event-exhibition__header--details"))
        .next()?;
    let title = header
        .select(&selector("h1"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty())?;
    let dates: Vec<String> = header
        .select(&selector("time[datetime]"))
        .filter_map(|t| t.value().attr("datetime"))
        .map(str::to_string)
        .collect();
    let when = header
        .select(&selector("h3"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty());
    let rooms = header
        .select(&selector("h3 span"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty());
    let info = doc
        .select(&selector(".o-event-exhibition__header--info"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty());
    let image_url = doc
        .select(&selector(r#"meta[property="og:image"]"#))
        .next()
        .and_then(|e| e.value().attr("content"))
        .map(str::to_string);
    let path = url.path();
    let slug = path.trim_matches('/').rsplit('/').next().unwrap_or(path);
    Some(RawEvent {
        source_event_id: slug.to_string(),
        source_url: Some(url.to_string()),
        payload: json!({
            "url": url.as_str(),
            "title": title,
            "label": label,
            "dates": dates,
            "when_text": when,
            "rooms": rooms,
            "info_text": info,
            "description": description_html(&doc),
            "image_url": image_url,
        }),
    })
}

/// The date part of a `<time datetime>` value (`2026-10-01T12:00:00Z`).
pub fn parse_time_date(value: &str) -> Result<NaiveDate, SourceError> {
    value
        .get(..10)
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .ok_or_else(|| SourceError::Parse(format!("bad datetime {value:?}")))
}

/// Normalise a Mall Galleries [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let s = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = s("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("detail page without title".into()))?;
    let dates: Vec<&str> = payload
        .get("dates")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let (first_day, last_day) = match dates.as_slice() {
        [] => return Ok(None),
        [d] => (parse_time_date(d)?, None),
        [a, b, ..] => (parse_time_date(a)?, Some(parse_time_date(b)?)),
    };
    if last_day.is_some_and(|l| l < first_day) {
        return Err(SourceError::Parse(format!("dates out of order: {dates:?}")));
    }
    let starts_at = london_to_utc(first_day.and_time(NaiveTime::MIN));
    let ends_at = last_day.map(|d| london_to_utc(d.and_time(NaiveTime::MIN)));
    let category = match s("label") {
        Some(l) => map_category(&[l]).unwrap_or(Category::Exhibition),
        None => Category::Exhibition,
    };

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(s("description")),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at: ends_at.filter(|e| *e > starts_at),
        all_day: true,
        price: s("info_text").map(parse_price).unwrap_or_default(),
        url: s("url").map(str::to_string),
        image_url: s("image_url").map(str::to_string),
        category,
        tags: vec!["art".to_string()],
    }))
}

#[async_trait]
impl Source for MallGalleries {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let items = parse_listing(&ctx.get_text(&url).await?);
        if items.is_empty() {
            return Err(SourceError::Parse(
                "no exhibition links found on the homepage".into(),
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
                Ok(html) => match parse_detail(&html, &url, item.label.as_deref()) {
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

    fn times(dates: &[&str]) -> (String, Option<String>) {
        let e = normalise_payload(&json!({"title": "Show", "dates": dates}))
            .unwrap()
            .unwrap();
        (e.starts_at.to_rfc3339(), e.ends_at.map(|t| t.to_rfc3339()))
    }

    #[test]
    fn dates_are_london_midnights() {
        // BST start, GMT end.
        assert_eq!(
            times(&["2026-10-20T12:00:00Z", "2026-11-06T12:00:00Z"]),
            (
                "2026-10-19T23:00:00+00:00".into(),
                Some("2026-11-06T00:00:00+00:00".into())
            )
        );
        assert_eq!(
            times(&["2027-01-10T12:00:00Z"]),
            ("2027-01-10T00:00:00+00:00".into(), None)
        );
        // A one-day show has no end.
        assert_eq!(
            times(&["2026-10-20T12:00:00Z", "2026-10-20T12:00:00Z"]),
            ("2026-10-19T23:00:00+00:00".into(), None)
        );
    }

    #[test]
    fn missing_dates_are_skips_and_bad_ones_errors() {
        assert!(
            normalise_payload(&json!({"title": "Show", "dates": []}))
                .unwrap()
                .is_none()
        );
        for dates in [
            json!(["soon"]),
            json!(["2026-11-06T12:00:00Z", "2026-10-20T12:00:00Z"]),
        ] {
            assert!(
                normalise_payload(&json!({"title": "Show", "dates": dates})).is_err(),
                "{dates}"
            );
        }
    }

    #[test]
    fn label_sets_the_category() {
        let cat = |label: Value| {
            normalise_payload(&json!({"title": "X", "label": label, "dates": ["2026-10-20"]}))
                .unwrap()
                .unwrap()
                .category
        };
        assert_eq!(cat(json!("Exhibition")), Category::Exhibition);
        assert_eq!(cat(Value::Null), Category::Exhibition);
        assert_eq!(cat(json!("Talk")), Category::Talk);
    }
}
