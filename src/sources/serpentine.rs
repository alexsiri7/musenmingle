//! Serpentine Galleries (Kensington Gardens) — example hand-written scraper.
//!
//! * robots.txt (checked 2026-09-25, saved as a fixture): only `/cms/wp-admin/`
//!   is disallowed; `/whats-on/` is allowed for all agents.
//! * The listing page `/whats-on/` has no Event markup, so we collect links to
//!   detail pages (`/whats-on/<slug>/`) and read each detail page's
//!   schema.org **JSON-LD `Event`** node. Pages without one (podcasts,
//!   research resources) are skipped silently.
//! * Quirk: the site emits London wall-clock times with a `+00:00` offset even
//!   during BST (e.g. Park Nights "8pm" appears as `T20:00:00+00:00`), so times
//!   are parsed with [`parse_london_wall_clock`], ignoring the offset. Dates
//!   without a time appear as `T00:00:00+00:00`, so a midnight start is
//!   `all_day` even when the end has a time (the Pavilion's last day is
//!   `T18:00`, its closing hour); the run then ends at its last day's midnight.
//! * Quirk: JSON-LD `offers.price` is unreliable (e.g. `"107."` for a £10/£7
//!   ticket). The human-readable "Price: ..." line in the page banner is
//!   preferred (CSS fallback), JSON-LD price only when it is absent.
//! * Not every "What's on" page is an event we want: online programmes
//!   (`OnlineEventAttendanceMode`) and open-ended projects/research strands
//!   (JSON-LD ranges longer than [`MAX_RANGE_DAYS`], e.g. one ending in 2100)
//!   are skipped (`Ok(None)`).
//! * Venues are the Serpentine's own buildings; addresses/coordinates come
//!   from the static table in [`venue_details`].

use async_trait::async_trait;
use chrono::NaiveTime;
use scraper::{Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::jsonld::{extract_events, first_offer, image_url, str_or_name};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, is_london_midnight, london_date, london_to_utc,
    map_category, parse_london_wall_clock, parse_price, price_from_amounts,
};

pub const KEY: &str = "serpentine-galleries";
/// Upper bound on detail pages fetched per run (≈ 1 min at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 30;
/// Ranges longer than this are ongoing projects, not exhibitions.
pub const MAX_RANGE_DAYS: i64 = 366;
const LISTING_PATH: &str = "/whats-on/";
const NON_EVENT_SLUGS: &[&str] = &["archive", "page"];

pub struct Serpentine {
    base_url: Url,
    max_detail_pages: usize,
}

impl Serpentine {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            max_detail_pages: MAX_DETAIL_PAGES,
        }
    }
}

/// Extract detail-page paths (`/whats-on/<slug>/`) from the listing HTML, in
/// document order, de-duplicated. Links may be absolute or relative; only the
/// path is kept so the caller resolves them against its own base URL.
pub fn parse_listing(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("a[href]").expect("valid selector");
    let base = Url::parse("https://www.serpentinegalleries.org/").expect("valid url");
    let mut out: Vec<String> = Vec::new();
    for a in doc.select(&sel) {
        let Some(href) = a.value().attr("href") else {
            continue;
        };
        let Ok(u) = base.join(href) else { continue };
        if u.host_str() != Some("www.serpentinegalleries.org") || u.query().is_some() {
            continue;
        }
        let Some(slug) = u
            .path()
            .strip_prefix(LISTING_PATH)
            .and_then(|r| r.strip_suffix('/'))
        else {
            continue;
        };
        let valid = !slug.is_empty()
            && !slug.contains('/')
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !NON_EVENT_SLUGS.contains(&slug);
        let path = format!("{LISTING_PATH}{slug}/");
        if valid && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// Parse one detail page into a [`RawEvent`] (None if it has no JSON-LD
/// Event). `path` is the page path, used as the stable source id.
pub fn parse_detail(html: &str, path: &str) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let event = extract_events(&doc).into_iter().next()?;
    let price_sel =
        Selector::parse(".banner__meta .meta__row--capitalise").expect("valid selector");
    let price_text = doc
        .select(&price_sel)
        .map(|e| clean_text(&e.text().collect::<String>()))
        .find(|t| t.to_lowercase().starts_with("price"));
    let slug = path.trim_matches('/').rsplit('/').next().unwrap_or(path);
    let source_url = event
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://www.serpentinegalleries.org{path}"));
    Some(RawEvent {
        source_event_id: slug.to_string(),
        source_url: Some(source_url),
        payload: json!({ "jsonld": event, "price_text": price_text }),
    })
}

/// Static details for Serpentine venues: (canonical name, address, lat, lng).
/// Coordinates are approximate building locations in Kensington Gardens.
pub fn venue_details(location: &str) -> Option<(&'static str, &'static str, f64, f64)> {
    let l = location.to_lowercase();
    if l.contains("north") || l.contains("sackler") {
        Some((
            "Serpentine North Gallery",
            "West Carriage Drive, Kensington Gardens, London W2 2AR",
            51.5055,
            -0.1718,
        ))
    } else if l.contains("pavilion") {
        Some((
            "Serpentine Pavilion",
            "Kensington Gardens, London W2 3XA",
            51.5046,
            -0.1756,
        ))
    } else if l.contains("serpentine") {
        // "Serpentine South Gallery", and the older "Serpentine Gallery".
        Some((
            "Serpentine South Gallery",
            "Kensington Gardens, London W2 3XA",
            51.5045,
            -0.1751,
        ))
    } else {
        None
    }
}

/// Normalise a Serpentine [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let ev = payload
        .get("jsonld")
        .ok_or_else(|| SourceError::Parse("payload without jsonld".into()))?;
    let title = ev
        .get("name")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("Event without name".into()))?;
    let starts_at = ev
        .get("startDate")
        .and_then(Value::as_str)
        .and_then(parse_london_wall_clock)
        .ok_or_else(|| SourceError::Parse(format!("no startDate for {title:?}")))?;
    let ends_at = ev
        .get("endDate")
        .and_then(Value::as_str)
        .and_then(parse_london_wall_clock)
        .filter(|e| *e > starts_at);

    let online = ev
        .get("eventAttendanceMode")
        .and_then(Value::as_str)
        .is_some_and(|m| m.contains("OnlineEventAttendanceMode"));
    let open_ended =
        ends_at.is_some_and(|e| e - starts_at > chrono::Duration::days(MAX_RANGE_DAYS));
    if online || open_ended {
        return Ok(None);
    }
    // Measured on the venue's literal span, before an all-day end is floored.
    let multi_day = ends_at.is_some_and(|e| e - starts_at > chrono::Duration::days(2));

    let all_day = is_london_midnight(starts_at);
    let ends_at = if all_day {
        ends_at
            .map(|e| london_to_utc(london_date(e).and_time(NaiveTime::MIN)))
            .filter(|e| *e > starts_at)
    } else {
        ends_at
    };

    let location = str_or_name(ev, "location");
    let (venue_name, address, lat, lng) = match location.and_then(venue_details) {
        Some((n, a, lat, lng)) => (
            Some(n.to_string()),
            Some(a.to_string()),
            Some(lat),
            Some(lng),
        ),
        None => (location.map(clean_text), None, None, None),
    };

    let price = match payload.get("price_text").and_then(Value::as_str) {
        Some(t) => parse_price(t),
        None => first_offer(ev)
            .map(|o| {
                let amount = match o.get("price") {
                    Some(Value::String(s)) => s.trim().trim_end_matches('.').parse().ok(),
                    Some(Value::Number(n)) => n.to_string().parse().ok(),
                    _ => None,
                };
                price_from_amounts(
                    amount,
                    amount,
                    o.get("priceCurrency").and_then(Value::as_str),
                )
            })
            .unwrap_or_default(),
    };

    let url = ev.get("url").and_then(Value::as_str).map(str::to_string);
    let slug_hint = url
        .as_deref()
        .map(|u| u.replace(['-', '/'], " "))
        .unwrap_or_default();
    let category = map_category(&[title.as_str(), slug_hint.as_str()]).unwrap_or(if multi_day {
        Category::Exhibition
    } else {
        Category::Community
    });

    let mut tags = vec!["contemporary-art".to_string()];
    if title.to_lowercase().contains("park nights") {
        tags.push("performance".into());
    }
    if location.is_some_and(|l| l.to_lowercase().contains("pavilion")) {
        tags.push("architecture".into());
    }

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, venue_name.as_deref()),
        description: clean_description(ev.get("description").and_then(Value::as_str)),
        title,
        venue_name,
        address,
        lat,
        lng,
        starts_at,
        ends_at,
        all_day,
        price,
        url,
        image_url: image_url(ev),
        category,
        tags,
    }))
}

#[async_trait]
impl Source for Serpentine {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listing_url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let listing = ctx.get_text(&listing_url).await?;
        let paths = parse_listing(&listing);
        if paths.is_empty() {
            return Err(SourceError::Parse(
                "no event links found on the listing page".into(),
            ));
        }
        let mut out = Vec::new();
        for path in paths.iter().take(self.max_detail_pages) {
            let url = match self.base_url.join(path) {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad detail path {path}: {e}"));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => {
                    if let Some(raw) = parse_detail(&html, path) {
                        out.push(raw);
                    }
                }
                Err(e) => ctx.report_error(format!("{path}: {e}")),
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }
}
