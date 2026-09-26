//! Whitechapel Gallery — hand-written scraper over the exhibition pages' HTML.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *` /
//!   `Disallow:` (empty), so everything is allowed.
//! * The only JSON-LD is a Yoast `WebPage`/`WebSite` graph with no `Event`, so
//!   CSS selectors are used. `/exhibitions/` is a single page; its upcoming
//!   cards are `.mediaBlock` blocks linking to `/exhibitions/<slug>/`. Past
//!   exhibitions are `.packeryItem` cards (not `.mediaBlock`) and so are not
//!   scraped. The section listing current exhibitions could not be observed
//!   (none were running on 2026-09-26); the other card templates on the site
//!   (year archive, homepage) also use `.mediaBlock`, so the selector does
//!   not depend on the section wrapper.
//! * Every detail page is fetched for its price line and description. Dates
//!   come from the "visit info" calendar line (`07 Oct - 14 Feb 2027`,
//!   `Tue 17 Nov - Sun 13 Dec 2026`): date-only ranges with abbreviated
//!   months and optional weekdays. A start without a year takes the end's
//!   year (the year before if that would put it after the end). Both ends are
//!   stored as London midnight of their day, as for Somerset House.
//! * Pages without a calendar line (past exhibitions) and open-ended text
//!   ("Until …", "From …", "Ongoing") are skipped (`Ok(None)`); any other
//!   date text is a parse error.
//! * Price comes only from the `.visitStatus` paragraph ("Free entry").
//!   Ticketed shows only have a "Book Now" button whose prices are loaded
//!   from Spektrix by JavaScript, so their price is unknown. The page body is
//!   not used: it mentions unrelated amounts (a "£10,000 prize").
//! * Every item is placed at the gallery; the room ("Gallery 4") is kept in
//!   the payload only. An off-site item would get the wrong venue.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, parse_price};

pub const KEY: &str = "whitechapel-gallery";
/// Upper bound on detail pages fetched per run (≈ 40 s at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 20;
const LISTING_PATH: &str = "/exhibitions/";
const SITE: &str = "https://www.whitechapelgallery.org";
const VENUE_NAME: &str = "Whitechapel Gallery";
const VENUE_ADDRESS: &str = "77-82 Whitechapel High Street, London E1 7QX";
/// Approximate location of the building.
const VENUE_LAT: f64 = 51.5160;
const VENUE_LNG: f64 = -0.0700;
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

pub struct WhitechapelGallery {
    base_url: Url,
}

impl WhitechapelGallery {
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

/// Extract exhibition paths (`/exhibitions/<slug>/`) from the listing page's
/// cards, in document order, de-duplicated.
pub fn parse_listing(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let base = Url::parse(SITE).expect("valid url");
    let mut out: Vec<String> = Vec::new();
    for a in doc.select(&selector(".mediaBlock a[href]")) {
        let Some(Ok(u)) = a.value().attr("href").map(|h| base.join(h)) else {
            continue;
        };
        if u.host_str() != Some("www.whitechapelgallery.org") || u.query().is_some() {
            continue;
        }
        let Some(slug) = u
            .path()
            .strip_prefix("/exhibitions/")
            .and_then(|s| s.strip_suffix('/'))
        else {
            continue;
        };
        let valid = !slug.is_empty()
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        let path = format!("/exhibitions/{slug}/");
        if valid && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// The leading paragraphs of the page body: an optional `h3` kicker is
/// skipped, and the first other non-`p` element (bio accordions, "About the
/// artists", the sponsor list) ends the description.
fn description_html(doc: &Html) -> Option<String> {
    let body = doc.select(&selector("div.oneHalf_pos2")).next()?;
    let mut children = body.children().filter_map(ElementRef::wrap).peekable();
    children.next_if(|e| e.value().name() == "h3");
    let paragraphs: Vec<String> = children
        .take_while(|e| e.value().name() == "p")
        .map(|e| e.inner_html())
        .collect();
    (!paragraphs.is_empty()).then(|| paragraphs.join("\n"))
}

/// Parse one detail page into a [`RawEvent`] (None if it has no title).
/// `path` is the page path; its slug is the stable source id.
pub fn parse_detail(html: &str, path: &str) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let title = doc
        .select(&selector(".exhibition_single h1"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty())?;
    let first_text = |s: &str| {
        doc.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let date_text = first_text(".visitInfo p.icon-calendar + p.indent");
    let room = first_text(".visitInfo p.icon-pin + p.indent");
    let price_text = first_text(".visitStatus p");
    let image_url = doc
        .select(&selector(r#"meta[property="og:image"]"#))
        .next()
        .and_then(|e| e.value().attr("content"))
        .map(str::to_string);
    let slug = path.trim_matches('/').rsplit('/').next().unwrap_or(path);
    let url = format!("{SITE}{path}");
    Some(RawEvent {
        source_event_id: slug.to_string(),
        source_url: Some(url.clone()),
        payload: json!({
            "url": url,
            "title": title,
            "date_text": date_text,
            "room": room,
            "price_text": price_text,
            "description": description_html(&doc),
            "image_url": image_url,
        }),
    })
}

/// Parse a visit-info date range into its first and last day. `Ok(None)` for
/// open-ended text ("Until …", "From …", "Ongoing").
pub fn parse_date_range(text: &str) -> Result<Option<(NaiveDate, NaiveDate)>, SourceError> {
    let lower = text.to_lowercase();
    if ["until", "from", "ongoing", "open"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return Ok(None);
    }
    let err = || SourceError::Parse(format!("unrecognised date range {text:?}"));
    let (start, end) = [" - ", " – ", " — "]
        .iter()
        .find_map(|sep| lower.split_once(sep))
        .ok_or_else(err)?;
    let (end_day, end_month, Some(end_year)) = parse_day(end).ok_or_else(err)? else {
        return Err(err());
    };
    let end = NaiveDate::from_ymd_opt(end_year, end_month, end_day).ok_or_else(err)?;
    let (day, month, year) = parse_day(start).ok_or_else(err)?;
    let start = match year {
        Some(y) => NaiveDate::from_ymd_opt(y, month, day).ok_or_else(err)?,
        None => {
            let d = NaiveDate::from_ymd_opt(end.year(), month, day).ok_or_else(err)?;
            if d > end {
                NaiveDate::from_ymd_opt(end.year() - 1, month, day).ok_or_else(err)?
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

/// "[Tue] 07 oct [2026]" (lower-cased) → (day, month, year). Months match on
/// their first three letters, so "sept" and "october" are accepted.
fn parse_day(s: &str) -> Option<(u32, u32, Option<i32>)> {
    let mut tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens
        .first()
        .is_some_and(|t| t.chars().all(|c| c.is_ascii_alphabetic()))
    {
        tokens.remove(0);
    }
    let (day, month, year) = match tokens.as_slice() {
        [d, m] => (d, m, None),
        [d, m, y] => (d, m, Some(y.parse().ok()?)),
        _ => return None,
    };
    let month = MONTHS.iter().position(|p| month.get(..3) == Some(*p))? as u32 + 1;
    Some((day.parse().ok()?, month, year))
}

/// Normalise a Whitechapel Gallery [`RawEvent`] payload.
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
    let price = payload
        .get("price_text")
        .and_then(Value::as_str)
        .map(parse_price)
        .unwrap_or_default();

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(payload.get("description").and_then(Value::as_str)),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at: Some(ends_at).filter(|e| *e > starts_at),
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
        tags: vec!["art".to_string()],
    }))
}

#[async_trait]
impl Source for WhitechapelGallery {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let paths = parse_listing(&ctx.get_text(&url).await?);
        if paths.is_empty() {
            return Err(SourceError::Parse(
                "no exhibition links found on the listing page".into(),
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
    fn parses_visit_info_date_ranges() {
        assert_eq!(
            range("07 Oct - 14 Feb 2027"),
            pair("2026-10-07", "2027-02-14")
        );
        assert_eq!(
            range("7 Oct 2026 - 14 Feb 2027"),
            pair("2026-10-07", "2027-02-14")
        );
        assert_eq!(
            range("7 Oct - 8 Nov 2026"),
            pair("2026-10-07", "2026-11-08")
        );
        assert_eq!(
            range("Tue 17 Nov - Sun 13 Dec 2026"),
            pair("2026-11-17", "2026-12-13")
        );
        assert_eq!(
            range("15 Jul - 6 Sep 2026"),
            pair("2026-07-15", "2026-09-06")
        );
        assert_eq!(
            range("20 Sept - 3 October 2026"),
            pair("2026-09-20", "2026-10-03")
        );
    }

    #[test]
    fn open_ended_ranges_are_skips() {
        for text in ["Until 14 Feb 2027", "Ongoing", "From 7 Oct 2026"] {
            assert_eq!(range(text), None, "{text}");
        }
    }

    #[test]
    fn unrecognised_ranges_are_errors() {
        for text in [
            "Autumn 2026",
            "14 Feb 2027 - 7 Oct 2026",
            "7 Oct 2026",
            "7 Oct - 8 Nov",
        ] {
            assert!(parse_date_range(text).is_err(), "{text}");
        }
    }
}
