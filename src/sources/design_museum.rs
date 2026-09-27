//! Design Museum (Kensington High Street) — hand-written scraper over the
//! exhibition pages' HTML.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *` /
//!   `Allow: /`, so `/exhibitions` is allowed.
//! * There is no schema.org JSON-LD on the listing or detail pages, so CSS
//!   selectors are used. The current programme is on `/exhibitions` and the
//!   upcoming one on `/exhibitions/future-exhibitions-and-displays`; both
//!   link to detail pages (`/exhibitions/<slug>`) via `.page-item` cards.
//!   Listing cards only say "Until 4 October 2026", so every detail page is
//!   fetched for its header date range (`section.page-description time.date`).
//!   Other `<time>` elements on detail pages belong to related-event teasers
//!   and are ignored.
//! * Exhibitions are date-only ranges ("1 May – 4 October 2026", "13 February
//!   2026 – January 2027"). A start without a year takes the end's year (the
//!   year before if that would put it after the end); a month-only end is the
//!   last day of that month. Both ends are stored as London midnight of their
//!   day, as for Somerset House.
//! * Open-ended items (the permanent collection "Open daily", "Until June
//!   2027" with no start, "From December 2025 - …") and pages without a header
//!   date are skipped (`Ok(None)`); any other date text is a parse error.
//! * Price: "Free display" pages are free; otherwise the first paragraph of the
//!   first `.buyticket-component` block, which on ticketed exhibitions is
//!   "Booking information" ("Adult: From £17.09 … Child: From £8.55").
//! * Every item is placed at the museum. The only off-site item seen (an
//!   Earl's Court installation) is open-ended and therefore skipped; a future
//!   off-site item with a proper date range would get the wrong venue.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, parse_price};

pub const KEY: &str = "design-museum";
/// Upper bound on detail pages fetched per run (≈ 40 s at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 20;
const LISTING_PATHS: &[&str] = &[
    "/exhibitions",
    "/exhibitions/future-exhibitions-and-displays",
];
/// `/exhibitions/<slug>` pages that are hubs, not exhibitions.
const HUB_SLUGS: &[&str] = &[
    "future-exhibitions-and-displays",
    "touring-exhibitions",
    "past-exhibitions",
];
const SITE: &str = "https://designmuseum.org";
const VENUE_NAME: &str = "Design Museum";
const VENUE_ADDRESS: &str = "224-238 Kensington High Street, London W8 6AG";
/// Approximate location of the building.
const VENUE_LAT: f64 = 51.4994;
const VENUE_LNG: f64 = -0.2006;

pub struct DesignMuseum {
    base_url: Url,
}

impl DesignMuseum {
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

/// Extract exhibition paths (`/exhibitions/<slug>`) from the cards of a
/// listing page, in document order, de-duplicated.
pub fn parse_listing(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let base = Url::parse(SITE).expect("valid url");
    let mut out: Vec<String> = Vec::new();
    for a in doc.select(&selector(".page-item a[href]")) {
        let Some(Ok(u)) = a.value().attr("href").map(|h| base.join(h)) else {
            continue;
        };
        if u.host_str() != Some("designmuseum.org") || u.query().is_some() {
            continue;
        }
        let Some(slug) = u.path().strip_prefix("/exhibitions/") else {
            continue;
        };
        let valid = !slug.is_empty()
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !HUB_SLUGS.contains(&slug);
        let path = format!("/exhibitions/{slug}");
        if valid && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// Parse one detail page into a [`RawEvent`] (None if it has no title).
/// `path` is the page path; its slug is the stable source id.
pub fn parse_detail(html: &str, path: &str) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let header = doc.select(&selector("section.page-description")).next()?;
    let title = header
        .select(&selector("h1"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty())?;
    let labels: Vec<String> = header
        .select(&selector(".labels .highlight"))
        .map(element_text)
        .collect();
    let date_text = header
        .select(&selector("time.date"))
        .next()
        .map(element_text);
    let description = header
        .select(&selector(".intro .rich-text"))
        .next()
        .map(|e| e.inner_html());
    let price_text = doc
        .select(&selector(".buyticket-component .rich-text p"))
        .next()
        .map(element_text);
    let image_url = doc
        .select(&selector(".modern-backdrop"))
        .next()
        .and_then(|e| e.value().attr("style"))
        .and_then(|s| s.split_once("url(")?.1.split_once(')'))
        .and_then(|(u, _)| Url::parse(SITE).ok()?.join(u).ok())
        .map(String::from);
    let slug = path.trim_matches('/').rsplit('/').next().unwrap_or(path);
    let url = format!("{SITE}{path}");
    Some(RawEvent {
        source_event_id: slug.to_string(),
        source_url: Some(url.clone()),
        payload: json!({
            "url": url,
            "title": title,
            "labels": labels,
            "date_text": date_text,
            "description": description,
            "price_text": price_text,
            "image_url": image_url,
        }),
    })
}

/// Parse a header date range into its first and last day. `Ok(None)` for
/// open-ended text ("Until …", "From …", "Open daily").
pub fn parse_date_range(text: &str) -> Result<Option<(NaiveDate, NaiveDate)>, SourceError> {
    let lower = text.to_lowercase();
    if ["until", "from", "open"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return Ok(None);
    }
    let err = || SourceError::Parse(format!("unrecognised date range {text:?}"));
    let (start, end) = [" – ", " — ", " - "]
        .iter()
        .find_map(|sep| text.split_once(sep))
        .ok_or_else(err)?;
    let end = parse_day(end.trim())
        .or_else(|| last_day_of_month(end.trim()))
        .ok_or_else(err)?;
    let start = match parse_day(start.trim()) {
        Some(d) => d,
        None => {
            let d = parse_day(&format!("{} {}", start.trim(), end.year())).ok_or_else(err)?;
            if d > end {
                d.with_year(end.year() - 1).ok_or_else(err)?
            } else {
                d
            }
        }
    };
    if start > end {
        return Err(err());
    }
    Ok(Some((start, end)))
}

/// "4 October 2026".
fn parse_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%d %B %Y").ok()
}

/// "January 2027" → 31 January 2027.
fn last_day_of_month(s: &str) -> Option<NaiveDate> {
    let first = NaiveDate::parse_from_str(&format!("1 {s}"), "%d %B %Y").ok()?;
    first.checked_add_months(chrono::Months::new(1))?.pred_opt()
}

/// Normalise a Design Museum [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("detail page without title".into()))?;
    let Some(date_text) = payload.get("date_text").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some((first_day, last_day)) = parse_date_range(date_text)? else {
        return Ok(None);
    };
    let starts_at = london_to_utc(first_day.and_time(NaiveTime::MIN));
    let ends_at = london_to_utc(last_day.and_time(NaiveTime::MIN));

    let labels: Vec<&str> = payload
        .get("labels")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let free_display = labels
        .iter()
        .any(|l| l.to_lowercase().contains("free display"));
    let price = if free_display {
        parse_price("Free")
    } else {
        payload
            .get("price_text")
            .and_then(Value::as_str)
            .map(parse_price)
            .unwrap_or_default()
    };

    let mut tags = vec!["design".to_string()];
    if free_display {
        tags.push("free-display".into());
    }

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(payload.get("description").and_then(Value::as_str)),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at: Some(ends_at).filter(|e| *e > starts_at),
        all_day: true,
        price,
        url: payload
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        image_url: payload
            .get("image_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        category: Category::Exhibition,
        tags,
    }))
}

#[async_trait]
impl Source for DesignMuseum {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let mut paths: Vec<String> = Vec::new();
        for listing_path in LISTING_PATHS {
            let url = self
                .base_url
                .join(listing_path)
                .map_err(|e| SourceError::Config(e.to_string()))?;
            for p in parse_listing(&ctx.get_text(&url).await?) {
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
        if paths.is_empty() {
            return Err(SourceError::Parse(
                "no exhibition links found on the listing pages".into(),
            ));
        }
        let mut out = Vec::new();
        for path in paths.iter().take(MAX_DETAIL_PAGES) {
            let url = match self.base_url.join(path) {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad detail path {path}: {e}"));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html, path) {
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

    fn range(text: &str) -> Option<(String, String)> {
        parse_date_range(text)
            .unwrap()
            .map(|(a, b)| (a.to_string(), b.to_string()))
    }

    fn pair(a: &str, b: &str) -> Option<(String, String)> {
        Some((a.into(), b.into()))
    }

    #[test]
    fn parses_header_date_ranges() {
        assert_eq!(
            range("1 May – 4 October 2026"),
            pair("2026-05-01", "2026-10-04")
        );
        assert_eq!(
            range("13 February 2026 – January 2027"),
            pair("2026-02-13", "2027-01-31")
        );
        assert_eq!(
            range("2 October 2026 – 4 April 2027"),
            pair("2026-10-02", "2027-04-04")
        );
        assert_eq!(
            range("1 November – 4 February 2027"),
            pair("2026-11-01", "2027-02-04")
        );
        assert_eq!(
            range("6 November 2026 - 8 August 2027"),
            pair("2026-11-06", "2027-08-08")
        );
    }

    #[test]
    fn open_ended_ranges_are_skips() {
        for text in [
            "Until June 2027",
            "Open daily",
            "From December 2025 - Open daily from 9:00 – 17:00",
        ] {
            assert_eq!(range(text), None, "{text}");
        }
    }

    #[test]
    fn unrecognised_ranges_are_errors() {
        for text in [
            "Summer 2027",
            "4 October 2026 – 1 May 2026",
            "Every Tuesday",
        ] {
            assert!(parse_date_range(text).is_err(), "{text}");
        }
    }
}
