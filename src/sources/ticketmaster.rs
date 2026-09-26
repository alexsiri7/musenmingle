//! Ticketmaster Discovery API v2 source.
//!
//! * Endpoint: `GET {base}/discovery/v2/events.json`
//! * Scope: `city=London&countryCode=GB` (Greater London venues are listed with
//!   city "London").
//! * Classifications: segments **Arts & Theatre** (`KZFzniwnSyZfZ7v7na`) and
//!   **Miscellaneous** (`KZFzniwnSyZfZ7v7n1`). Those contain the genres we
//!   care about (Fine Art, Lecture/Seminar, Hobby/Special Interest Expos,
//!   Community/Civic, ...) but also theatre, comedy etc., which
//!   [`Ticketmaster::normalise`] skips via [`map_category`] returning `None`.
//! * Pagination: `size=100`, at most [`DEFAULT_MAX_PAGES`] pages, and never
//!   beyond the API's deep-paging limit (`size * page < 1000`).
//! * Rate limits: the API allows 5 req/s and 5000/day; the FetchContext
//!   per-domain limiter (default 1 req / 2 s) keeps us far below that.
//! * The API key comes from `TICKETMASTER_API_KEY` and is never logged
//!   (FetchContext redacts query strings).

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde_json::Value;
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, is_london_midnight, london_to_utc, map_category,
    parse_datetime, price_from_amounts,
};

pub const KEY: &str = "ticketmaster";
pub const SEGMENT_ARTS_THEATRE: &str = "KZFzniwnSyZfZ7v7na";
pub const SEGMENT_MISCELLANEOUS: &str = "KZFzniwnSyZfZ7v7n1";
pub const DEFAULT_PAGE_SIZE: u32 = 100;
pub const DEFAULT_MAX_PAGES: u32 = 5;
const DEEP_PAGING_LIMIT: u32 = 1000;

pub struct Ticketmaster {
    base_url: Url,
    api_key: String,
    page_size: u32,
    max_pages: u32,
}

impl Ticketmaster {
    pub fn new(base_url: Url, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            page_size: DEFAULT_PAGE_SIZE,
            max_pages: DEFAULT_MAX_PAGES,
        }
    }

    pub fn with_paging(mut self, page_size: u32, max_pages: u32) -> Self {
        self.page_size = page_size.clamp(1, 200);
        self.max_pages = max_pages.max(1);
        self
    }

    fn page_url(&self, page: u32) -> Result<Url, SourceError> {
        let mut url = self
            .base_url
            .join("/discovery/v2/events.json")
            .map_err(|e| SourceError::Config(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("apikey", &self.api_key)
            .append_pair("city", "London")
            .append_pair("countryCode", "GB")
            .append_pair(
                "segmentId",
                &format!("{SEGMENT_ARTS_THEATRE},{SEGMENT_MISCELLANEOUS}"),
            )
            .append_pair("locale", "*")
            .append_pair("sort", "date,asc")
            .append_pair("size", &self.page_size.to_string())
            .append_pair("page", &page.to_string());
        Ok(url)
    }
}

#[async_trait]
impl Source for Ticketmaster {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let mut out = Vec::new();
        let mut page = 0u32;
        loop {
            let body: Value = ctx.get_json(&self.page_url(page)?).await?;
            let events = body
                .pointer("/_embedded/events")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for ev in events {
                let Some(id) = ev.get("id").and_then(Value::as_str) else {
                    ctx.report_error("ticketmaster event without id");
                    continue;
                };
                out.push(RawEvent {
                    source_event_id: id.to_string(),
                    source_url: ev.get("url").and_then(Value::as_str).map(str::to_string),
                    payload: ev.clone(),
                });
            }
            let total_pages = body
                .pointer("/page/totalPages")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            page += 1;
            if page >= total_pages
                || page >= self.max_pages
                || (page + 1) * self.page_size > DEEP_PAGING_LIMIT
            {
                break;
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_event(&raw.payload)
    }
}

fn s<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "Undefined")
}

fn decimal(v: Option<&Value>) -> Option<Decimal> {
    match v? {
        Value::Number(n) => n.to_string().parse().ok(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Normalise one Discovery API event object.
pub fn normalise_event(ev: &Value) -> Result<Option<NewEvent>, SourceError> {
    let title = s(ev, "/name")
        .map(clean_text)
        .ok_or_else(|| SourceError::Parse("event without name".into()))?;

    // Classification hints, most specific first; the title goes last.
    let class = ev
        .pointer("/classifications")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find(|c| c.get("primary").and_then(Value::as_bool) == Some(true))
                .or_else(|| a.first())
        });
    let mut hints: Vec<&str> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    if let Some(c) = class {
        for p in ["/subGenre/name", "/genre/name", "/segment/name"] {
            if let Some(n) = s(c, p) {
                hints.push(n);
                let tag = n.to_lowercase();
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
        }
    }
    hints.push(&title);
    let Some(category) = map_category(&hints) else {
        return Ok(None);
    };

    let starts_at = s(ev, "/dates/start/dateTime")
        .and_then(parse_datetime)
        .or_else(|| {
            let date = s(ev, "/dates/start/localDate")?;
            let time = s(ev, "/dates/start/localTime").unwrap_or("00:00:00");
            chrono::NaiveDateTime::parse_from_str(&format!("{date}T{time}"), "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(london_to_utc)
        })
        .ok_or_else(|| SourceError::Parse(format!("no start date for {title:?}")))?;
    let ends_at = s(ev, "/dates/end/dateTime")
        .and_then(parse_datetime)
        .or_else(|| s(ev, "/dates/end/localDateTime").and_then(parse_datetime))
        .filter(|e| *e >= starts_at);
    let all_day = s(ev, "/dates/start/dateTime").is_none()
        && s(ev, "/dates/start/localTime").is_none()
        && ends_at.is_none_or(is_london_midnight);

    let venue = ev.pointer("/_embedded/venues/0");
    let venue_name = venue.and_then(|v| s(v, "/name")).map(clean_text);
    let address = venue.map(|v| {
        [
            s(v, "/address/line1"),
            s(v, "/address/line2"),
            s(v, "/city/name"),
            s(v, "/postalCode"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    });
    let coord = |p: &str| {
        venue.and_then(|v| v.pointer(p)).and_then(|x| match x {
            Value::String(s) => s.trim().parse::<f64>().ok(),
            Value::Number(n) => n.as_f64(),
            _ => None,
        })
    };
    let (lat, lng) = (coord("/location/latitude"), coord("/location/longitude"));

    let price = ev
        .pointer("/priceRanges/0")
        .map(|p| {
            price_from_amounts(
                decimal(p.get("min")),
                decimal(p.get("max")),
                p.get("currency").and_then(Value::as_str),
            )
        })
        .unwrap_or_default();

    // Prefer the widest 16:9 image.
    let image_url = ev
        .pointer("/images")
        .and_then(Value::as_array)
        .and_then(|imgs| {
            imgs.iter()
                .filter(|i| s(i, "/ratio") == Some("16_9"))
                .chain(imgs.iter())
                .max_by_key(|i| {
                    (
                        s(i, "/ratio") == Some("16_9"),
                        i.get("width").and_then(Value::as_u64).unwrap_or(0),
                    )
                })
        })
        .and_then(|i| s(i, "/url"))
        .map(str::to_string);

    let description = clean_description(s(ev, "/description").or(s(ev, "/info")));

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, venue_name.as_deref()),
        title,
        description,
        venue_name,
        address: address.filter(|a| !a.is_empty()),
        lat,
        lng,
        starts_at,
        ends_at,
        all_day,
        price,
        url: s(ev, "/url").map(str::to_string),
        image_url,
        category,
        tags,
    }))
}
