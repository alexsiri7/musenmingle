//! Goldsmiths CCA (New Cross) — hand-written scraper over the homepage's
//! exhibition list and the exhibition pages' HTML.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   disallows only `/wp-admin/`.
//! * There is no JSON-LD on the site, so CSS selectors are used. `/whats-on/`
//!   redirects to the homepage, which lists the current and upcoming
//!   exhibitions as `section.event-item` blocks: title link
//!   (`h2.fixed-title a`, to `/exhibition/<slug>/`), image and a date line
//!   (`25 September–13 December 2026`). Events (talks, tours) live elsewhere
//!   and are not scraped.
//! * Every exhibition page is fetched (at most [`MAX_DETAIL_PAGES`] per run)
//!   for its subtitle (`header.post-info h2`, the show's name when the
//!   homepage title is the artist's) and description: the `.enter`
//!   paragraphs before the "Read more…" toggle, whose hidden content (the
//!   biography) is left out. The `h1` is upper-cased, so the homepage's
//!   title is used.
//! * Dates are date-only ranges with full or abbreviated months and no
//!   spaces around the dash (`25 September–13 December 2026`,
//!   `6–20 November 2026`). A start without a year takes the end's year (the
//!   year before if that would put it after the end); both ends are stored
//!   as London midnight of their day. Open-ended text ("Ongoing", "Until …")
//!   is skipped (`Ok(None)`).
//! * The site states no prices on exhibition pages, so price is unknown.
//! * Every item is placed at the gallery.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc};

pub const KEY: &str = "goldsmiths-cca";
/// Upper bound on exhibition pages fetched per run (the homepage shows 2–3).
pub const MAX_DETAIL_PAGES: usize = 10;
const LISTING_PATH: &str = "/";
const SITE_HOST: &str = "goldsmithscca.art";
const VENUE_NAME: &str = "Goldsmiths CCA";
const VENUE_ADDRESS: &str = "St James', New Cross, London SE14 6AD";
/// Approximate location of the building.
const VENUE_LAT: f64 = 51.4758;
const VENUE_LNG: f64 = -0.0361;
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

pub struct GoldsmithsCca {
    base_url: Url,
}

impl GoldsmithsCca {
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

/// One exhibition on the homepage.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ListingItem {
    /// `/exhibition/<slug>/`.
    pub path: String,
    pub title: String,
    pub date_text: Option<String>,
    pub summary: Option<String>,
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

/// Extract the homepage's exhibition blocks, in document order,
/// de-duplicated by path.
pub fn parse_listing(html: &str) -> Vec<ListingItem> {
    let doc = Html::parse_document(html);
    let base = Url::parse(&format!("https://{SITE_HOST}/")).expect("valid url");
    let first_text = |section: ElementRef<'_>, s: &str| {
        section
            .select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let mut out: Vec<ListingItem> = Vec::new();
    for section in doc.select(&selector("section.event-item")) {
        let Some(a) = section.select(&selector("h2.fixed-title a[href]")).next() else {
            continue;
        };
        let Some(Ok(u)) = a.value().attr("href").map(|h| base.join(h)) else {
            continue;
        };
        if u.host_str() != Some(SITE_HOST) || u.query().is_some() {
            continue;
        }
        let Some(slug) = u
            .path()
            .strip_prefix("/exhibition/")
            .and_then(|s| s.strip_suffix('/'))
        else {
            continue;
        };
        let valid = !slug.is_empty()
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        let path = format!("/exhibition/{slug}/");
        let title = element_text(a);
        if !valid || title.is_empty() || out.iter().any(|i| i.path == path) {
            continue;
        }
        out.push(ListingItem {
            path,
            title,
            date_text: first_text(section, ".col .date"),
            summary: first_text(section, ".col .text"),
            image_url: section
                .select(&selector(".image img[src]"))
                .next()
                .and_then(|i| i.value().attr("src"))
                .map(str::to_string),
        });
    }
    out
}

/// The `.enter` paragraphs before the "Read more…" toggle, minus
/// image-only paragraphs (sponsor logos).
fn description_html(doc: &Html) -> Option<String> {
    let body = doc.select(&selector("article .enter")).next()?;
    let more = selector(".more-link");
    let paragraphs: Vec<String> = body
        .children()
        .filter_map(ElementRef::wrap)
        .take_while(|e| e.value().name() == "p" && e.select(&more).next().is_none())
        .filter(|e| !element_text(*e).is_empty())
        .map(|e| e.inner_html())
        .collect();
    (!paragraphs.is_empty()).then(|| paragraphs.join("\n"))
}

/// Parse one exhibition page into a [`RawEvent`] (None if it is not an
/// exhibition page). `url` is the address the page was fetched from; `item`
/// is its homepage entry.
pub fn parse_detail(html: &str, url: &Url, item: &ListingItem) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let first_text = |s: &str| {
        doc.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    // The page heading proves it is an exhibition page.
    first_text("header.post-info h1")?;
    let subtitle = first_text("header.post-info h2");
    let title = match &subtitle {
        Some(sub) if item.title.contains(':') => format!("{} – {sub}", item.title),
        Some(sub) => format!("{}: {sub}", item.title),
        None => item.title.clone(),
    };
    let date_text = item
        .date_text
        .clone()
        .or_else(|| first_text("header.post-info time"));
    Some(RawEvent {
        source_event_id: item.slug().to_string(),
        source_url: Some(url.to_string()),
        payload: json!({
            "url": url.as_str(),
            "title": title,
            "listing_title": item.title,
            "subtitle": subtitle,
            "date_text": date_text,
            "summary": item.summary,
            "description": description_html(&doc),
            "image_url": item.image_url,
        }),
    })
}

/// Parse a date range into its first and last day. `Ok(None)` for
/// open-ended text ("Until …", "From …", "Ongoing").
pub fn parse_date_range(text: &str) -> Result<Option<(NaiveDate, NaiveDate)>, SourceError> {
    let lower = clean_text(text).to_lowercase();
    if ["until", "from", "ongoing", "open"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return Ok(None);
    }
    let err = || SourceError::Parse(format!("unrecognised date range {text:?}"));
    let (start, end) = match ["–", "—", "-"].iter().find_map(|sep| lower.split_once(sep)) {
        Some((s, e)) => (Some(s.trim()), e.trim()),
        None => (None, lower.trim()),
    };
    let (end_day, Some(end_month), Some(end_year)) = parse_day(end).ok_or_else(err)? else {
        return Err(err());
    };
    let last = NaiveDate::from_ymd_opt(end_year, end_month, end_day).ok_or_else(err)?;
    let first = match start {
        None => last,
        Some(s) => {
            let (day, month, year) = parse_day(s).ok_or_else(err)?;
            let month = month.unwrap_or(end_month);
            match year {
                Some(y) => NaiveDate::from_ymd_opt(y, month, day).ok_or_else(err)?,
                None => {
                    let d = NaiveDate::from_ymd_opt(last.year(), month, day).ok_or_else(err)?;
                    if d > last {
                        NaiveDate::from_ymd_opt(last.year() - 1, month, day).ok_or_else(err)?
                    } else {
                        d
                    }
                }
            }
        }
    };
    if first > last {
        return Err(err());
    }
    Ok(Some((first, last)))
}

/// "[thu] 06 [november [2026]]" (lower-cased) → (day, month, year). Months
/// match on their first three letters.
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
    let month = match month {
        Some(m) => Some(MONTHS.iter().position(|p| m.get(..3) == Some(*p))? as u32 + 1),
        None => None,
    };
    Some((day.parse().ok()?, month, year))
}

/// Normalise a Goldsmiths CCA [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("exhibition without title".into()))?;
    let date_text = payload
        .get("date_text")
        .and_then(Value::as_str)
        .ok_or_else(|| SourceError::Parse("exhibition without a date line".into()))?;
    let Some((first_day, last_day)) = parse_date_range(date_text)? else {
        return Ok(None);
    };
    let starts_at = london_to_utc(first_day.and_time(NaiveTime::MIN));
    let ends_at = london_to_utc(last_day.and_time(NaiveTime::MIN));
    let description = clean_description(payload.get("description").and_then(Value::as_str))
        .or_else(|| clean_description(payload.get("summary").and_then(Value::as_str)));

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description,
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at: Some(ends_at).filter(|e| *e > starts_at),
        all_day: true,
        price: Default::default(),
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
impl Source for GoldsmithsCca {
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
                "no exhibitions found on the homepage".into(),
            ));
        }
        let mut out = Vec::new();
        for item in items.iter().take(MAX_DETAIL_PAGES) {
            let path = &item.path;
            let url = match self.base_url.join(path) {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad exhibition path {path}: {e}"));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html, &url, item) {
                    Some(raw) => out.push(raw),
                    None => ctx.report_error(format!("{path}: not an exhibition page")),
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
    fn parses_date_ranges() {
        assert_eq!(
            range("25 September–13 December 2026"),
            pair("2026-09-25", "2026-12-13")
        );
        assert_eq!(
            range("06 November–12 December 2026"),
            pair("2026-11-06", "2026-12-12")
        );
        assert_eq!(
            range("25 Sep-13 Dec 2026"),
            pair("2026-09-25", "2026-12-13")
        );
        assert_eq!(
            range("6–20 November 2026"),
            pair("2026-11-06", "2026-11-20")
        );
        assert_eq!(
            range("14 November–10 January 2027"),
            pair("2026-11-14", "2027-01-10")
        );
        assert_eq!(
            range("28 November 2026 – 7 February 2027"),
            pair("2026-11-28", "2027-02-07")
        );
        assert_eq!(range("12 December 2026"), pair("2026-12-12", "2026-12-12"));
    }

    #[test]
    fn open_ended_ranges_are_skips() {
        for text in ["Ongoing", "Until 13 December 2026", "From 6 November 2026"] {
            assert_eq!(range(text), None, "{text}");
        }
    }

    #[test]
    fn unrecognised_ranges_are_errors() {
        for text in [
            "Autumn 2026",
            "13 December 2026–25 September 2026",
            "25 September–13 December",
            "6 November",
            "",
        ] {
            assert!(parse_date_range(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn single_day_show_has_no_end() {
        let event = normalise_payload(&json!({
            "title": "One day only",
            "date_text": "7 October 2026",
        }))
        .unwrap()
        .unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-06T23:00:00+00:00");
        assert_eq!(event.ends_at, None);
    }

    #[test]
    fn missing_date_is_an_error() {
        assert!(normalise_payload(&json!({"title": "No date"})).is_err());
    }
}
