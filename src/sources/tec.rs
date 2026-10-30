//! The Events Calendar (TEC) — one platform source for the many London venue
//! sites that run WordPress with this plugin. Each venue is an
//! `events.sources` row with `platform = 'tec'` and its settings in `config`
//! ([`TecConfig`]), so adding a venue is a migration row, not code.
//!
//! * robots.txt is checked per venue when seeding: some sites disallow
//!   `/wp-json/` or every URL with a query (`/*?`), which rules out the API;
//!   those rows set `api_path: null` and read the list view instead.
//! * API path: `GET {api_path}?ends_after=<yesterday>&per_page=50&page=N`
//!   for N up to `total_pages`, at most [`MAX_API_PAGES`] pages. The API's
//!   default window (and `start_date`) only returns events *starting* today
//!   or later, hiding exhibitions that are already running; `ends_after`
//!   keeps them. TEC reads the date as 23:59:59 of that day, hence
//!   yesterday. Page URLs are built from `base_url`, not the response's
//!   `next_rest_url`. An empty programme is `Ok(vec![])`.
//! * Fallback: if the first API page is refused (401/403/404/410), isn't
//!   JSON or is disallowed by robots.txt, and the row has a `list_path`, the
//!   TEC list view's schema.org `Event` JSON-LD is read instead. Other
//!   failures (5xx, timeouts) are errors: an outage must reach the health
//!   checker. A list view without `Event` JSON-LD is an error too. An
//!   event whose JSON-LD description is cut short ("…") has its page fetched
//!   (from `base_url`) for the full description, read only for printed
//!   sessions; a page that fails is reported and the event kept as listed.
//!   No images are fetched.
//! * Ids are the event URL's path (occurrences of a recurring event carry
//!   their date in it), the one identifier both paths carry, so a venue
//!   moving between the API and the fallback keeps its ids.
//! * Times: every venue is in London and TEC stores the wall-clock time the
//!   venue typed, so `start_date`/`end_date` (API) and `startDate`/`endDate`
//!   (JSON-LD) are read as London wall clock. The API's `utc_*` fields and
//!   the JSON-LD offsets follow the site's timezone setting, which is often
//!   a fixed "UTC+0"/"UTC+1" and so an hour or two off in one half of the
//!   year (checked 2026-09-27: Freud Museum's 6:00 pm talk has
//!   `utc_start_date` 16:00; New River Studios' 7:30 pm gig has
//!   `+00:00` in October). All-day events are stored date-only. The
//!   venue's calendar can carry one start time for days that differ, so a
//!   timed API event spanning several days whose description has a block
//!   headed by its start date with a clock range ("Wednesday 14 October /
//!   Private View 5 – 9pm") starts at that time (#262). A timed event
//!   whose description prints dated sessions one per line, the first on its
//!   first day and the last after its last ("Wednesday 18th November
//!   11-4pm" … "Saturday 21st November 1-4pm"), takes them as its sessions:
//!   venues store a course as its first day (#385).
//! * Venue: the event's TEC venue, else the row's `config.venue` (sites
//!   that leave events without a venue), else unknown. Only London events
//!   are kept: the row's own venue, coordinates inside Greater London, or,
//!   without coordinates, an address mentioning London ("Bexley" is kept by
//!   its coordinates). Online events (TEC's venue "Online", `is_virtual`
//!   with no venue, JSON-LD `OnlineEventAttendanceMode`) and events hidden
//!   from listings are skipped.
//! * Category (API): a TEC category slug in `skip_categories` skips the
//!   event; otherwise the first slug in `category_map`, then
//!   `map_category` over the category names and title, then
//!   `default_category`; with none of those the event is skipped. The
//!   JSON-LD carries no categories: title, then description, then
//!   `default_category`. Without a `default_category` that leaves out film
//!   screenings, tours, gigs and the like; `qa_scope` tells the scraper
//!   check. On both paths, a title containing one of the row's
//!   `skip_keywords` skips the event.
//! * Price: `cost` text (its `currency_code` is unreliable: "USD" for "£"
//!   prices; a leading "Free –" is a £0 tier), widened by the description's
//!   ticket lines (online and livestream lines excluded) when it is paid; or
//!   the JSON-LD offers. TEC strings are HTML-escaped (the JSON-LD
//!   descriptions twice), so all text goes through `clean_text`.
//! * The content policy is decided per venue in its seed row.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, Utc};
use chrono_tz::Europe::London;
use rust_decimal::Decimal;
use scraper::{Html, Node, Selector};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::the_showroom::find_clock_range;
use super::{SkipReason, Source, SourceError, jsonld};
use crate::fetch::{FetchContext, FetchError};
use crate::model::{Category, NewEvent, Price, RawEvent, Session};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, in_london_bbox, is_london_midnight, london_date,
    london_to_utc, map_category, month_number, parse_london_wall_clock, parse_price,
    price_from_amounts, words,
};

/// `events.sources.platform` of TEC venues.
pub const PLATFORM: &str = "tec";
/// Upper bound on API pages per run (150 events).
pub const MAX_API_PAGES: u32 = 3;
const PER_PAGE: u32 = 50;
/// Longest gap between a course's printed sessions: a weekly class.
const MAX_SESSION_GAP_DAYS: i64 = 7;
const DEFAULT_API_PATH: &str = "/wp-json/tribe/events/v1/events";

/// A venue's `events.sources.config`. Unknown fields are rejected, so a
/// typo in a seed row is a recorded skip rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TecConfig {
    /// The TEC REST events endpoint; `null` reads only the list view.
    #[serde(default = "default_api_path")]
    pub api_path: Option<String>,
    /// The TEC list view, read when the API is off.
    #[serde(default)]
    pub list_path: Option<String>,
    /// Where events without a venue take place.
    #[serde(default)]
    pub venue: Option<DefaultVenue>,
    /// TEC category slug → category, checked before keyword matching.
    #[serde(default)]
    pub category_map: BTreeMap<String, Category>,
    /// TEC category slugs whose events are out of scope.
    #[serde(default)]
    pub skip_categories: Vec<String>,
    /// For venues whose uncategorised events are all one kind.
    #[serde(default)]
    pub default_category: Option<Category>,
    /// Words or phrases whose presence in a title skips the event.
    #[serde(default)]
    pub skip_keywords: Vec<String>,
}

fn default_api_path() -> Option<String> {
    Some(DEFAULT_API_PATH.to_string())
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultVenue {
    pub name: String,
    #[serde(default)]
    pub address: Option<String>,
}

impl TecConfig {
    /// Parse a row's `config`; NULL means all defaults.
    pub fn from_json(config: Option<&Value>) -> Result<Self, SkipReason> {
        let config: TecConfig = serde_json::from_value(config.cloned().unwrap_or(json!({})))
            .map_err(|e| SkipReason::InvalidConfig(e.to_string()))?;
        if config.api_path.is_none() && config.list_path.is_none() {
            return Err(SkipReason::InvalidConfig(
                "neither api_path nor list_path is set".into(),
            ));
        }
        Ok(config)
    }
}

pub struct Tec {
    key: String,
    base_url: Url,
    config: TecConfig,
}

impl Tec {
    pub fn from_row(key: &str, base_url: Url, config: Option<&Value>) -> Result<Self, SkipReason> {
        Ok(Self {
            key: key.to_string(),
            base_url,
            config: TecConfig::from_json(config)?,
        })
    }

    fn url(&self, path: &str) -> Result<Url, SourceError> {
        self.base_url
            .join(path)
            .map_err(|e| SourceError::Config(format!("{path:?}: {e}")))
    }

    async fn fetch_api(
        &self,
        ctx: &FetchContext,
        path: &str,
    ) -> Result<Vec<RawEvent>, SourceError> {
        let ends_after = london_date(Utc::now()) - Days::new(1);
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let mut url = self.url(path)?;
            url.query_pairs_mut()
                .append_pair("ends_after", &ends_after.format("%Y-%m-%d").to_string())
                .append_pair("per_page", &PER_PAGE.to_string())
                .append_pair("page", &page.to_string());
            let parsed = match ctx.get_json::<Value>(&url).await {
                Ok(body) => parse_api_page(&body),
                Err(e) => Err(e.into()),
            };
            let (items, total_pages) = match parsed {
                Ok(parsed) => parsed,
                Err(e) if page == 1 => return Err(e),
                Err(e) => {
                    ctx.report_error(format!("API page {page}: {e}"));
                    break;
                }
            };
            collect(ctx, items, &mut out);
            if page >= total_pages.min(MAX_API_PAGES) {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    async fn fetch_list(
        &self,
        ctx: &FetchContext,
        path: &str,
    ) -> Result<Vec<RawEvent>, SourceError> {
        let url = self.url(path)?;
        let items = parse_list_view(&ctx.get_text(&url).await?, &url);
        if items.is_empty() {
            return Err(SourceError::Parse(
                "no Event JSON-LD found on the list view".into(),
            ));
        }
        let mut out = Vec::new();
        collect(ctx, items, &mut out);
        for raw in &mut out {
            if list_excerpt_is_cut(raw) {
                self.add_page_description(ctx, raw).await;
            }
        }
        Ok(out)
    }

    /// Adds the event page's description to the payload, for the sessions
    /// it prints. The page is fetched from `base_url`, like the API pages;
    /// a failure, or a page without a description, is reported and the
    /// event kept as the list view gives it.
    async fn add_page_description(&self, ctx: &FetchContext, raw: &mut RawEvent) {
        let page = match self.url(&raw.source_event_id) {
            Ok(url) => ctx.get_text(&url).await.map_err(SourceError::from),
            Err(e) => Err(e),
        };
        match page.map(|html| page_description(&html)) {
            Ok(Some(description)) => {
                raw.payload["page_description"] = Value::String(description);
            }
            Ok(None) => ctx.report_error(format!(
                "{}: no event description on the page",
                raw.source_event_id
            )),
            Err(e) => ctx.report_error(format!("{}: {e}", raw.source_event_id)),
        }
    }
}

/// The full description on a TEC event page.
pub fn page_description(html: &str) -> Option<String> {
    let description =
        Selector::parse(".tribe-events-single-event-description").expect("valid selector");
    Html::parse_document(html)
        .select(&description)
        .next()
        .map(|el| el.inner_html())
}

fn list_excerpt_is_cut(raw: &RawEvent) -> bool {
    raw.payload
        .get("event")
        .and_then(jsonld_description)
        .is_some_and(|d| d.ends_with('…'))
}

/// Keep the first of each id; report items that have none.
fn collect(ctx: &FetchContext, items: Vec<Result<RawEvent, SourceError>>, out: &mut Vec<RawEvent>) {
    for item in items {
        match item {
            Ok(raw) if out.iter().any(|r| r.source_event_id == raw.source_event_id) => {}
            Ok(raw) => out.push(raw),
            Err(e) => ctx.report_error(e.to_string()),
        }
    }
}

/// Whether the API is off or unreadable (so the list view may serve
/// instead), as opposed to failing for now.
fn api_unavailable(e: &SourceError) -> bool {
    match e {
        SourceError::Fetch(FetchError::Status { status, .. }) => {
            matches!(status.as_u16(), 401 | 403 | 404 | 410)
        }
        SourceError::Fetch(FetchError::Decode { .. } | FetchError::RobotsDisallowed(_)) => true,
        _ => false,
    }
}

fn raw_event(url: Option<Url>, payload: Value) -> Result<RawEvent, SourceError> {
    let url = url
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or_else(|| {
            SourceError::Parse(format!(
                "event without a usable url: {:?}",
                payload
                    .pointer("/event/title")
                    .or(payload.pointer("/event/name"))
            ))
        })?;
    Ok(RawEvent {
        source_event_id: url.path().to_string(),
        source_url: Some(url.to_string()),
        payload,
    })
}

/// One page of the TEC REST API: its events (pruned to the fields
/// [`normalise_payload`] reads) and `total_pages`.
pub fn parse_api_page(
    body: &Value,
) -> Result<(Vec<Result<RawEvent, SourceError>>, u32), SourceError> {
    let events = body
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::Parse("API response has no events array".into()))?;
    let total_pages = body
        .get("total_pages")
        .and_then(Value::as_u64)
        .map_or(1, |n| n.min(u64::from(u32::MAX)) as u32);
    let items = events
        .iter()
        .map(|e| {
            let venue = e.get("venue").filter(|v| v.is_object()).map(|v| {
                json!({
                    "venue": v.get("venue"),
                    "address": v.get("address"),
                    "city": v.get("city"),
                    "zip": v.get("zip"),
                    "geo_lat": v.get("geo_lat"),
                    "geo_lng": v.get("geo_lng"),
                })
            });
            let categories: Vec<Value> = e
                .get("categories")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|c| json!({"name": c.get("name"), "slug": c.get("slug")}))
                .collect();
            let payload = json!({
                "via": "api",
                "event": {
                    "url": e.get("url"),
                    "title": e.get("title"),
                    "description": e.get("description"),
                    "all_day": e.get("all_day"),
                    "start_date": e.get("start_date"),
                    "end_date": e.get("end_date"),
                    "cost": e.get("cost"),
                    "image": e.pointer("/image/url"),
                    "categories": categories,
                    "venue": venue,
                    "is_virtual": e.get("is_virtual"),
                    "hide_from_listings": e.get("hide_from_listings"),
                },
            });
            let url = e
                .get("url")
                .and_then(Value::as_str)
                .and_then(|u| Url::parse(u).ok());
            raw_event(url, payload)
        })
        .collect();
    Ok((items, total_pages))
}

/// The `Event` JSON-LD nodes of a TEC list view fetched from `page_url`.
pub fn parse_list_view(html: &str, page_url: &Url) -> Vec<Result<RawEvent, SourceError>> {
    jsonld::extract_events(&Html::parse_document(html))
        .into_iter()
        .map(|node| {
            let url = node
                .get("url")
                .and_then(Value::as_str)
                .and_then(|u| page_url.join(u).ok());
            raw_event(url, json!({"via": "jsonld", "event": node}))
        })
        .collect()
}

struct Venue {
    name: String,
    address: Option<String>,
    lat: Option<f64>,
    lng: Option<f64>,
    from_config: bool,
}

impl Venue {
    fn from_config(config: &TecConfig) -> Option<Venue> {
        config.venue.as_ref().map(|v| Venue {
            name: clean_text(&v.name),
            address: v.address.as_deref().map(clean_text),
            lat: None,
            lng: None,
            from_config: true,
        })
    }

    fn in_london(&self) -> bool {
        if self.from_config {
            return true;
        }
        match (self.lat, self.lng) {
            (Some(lat), Some(lng)) => in_london_bbox(lat, lng),
            _ => [Some(&self.name), self.address.as_ref()]
                .into_iter()
                .flatten()
                .any(|t| t.to_lowercase().contains("london")),
        }
    }

    fn is_online(&self) -> bool {
        [Some(&self.name), self.address.as_ref()]
            .into_iter()
            .flatten()
            .any(|t| has_word(t, "online"))
    }
}

fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case(word))
}

/// Whether `title` contains `keyword`'s words, in order, as whole words.
fn has_keyword(title: &str, keyword: &str) -> bool {
    let (title, keyword) = (words(title), words(keyword));
    !keyword.is_empty()
        && title
            .windows(keyword.len())
            .any(|w| w == keyword.as_slice())
}

/// A number, or a number in a string.
fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
}

fn join_parts(parts: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let parts: Vec<String> = parts.into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// When an event happens.
struct Times {
    starts_at: DateTime<Utc>,
    ends_at: Option<DateTime<Utc>>,
    all_day: bool,
}

impl Times {
    fn all_day(first: NaiveDate, last: NaiveDate) -> Times {
        let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
        Times {
            starts_at: midnight(first),
            ends_at: (last > first).then(|| midnight(last)),
            all_day: true,
        }
    }

    fn timed(starts_at: DateTime<Utc>, ends_at: Option<DateTime<Utc>>) -> Times {
        Times {
            starts_at,
            ends_at: ends_at.filter(|e| *e > starts_at),
            all_day: false,
        }
    }
}

/// What both paths extract before the shared rules apply.
struct Parsed {
    title: String,
    description: Option<String>,
    times: Times,
    venue: Option<Venue>,
    price: Price,
    url: Option<String>,
    image_url: Option<String>,
    category: Option<Category>,
    tags: Vec<String>,
    /// Sessions printed in the description, replacing `times`.
    sessions: Option<Vec<Session>>,
}

/// Normalise a TEC [`RawEvent`] payload (either path) for a venue.
pub fn normalise_payload(
    payload: &Value,
    config: &TecConfig,
) -> Result<Option<NewEvent>, SourceError> {
    let event = payload
        .get("event")
        .ok_or_else(|| SourceError::Parse("payload without event".into()))?;
    let parsed = match payload.get("via").and_then(Value::as_str) {
        Some("api") => parse_api_event(event, config)?,
        Some("jsonld") => parse_jsonld_event(
            event,
            payload.get("page_description").and_then(Value::as_str),
            config,
        )?,
        other => return Err(SourceError::Parse(format!("unknown payload via {other:?}"))),
    };
    let Some(p) = parsed else {
        return Ok(None);
    };
    if config
        .skip_keywords
        .iter()
        .any(|k| has_keyword(&p.title, k))
    {
        return Ok(None);
    }
    let Some(venue) = p.venue else {
        return Ok(None);
    };
    if venue.is_online() || !venue.in_london() {
        return Ok(None);
    }
    let Some(category) = p.category.or(config.default_category) else {
        return Ok(None);
    };
    let mut event = NewEvent {
        sessions: Vec::new(),
        dedupe_key: String::new(),
        title: p.title,
        description: p.description,
        venue_name: Some(venue.name),
        address: venue.address,
        lat: venue.lat,
        lng: venue.lng,
        starts_at: p.times.starts_at,
        ends_at: p.times.ends_at,
        all_day: p.times.all_day,
        price: p.price,
        url: p.url,
        image_url: p.image_url,
        category,
        tags: p.tags,
    };
    if let Some(sessions) = p.sessions {
        event.set_sessions(sessions);
    }
    event.dedupe_key = dedupe_key(&event.title, event.starts_at, event.venue_name.as_deref());
    Ok(Some(event))
}

fn parse_api_event(event: &Value, config: &TecConfig) -> Result<Option<Parsed>, SourceError> {
    let flag = |k: &str| event.get(k).and_then(Value::as_bool).unwrap_or(false);
    let title = text_of(event.get("title"))
        .ok_or_else(|| SourceError::Parse("event without title".into()))?;
    let venue = event.get("venue").filter(|v| v.is_object()).and_then(|v| {
        Some(Venue {
            name: text_of(v.get("venue"))?,
            address: join_parts(["address", "city", "zip"].map(|k| text_of(v.get(k)))),
            lat: number(v.get("geo_lat")),
            lng: number(v.get("geo_lng")),
            from_config: false,
        })
    });
    if flag("hide_from_listings") || (flag("is_virtual") && venue.is_none()) {
        return Ok(None);
    }
    let categories: Vec<(String, String)> = event
        .get("categories")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some((
                c.get("slug")?.as_str()?.to_string(),
                text_of(c.get("name")).unwrap_or_default(),
            ))
        })
        .collect();
    if categories
        .iter()
        .any(|(slug, _)| config.skip_categories.contains(slug))
    {
        return Ok(None);
    }
    let category = categories
        .iter()
        .find_map(|(slug, _)| config.category_map.get(slug).copied())
        .or_else(|| {
            let hints: Vec<&str> = categories
                .iter()
                .map(|(_, name)| name.as_str())
                .chain([title.as_str()])
                .collect();
            map_category(&hints)
        });

    let date_field = |k: &str| {
        event
            .get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| SourceError::Parse(format!("{title:?} has no {k}")))
    };
    let start = date_field("start_date")?;
    let end = event.get("end_date").and_then(Value::as_str);
    let bad = |s: &str| SourceError::Parse(format!("{title:?}: unrecognised date {s:?}"));
    let times = if flag("all_day") {
        let day = |s: &str| {
            s.get(..10)
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .ok_or_else(|| bad(s))
        };
        let first = day(start)?;
        let last = end.map(day).transpose()?.unwrap_or(first);
        Times::all_day(first, last)
    } else {
        let mut starts_at = parse_london_wall_clock(start).ok_or_else(|| bad(start))?;
        let ends_at = end.and_then(parse_london_wall_clock);
        let first = london_date(starts_at);
        if ends_at.is_some_and(|e| london_date(e) > first)
            && let Some(t) = event
                .get("description")
                .and_then(Value::as_str)
                .and_then(|d| first_day_time(d, first))
        {
            starts_at = london_to_utc(first.and_time(t));
        }
        Times::timed(starts_at, ends_at)
    };
    let price = widen_with_ticket_lines(
        text_of(event.get("cost"))
            .map(|c| parse_cost(&c))
            .unwrap_or_default(),
        event.get("description").and_then(Value::as_str),
    );

    let description = event.get("description").and_then(Value::as_str);
    Ok(Some(Parsed {
        description: clean_description(description),
        sessions: description.and_then(|d| printed_sessions(d, &times)),
        times,
        venue: venue.or_else(|| Venue::from_config(config)),
        price,
        url: event.get("url").and_then(Value::as_str).map(str::to_string),
        image_url: event
            .get("image")
            .and_then(Value::as_str)
            .map(str::to_string),
        category,
        tags: categories
            .into_iter()
            .map(|(_, name)| name.to_lowercase())
            .filter(|n| !n.is_empty())
            .collect(),
        title,
    }))
}

/// The time of day of an instant in London.
fn london_time(t: DateTime<Utc>) -> NaiveTime {
    t.with_timezone(&London).time()
}

/// A multi-day event's first-day time from its description (#262): the
/// venue's calendar can hold one start time for a fair whose days differ
/// ("Wednesday 14 October / Private View 5 – 9pm"). The first dated line
/// on `day` gives its clock range's start.
fn first_day_time(description: &str, day: NaiveDate) -> Option<NaiveTime> {
    description_lines(description)
        .iter()
        .filter_map(|line| dated_line(line, day))
        .find_map(|(date, start, _)| (date == day).then_some(start))
}

/// The sessions a description prints one per line when the event's dates
/// hold only the first (#385: a four-day course stored as its first day,
/// "Wednesday 18th November 11-4pm" … "Saturday 21st November 1-4pm").
/// The sessions run from the first dated line, which must be on the event's
/// first day, while each line follows the one before by at most
/// [`MAX_SESSION_GAP_DAYS`], so a later date mentioned in passing ("our next
/// course starts …") is no session; they are taken only when they reach past
/// the event's last day, so they never shorten it.
fn printed_sessions(description: &str, times: &Times) -> Option<Vec<Session>> {
    if times.all_day {
        return None;
    }
    let first = london_date(times.starts_at);
    let last = london_date(times.ends_at.unwrap_or(times.starts_at));
    let mut lines: Vec<(NaiveDate, NaiveTime, Option<NaiveTime>)> = Vec::new();
    for line in description_lines(description)
        .iter()
        .filter_map(|line| dated_line(line, first))
    {
        if let Some(&(previous, ..)) = lines.last() {
            let gap = (line.0 - previous).num_days();
            if !(0..=MAX_SESSION_GAP_DAYS).contains(&gap) {
                break;
            }
        }
        lines.push(line);
    }
    let (first_line, last_line) = (lines.first()?, lines.last()?);
    (first_line.0 == first && last_line.0 > last).then(|| {
        lines
            .iter()
            .map(|&(date, start, end)| Session {
                starts_at: london_to_utc(date.and_time(start)),
                ends_at: end.map(|e| london_to_utc(date.and_time(e))),
            })
            .collect()
    })
}

/// The text of each `p`/`li` block of a description, then of each of its
/// `br`-separated lines when it has several.
fn description_lines(description: &str) -> Vec<String> {
    let blocks = Selector::parse("p, li").expect("valid selector");
    let mut lines = Vec::new();
    for block in Html::parse_fragment(description).select(&blocks) {
        lines.push(clean_text(&block.inner_html()));
        let mut parts = vec![String::new()];
        for node in block.descendants() {
            match node.value() {
                Node::Text(text) => parts.last_mut().expect("non-empty").push_str(text),
                Node::Element(e) if e.name() == "br" => parts.push(String::new()),
                _ => {}
            }
        }
        if parts.len() > 1 {
            lines.extend(parts.iter().map(|p| clean_text(p)));
        }
    }
    lines
}

/// A line that begins with a date and gives a clock range after it
/// ("Weds 14 October: Private View 5 – 9pm", "Saturday 21st November
/// 1-4pm …"): the date and the range. Without a written year the date is
/// the first on or after `from`; a printed weekday must be that date's.
fn dated_line(line: &str, from: NaiveDate) -> Option<(NaiveDate, NaiveTime, Option<NaiveTime>)> {
    let text = line.to_lowercase();
    let mut tokens = text
        .split_whitespace()
        .map(|t| t.trim_end_matches([',', ':']))
        .peekable();
    let weekday = tokens
        .next_if(|t| crate::hours::parse_day(t).is_some())
        .and_then(crate::hours::parse_day);
    let number = tokens.next()?;
    let number = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|s| number.strip_suffix(s))
        .unwrap_or(number);
    let day: u32 = number.parse().ok()?;
    let month = month_number(tokens.next()?)?;
    let year = tokens
        .next_if(|t| t.parse::<i32>().is_ok_and(|y| (1900..=2200).contains(&y)))
        .and_then(|t| t.parse().ok());
    let date = match year {
        Some(year) => NaiveDate::from_ymd_opt(year, month, day)?,
        None => NaiveDate::from_ymd_opt(from.year(), month, day)
            .filter(|d| *d >= from)
            .or_else(|| NaiveDate::from_ymd_opt(from.year() + 1, month, day))?,
    };
    if weekday.is_some_and(|w| u32::from(w) != date.weekday().number_from_monday()) {
        return None;
    }
    let rest: Vec<&str> = tokens.collect();
    let (start, end) = find_clock_range(&rest.join(" "))?;
    Some((date, start, end))
}

/// The JSON-LD path; `page_description` is the event page's full
/// description, fetched when the list view's was cut short.
fn parse_jsonld_event(
    node: &Value,
    page_description: Option<&str>,
    config: &TecConfig,
) -> Result<Option<Parsed>, SourceError> {
    let title =
        text_of(node.get("name")).ok_or_else(|| SourceError::Parse("event without name".into()))?;
    if node
        .get("eventAttendanceMode")
        .and_then(Value::as_str)
        .is_some_and(|m| m.ends_with("OnlineEventAttendanceMode"))
    {
        return Ok(None);
    }
    let start = node
        .get("startDate")
        .and_then(Value::as_str)
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no startDate")))?;
    let starts_at = parse_london_wall_clock(start)
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: unrecognised date {start:?}")))?;
    let ends_at = node
        .get("endDate")
        .and_then(Value::as_str)
        .and_then(parse_london_wall_clock);
    let end_of_day = NaiveTime::from_hms_opt(23, 59, 59).expect("valid time");
    let times = if is_london_midnight(starts_at)
        && ends_at.is_none_or(|e| is_london_midnight(e) || london_time(e) == end_of_day)
    {
        Times::all_day(
            london_date(starts_at),
            ends_at.map_or(london_date(starts_at), london_date),
        )
    } else {
        Times::timed(starts_at, ends_at)
    };

    let location = node.get("location").filter(|l| !l.is_null());
    let venue = location
        .and_then(|l| {
            let address = match l.get("address") {
                Some(Value::String(s)) => text_of(Some(&Value::String(s.clone()))),
                Some(a @ Value::Object(_)) => join_parts(
                    ["streetAddress", "addressLocality", "postalCode"].map(|k| text_of(a.get(k))),
                ),
                _ => None,
            };
            Some(Venue {
                name: jsonld::str_or_name(node, "location")
                    .map(clean_text)
                    .filter(|n| !n.is_empty())?,
                address,
                lat: number(l.pointer("/geo/latitude")),
                lng: number(l.pointer("/geo/longitude")),
                from_config: false,
            })
        })
        .or_else(|| Venue::from_config(config));

    let description = jsonld_description(node);
    let category = map_category(
        &[Some(title.as_str()), description.as_deref()].map(Option::unwrap_or_default),
    );
    Ok(Some(Parsed {
        description,
        sessions: page_description.and_then(|d| printed_sessions(d, &times)),
        times,
        venue,
        price: jsonld_price(node),
        url: node.get("url").and_then(Value::as_str).map(str::to_string),
        image_url: jsonld::image_url(node),
        category,
        tags: Vec::new(),
        title,
    }))
}

/// A JSON-LD node's description: an excerpt ending in "…" when the
/// venue's is longer.
fn jsonld_description(node: &Value) -> Option<String> {
    node.get("description")
        .and_then(Value::as_str)
        // TEC escapes the HTML description once more for JSON-LD.
        .and_then(|d| clean_description(Some(&clean_text(&d.replace("\\n", " ")))))
}

/// TEC writes a free cheapest tier as "Free" in a cost range ("Free –
/// £42.00"), which `parse_price` alone would read as just £42.
fn parse_cost(cost: &str) -> Price {
    let price = parse_price(cost);
    let free_floor = cost
        .get(..4)
        .is_some_and(|word| word.eq_ignore_ascii_case("free"))
        && cost[4..].trim_start().starts_with(['-', '–', '—']);
    if free_floor && price.max.is_some() {
        Price {
            min: Some(Decimal::ZERO),
            ..price
        }
    } else {
        price
    }
}

/// TEC's `cost` is what the venue typed into the price field, which can
/// omit tiers its description lists ("Solidarity Ticket – £26.94"); a paid
/// cost is widened by the description's ticket lines (`p`/`li` blocks with
/// "ticket", no online/livestream, same currency).
fn widen_with_ticket_lines(price: Price, description: Option<&str>) -> Price {
    let (Some(min), Some(max), Some(currency), Some(description)) =
        (price.min, price.max, price.currency.as_deref(), description)
    else {
        return price;
    };
    if price.is_free {
        return price;
    }
    let lines = Selector::parse("p, li").expect("valid selector");
    let tiers: Vec<Price> = Html::parse_fragment(description)
        .select(&lines)
        .map(|el| clean_text(&el.inner_html()))
        .filter(|line| {
            let lower = line.to_lowercase();
            lower.contains("ticket")
                && !lower.contains("online")
                && !lower.contains("livestream")
                && !lower.contains("live stream")
        })
        .map(|line| parse_price(&line))
        .filter(|p| p.currency.as_deref() == Some(currency))
        .collect();
    Price {
        min: [min]
            .into_iter()
            .chain(tiers.iter().filter_map(|p| p.min))
            .min(),
        max: tiers.iter().filter_map(|p| p.max).chain([max]).max(),
        ..price
    }
}

/// Price from the JSON-LD offers: numeric prices give a range, a text price
/// ("£30.00") is parsed as text.
fn jsonld_price(node: &Value) -> Price {
    let offers: Vec<&Value> = match node.get("offers") {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(o @ Value::Object(_)) => vec![o],
        _ => Vec::new(),
    };
    let prices: Vec<String> = offers
        .iter()
        .filter_map(|o| match o.get("price")? {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .filter(|p| !p.is_empty())
        .collect();
    let amounts: Vec<Decimal> = prices.iter().filter_map(|p| p.parse().ok()).collect();
    if amounts.is_empty() {
        return prices.first().map(|p| parse_price(p)).unwrap_or_default();
    }
    let currency = offers
        .iter()
        .find_map(|o| o.get("priceCurrency").and_then(Value::as_str));
    price_from_amounts(
        amounts.iter().min().copied(),
        amounts.iter().max().copied(),
        currency,
    )
}

#[async_trait]
impl Source for Tec {
    fn key(&self) -> &str {
        &self.key
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let api = match &self.config.api_path {
            Some(path) => Some(self.fetch_api(ctx, path).await),
            None => None,
        };
        match (api, &self.config.list_path) {
            (Some(Err(e)), Some(list_path)) if api_unavailable(&e) => {
                tracing::info!(source = %self.key, error = %e, "TEC API unavailable, reading the list view");
                self.fetch_list(ctx, list_path).await
            }
            (Some(result), _) => result,
            (None, Some(list_path)) => self.fetch_list(ctx, list_path).await,
            (None, None) => Err(SourceError::Config(
                "neither api_path nor list_path is set".into(),
            )),
        }
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload, &self.config)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        let c = &self.config;
        match c.default_category {
            None => c
                .category_map
                .values()
                .all(|cat| KEYWORD_CATEGORIES.contains(cat))
                .then_some(
                    "Only talks, workshops, exhibitions, fairs and community events: \
                     events whose site categories or title name one of those (lecture, \
                     class, course, discussion, display, meetup and the like). Anything \
                     else (film screenings, tours, gigs, concerts, performances) is left \
                     out on purpose.",
                ),
            Some(_) => (!c.skip_categories.is_empty()).then_some(
                "Events the site files under some of its own categories (such as \
                 tours, gigs or performances) are left out on purpose; everything \
                 else is kept.",
            ),
        }
    }
}

/// The categories `map_category` can give, which `Tec::qa_scope`'s note
/// names as kept; a `category_map` to any other would make the note false.
const KEYWORD_CATEGORIES: [Category; 5] = [
    Category::Talk,
    Category::Workshop,
    Category::Exhibition,
    Category::Expo,
    Category::Community,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn config(v: Value) -> TecConfig {
        TecConfig::from_json(Some(&v)).unwrap()
    }

    /// An API payload for a timed event at a London venue, with `patch`'s
    /// fields replacing the defaults.
    fn api(patch: Value) -> Value {
        let mut event = json!({
            "url": "https://venue.example/event/show/",
            "title": "Talk &#8211; <em>Sculpture</em>",
            "description": "<p>About the talk.</p>",
            "all_day": false,
            "start_date": "2026-09-27 12:30:00",
            "end_date": "2026-09-27 16:00:00",
            "cost": "",
            "image": null,
            "categories": [],
            "venue": {"venue": "Venue &#038; Gardens", "address": "1 Road", "city": "London",
                      "zip": "E1 1AA", "geo_lat": null, "geo_lng": null},
            "is_virtual": false,
            "hide_from_listings": false,
        });
        for (k, v) in patch.as_object().unwrap() {
            event[k] = v.clone();
        }
        json!({"via": "api", "event": event})
    }

    fn norm(payload: &Value, config_json: Value) -> Option<NewEvent> {
        normalise_payload(payload, &config(config_json)).unwrap()
    }

    #[test]
    fn config_defaults_and_nulls() {
        let defaults = TecConfig::from_json(None).unwrap();
        assert_eq!(defaults, config(json!({})));
        assert_eq!(defaults.api_path.as_deref(), Some(DEFAULT_API_PATH));
        assert_eq!(defaults.list_path, None);
        let list_only = config(json!({"api_path": null, "list_path": "/events/"}));
        assert_eq!(list_only.api_path, None);
        assert!(TecConfig::from_json(Some(&json!({"venue": {"name": "X", "lat": 51.5}}))).is_err());
    }

    #[test]
    fn times_are_london_wall_clock() {
        let event = norm(&api(json!({})), json!({"default_category": "talk"})).unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-09-27T11:30:00+00:00");
        assert_eq!(
            event.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-09-27T15:00:00+00:00")
        );
        assert!(!event.all_day);
        assert_eq!(event.title, "Talk – Sculpture");
        assert_eq!(event.venue_name.as_deref(), Some("Venue & Gardens"));
        assert_eq!(event.address.as_deref(), Some("1 Road, London, E1 1AA"));
        assert_eq!(event.description.as_deref(), Some("About the talk."));

        let same_end = api(json!({"end_date": "2026-09-27 12:30:00"}));
        assert_eq!(norm(&same_end, json!({})).unwrap().ends_at, None);
    }

    #[test]
    fn multi_day_events_take_the_first_days_time_from_the_description() {
        let start = |description: &str, end_date: &str| {
            let payload = api(json!({"start_date": "2026-10-14 18:00:00",
                "end_date": end_date, "description": description}));
            let event = norm(&payload, json!({"default_category": "exhibition"})).unwrap();
            (
                event.starts_at.to_rfc3339(),
                event.ends_at.map(|t| t.to_rfc3339()),
            )
        };
        let days = "<p><strong>Wednesday 14 October<br /></strong>Private View 5 – 9pm</p>\
                    <p><strong>Thursday 15 October<br /></strong>General Admission 11am – 5pm</p>";
        assert_eq!(
            start(days, "2026-10-18 18:00:00"),
            (
                "2026-10-14T16:00:00+00:00".into(),
                Some("2026-10-18T17:00:00+00:00".into())
            )
        );
        assert_eq!(
            start(days, "2026-10-14 21:00:00"),
            (
                "2026-10-14T16:00:00+00:00".into(),
                Some("2026-10-15T16:00:00+00:00".into())
            ),
            "the printed days outrun the event's: sessions"
        );
        let kept = "2026-10-14T17:00:00+00:00";
        let narrative =
            "<p>The fair runs from Wednesday 14th October – Sunday 18th October, 5 – 9pm</p>";
        assert_eq!(start(narrative, "2026-10-18 18:00:00").0, kept);
        let other_day = "<p>Thursday 15 October General Admission 11am – 5pm</p>";
        assert_eq!(start(other_day, "2026-10-18 18:00:00").0, kept);
        let list = "<ul><li>Weds 14 October: Private View 5 – 9pm</li></ul>";
        assert_eq!(
            start(list, "2026-10-18 18:00:00").0,
            "2026-10-14T16:00:00+00:00"
        );
        let suffix_and_year = "<p>14th October 2026 Private View 5 – 9pm</p>";
        assert_eq!(
            start(suffix_and_year, "2026-10-18 18:00:00").0,
            "2026-10-14T16:00:00+00:00"
        );
    }

    #[test]
    fn printed_sessions_extend_an_event_stored_as_its_first_day() {
        let course = "<p><b>ROOT &#8211; A four day course</b><br />\n\
                      Wednesday 18th November 11-4pm Rethinking the weed.</p>\
                      <p>Thursday 19th November 11-4pm Plant walk.</p>\
                      <p>Saturday 21st November 1-4pm Co-lead our open day.</p>";
        let sessions = |start: &str, end: &str, description: &str| {
            let payload = api(json!({"start_date": start, "end_date": end,
                                     "description": description}));
            let event = norm(&payload, json!({"default_category": "workshop"})).unwrap();
            let times = (
                event.starts_at.to_rfc3339(),
                event.ends_at.map(|t| t.to_rfc3339()),
            );
            (times, event.sessions.len())
        };
        assert_eq!(
            sessions("2026-11-18 11:00:00", "2026-11-18 16:00:00", course),
            (
                (
                    "2026-11-18T11:00:00+00:00".into(),
                    Some("2026-11-21T16:00:00+00:00".into())
                ),
                3
            )
        );
        let stored = (
            (
                "2026-11-18T11:00:00+00:00".into(),
                Some("2026-11-18T16:00:00+00:00".into()),
            ),
            0,
        );
        let mentioned = "<p>Wednesday 18th November 11-4pm.</p>\
                         <p>Our next course starts on 2 December.</p>";
        assert_eq!(
            sessions("2026-11-18 11:00:00", "2026-11-18 16:00:00", mentioned),
            stored,
            "a date without a time is no session"
        );
        let later_start = "<p>Thursday 19th November 11-4pm.</p>\
                           <p>Saturday 21st November 1-4pm.</p>";
        assert_eq!(
            sessions("2026-11-18 11:00:00", "2026-11-18 16:00:00", later_start),
            stored,
            "the first printed day must be the event's"
        );
        let within = "<p>Wednesday 18th November 11-4pm.</p><p>Saturday 21st November 1-4pm.</p>";
        assert_eq!(
            sessions("2026-11-18 11:00:00", "2026-11-28 16:00:00", within).1,
            0,
            "printed days inside the event's dates leave them"
        );
        let next_course = "<p>Wednesday 18th November 11-4pm Rethinking the weed.</p>\
                           <p>Saturday 21st November 1-4pm Co-lead our open day.</p>\
                           <p>Wednesday 2nd December 9am our next course starts.</p>";
        assert_eq!(
            sessions("2026-11-18 11:00:00", "2026-11-18 16:00:00", next_course),
            (
                (
                    "2026-11-18T11:00:00+00:00".into(),
                    Some("2026-11-21T16:00:00+00:00".into())
                ),
                2
            ),
            "a date well after the sessions is not one of them"
        );
        let earlier_aside = "<p>Wednesday 18th November 11-4pm Course day one.</p>\
                             <p>Thursday 19th November 11-4pm Course day two.</p>\
                             <p>Sunday 15th November 2-3pm Optional taster.</p>";
        assert_eq!(
            sessions("2026-11-18 11:00:00", "2026-11-18 16:00:00", earlier_aside),
            (
                (
                    "2026-11-18T11:00:00+00:00".into(),
                    Some("2026-11-19T16:00:00+00:00".into())
                ),
                2
            ),
            "a date before the first day is not rolled into next year"
        );
        let undated_aside = "<p>Wednesday 18th November 11-4pm.</p>\
                             <p>Thursday 19th November 11-4pm.</p>\
                             <p>15 November 2-3pm taster.</p>";
        assert_eq!(
            sessions("2026-11-18 11:00:00", "2026-11-18 16:00:00", undated_aside).1,
            2,
            "nor one without its weekday"
        );
        let new_year = "<p>Wednesday 30 December 6-8pm.</p><p>Wednesday 6 January 6-8pm.</p>";
        assert_eq!(
            sessions("2026-12-30 18:00:00", "2026-12-30 20:00:00", new_year).0,
            (
                "2026-12-30T18:00:00+00:00".into(),
                Some("2027-01-06T20:00:00+00:00".into())
            )
        );
    }

    #[test]
    fn all_day_events_are_date_only() {
        let one_day = api(json!({"all_day": true, "start_date": "2026-10-03 00:00:00",
                                 "end_date": "2026-10-03 23:59:59", "title": "Exhibition day"}));
        let event = norm(&one_day, json!({})).unwrap();
        assert!(event.all_day);
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-02T23:00:00+00:00");
        assert_eq!(event.ends_at, None);

        let run = api(json!({"all_day": true, "start_date": "2026-09-09 00:00:00",
                             "end_date": "2026-11-01 23:59:59", "title": "Exhibition"}));
        let event = norm(&run, json!({})).unwrap();
        assert_eq!(
            event.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-11-01T00:00:00+00:00")
        );
    }

    #[test]
    fn venue_falls_back_to_config_then_skips() {
        let no_venue = api(json!({"venue": []}));
        let with_default = json!({"venue": {"name": "Bookshop", "address": "5 Road, London N1"}});
        let event = norm(&no_venue, with_default).unwrap();
        assert_eq!(event.venue_name.as_deref(), Some("Bookshop"));
        assert_eq!(event.address.as_deref(), Some("5 Road, London N1"));
        assert_eq!(norm(&no_venue, json!({})), None);
    }

    #[test]
    fn london_by_coordinates_or_address() {
        let bexley = |lat: f64, lng: f64| {
            api(
                json!({"venue": {"venue": "Hall Place", "address": "Bourne Road", "city": "Bexley",
                                 "zip": "DA5 1PQ", "geo_lat": lat, "geo_lng": lng}}),
            )
        };
        assert!(norm(&bexley(51.449, 0.161), json!({})).is_some());
        assert_eq!(norm(&bexley(50.82, -0.14), json!({})), None);
        let no_geo_elsewhere = api(json!({"venue": {"venue": "Hall", "city": "Brighton"}}));
        assert_eq!(norm(&no_geo_elsewhere, json!({})), None);
        let string_geo =
            api(json!({"venue": {"venue": "Hall", "geo_lat": "51.5", "geo_lng": "-0.1"}}));
        assert_eq!(norm(&string_geo, json!({})).unwrap().lat, Some(51.5));
    }

    #[test]
    fn online_hidden_and_virtual_events_are_skipped() {
        let online = api(json!({"venue": {"venue": "Online"}}));
        assert_eq!(norm(&online, json!({})), None);
        let virtual_only = api(json!({"venue": [], "is_virtual": true}));
        let with_default = json!({"venue": {"name": "Museum"}});
        assert_eq!(norm(&virtual_only, with_default.clone()), None);
        let hidden = api(json!({"hide_from_listings": true}));
        assert_eq!(norm(&hidden, with_default), None);
    }

    #[test]
    fn categories_skip_then_map_then_keywords_then_default() {
        let cats = |slugs: &[(&str, &str)]| {
            json!({"title": "Afternoon",
                   "categories": slugs.iter().map(|(s, n)| json!({"slug": s, "name": n})).collect::<Vec<_>>()})
        };
        let rules = json!({"category_map": {"exhibitions": "exhibition"},
                           "skip_categories": ["music"]});
        let cat = |patch: Value, rules: Value| norm(&api(patch), rules).map(|e| e.category);
        assert_eq!(
            cat(cats(&[("exhibitions", "Exhibitions")]), rules.clone()),
            Some(Category::Exhibition)
        );
        assert_eq!(
            cat(
                cats(&[("exhibitions", "Exhibitions"), ("music", "Music")]),
                rules.clone()
            ),
            None
        );
        assert_eq!(
            cat(
                cats(&[("classes", "Classes &amp; Workshops")]),
                rules.clone()
            ),
            Some(Category::Workshop)
        );
        assert_eq!(cat(cats(&[]), rules.clone()), None);
        assert_eq!(
            cat(json!({"title": "A book talk"}), rules),
            Some(Category::Talk)
        );
        assert_eq!(
            cat(cats(&[]), json!({"default_category": "community"})),
            Some(Category::Community)
        );
        let tags = norm(
            &api(cats(&[("classes", "Classes &amp; Workshops")])),
            json!({}),
        )
        .unwrap()
        .tags;
        assert_eq!(tags, ["classes & workshops"]);
    }

    #[test]
    fn the_qa_scope_names_what_is_left_out() {
        let tec = |config: Value| {
            Tec::from_row(
                "tec-x",
                Url::parse("https://x.test/").unwrap(),
                Some(&config),
            )
            .unwrap()
        };
        let scope = tec(json!({"skip_categories": ["tours"]}))
            .qa_scope()
            .unwrap();
        for word in [
            "talks",
            "workshops",
            "exhibitions",
            "film screenings",
            "tours",
        ] {
            assert!(scope.contains(word), "{word}");
        }
        let skips = tec(json!({"default_category": "talk", "skip_categories": ["music"]}))
            .qa_scope()
            .unwrap();
        assert!(skips.contains("left out on purpose"), "{skips}");
        assert!(
            tec(json!({"default_category": "talk"}))
                .qa_scope()
                .is_none()
        );
        assert!(
            tec(json!({"category_map": {"gigs": "music"}}))
                .qa_scope()
                .is_none()
        );
    }

    #[test]
    fn cost_text_sets_price_and_image_false_is_none() {
        let paid = norm(&api(json!({"cost": "&#163;1.50"})), json!({})).unwrap();
        assert_eq!(paid.price.currency.as_deref(), Some("GBP"));
        assert_eq!(
            paid.price.min.map(|d| d.to_string()).as_deref(),
            Some("1.50")
        );
        assert!(
            norm(&api(json!({"cost": "Free"})), json!({}))
                .unwrap()
                .price
                .is_free
        );
        let (items, _) = parse_api_page(&json!({"events": [
            {"url": "https://venue.example/event/x/2026-09-27/", "title": "X", "image": false}
        ]}))
        .unwrap();
        let raw = items.into_iter().next().unwrap().unwrap();
        assert_eq!(raw.source_event_id, "/event/x/2026-09-27/");
        assert_eq!(raw.payload["event"]["image"], Value::Null);
    }

    #[test]
    fn cost_is_widened_by_description_ticket_lines() {
        let range = |cost: &str, description: &str| {
            let price = norm(
                &api(json!({"cost": cost, "description": description})),
                json!({}),
            )
            .unwrap()
            .price;
            (
                price.min.map(|d| d.to_string()),
                price.max.map(|d| d.to_string()),
                price.currency,
                price.is_free,
            )
        };
        let tiers = "<p>Solidarity Ticket &#8211; &#163;26.94</p><p>Standard Ticket – £16.40</p>\
                     <p>A limited number of bursary tickets (£11.13) are available.</p>";
        let cost = "£11.13 – £16.40";
        let stored = |max: &str| {
            (
                Some("11.13".to_string()),
                Some(max.to_string()),
                Some("GBP".to_string()),
                false,
            )
        };
        assert_eq!(range(cost, tiers), stored("26.94"));
        assert_eq!(
            range(
                cost,
                "<p>Online ticket – £5</p><ul><li>Livestream ticket £40</li></ul>"
            ),
            stored("16.40")
        );
        assert_eq!(range(cost, "<p>The book costs £30.</p>"), stored("16.40"));
        assert_eq!(range(cost, "<p>Tickets $30</p>"), stored("16.40"));
        assert_eq!(range("", "<p>Tickets £25</p>"), (None, None, None, false));
        assert_eq!(
            range("Free &#8211; &#163;42.00", ""),
            (
                Some("0".to_string()),
                Some("42.00".to_string()),
                Some("GBP".to_string()),
                false
            )
        );
        assert_eq!(range("FREE – £42.00", "").0, Some("0".to_string()));
        assert!(range("Free", "<p>Tickets £25</p>").3);
        // A cheaper tier lowers the floor; a restated one keeps the cost's text.
        let gbp = |min: &str, max: &str| {
            (
                Some(min.to_string()),
                Some(max.to_string()),
                Some("GBP".to_string()),
                false,
            )
        };
        assert_eq!(
            range("£16.40 – £16.40", "<p>Bursary Ticket – £8.00</p>"),
            gbp("8.00", "16.40")
        );
        assert_eq!(
            range("£11.10 – £16.40", "<p>Bursary ticket (£11.1)</p>"),
            gbp("11.10", "16.40")
        );
    }

    #[test]
    fn api_events_without_url_are_errors() {
        let (items, total_pages) =
            parse_api_page(&json!({"events": [{"title": "X"}], "total_pages": 4})).unwrap();
        assert_eq!(total_pages, 4);
        assert!(items[0].is_err());
        assert!(parse_api_page(&json!({"message": "no"})).is_err());
    }

    fn jsonld(patch: Value) -> Value {
        let mut node = json!({
            "@type": "Event",
            "name": "Opening Night &#8211; Show",
            "url": "https://venue.example/event/show/",
            "description": "&lt;p&gt;An &lt;b&gt;opening&lt;/b&gt;.&lt;/p&gt;\\n",
            "startDate": "2026-10-03T19:30:00+00:00",
            "endDate": "2026-10-03T23:00:00+00:00",
            "eventAttendanceMode": "https://schema.org/OfflineEventAttendanceMode",
            "location": {"@type": "Place", "name": "The Rum Factory",
                         "address": {"@type": "PostalAddress", "streetAddress": "49 Pennington Street",
                                     "addressLocality": "London", "postalCode": "E1W 2BD"}},
            "offers": [{"price": "0", "priceCurrency": "GBP"}, {"price": "5", "priceCurrency": "GBP"}],
        });
        for (k, v) in patch.as_object().unwrap() {
            node[k] = v.clone();
        }
        json!({"via": "jsonld", "event": node})
    }

    #[test]
    fn jsonld_times_ignore_the_offset() {
        let event = norm(
            &jsonld(json!({})),
            json!({"default_category": "exhibition"}),
        )
        .unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-03T18:30:00+00:00");
        assert_eq!(event.title, "Opening Night – Show");
        assert_eq!(event.description.as_deref(), Some("An opening."));
        assert_eq!(
            event.address.as_deref(),
            Some("49 Pennington Street, London, E1W 2BD")
        );
        assert_eq!(event.price.min.map(|d| d.to_string()).as_deref(), Some("0"));
        assert_eq!(event.price.max.map(|d| d.to_string()).as_deref(), Some("5"));

        let all_day = jsonld(json!({"startDate": "2026-09-09T00:00:00+01:00",
                                    "endDate": "2026-11-01T23:59:59+00:00"}));
        let event = norm(&all_day, json!({"default_category": "exhibition"})).unwrap();
        assert!(event.all_day);
        assert_eq!(
            event.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-11-01T00:00:00+00:00")
        );
    }

    #[test]
    fn jsonld_venue_price_and_skips() {
        let rules = json!({"default_category": "talk",
                           "venue": {"name": "Bookshop", "address": "5 Road, London N1"}});
        let no_location = norm(&jsonld(json!({"location": null})), rules.clone()).unwrap();
        assert_eq!(no_location.venue_name.as_deref(), Some("Bookshop"));
        let text_price = norm(
            &jsonld(json!({"offers": {"price": "£30.00"}})),
            rules.clone(),
        )
        .unwrap();
        assert_eq!(
            text_price.price.min.map(|d| d.to_string()).as_deref(),
            Some("30.00")
        );
        let online =
            jsonld(json!({"eventAttendanceMode": "https://schema.org/OnlineEventAttendanceMode"}));
        assert_eq!(norm(&online, rules), None);
        assert_eq!(norm(&jsonld(json!({})), json!({})), None, "no category");
    }

    #[test]
    fn skip_keywords_match_whole_title_words() {
        let rules = json!({"skip_keywords": ["yoga"], "default_category": "exhibition"});
        let yoga = api(json!({"title": "Yoga Sessions at the Rum Factory"}));
        assert_eq!(norm(&yoga, rules.clone()), None);
        let yoga = jsonld(json!({"name": "Yoga Sessions at the Lakeside Centre"}));
        assert_eq!(norm(&yoga, rules.clone()), None);
        let mentions_yoga = jsonld(json!({"name": "Open To Ideas Workshop: Table Talk",
                                          "description": "With a yoga teacher."}));
        assert_eq!(
            norm(&mentions_yoga, rules).map(|e| e.category),
            Some(Category::Workshop)
        );
        let yogurt = api(json!({"title": "Yogurt tasting"}));
        let rules = json!({"skip_keywords": ["yoga"], "default_category": "community"});
        assert!(norm(&yogurt, rules).is_some());
        assert_eq!(
            config(json!({"skip_keywords": ["yoga"]})).skip_keywords,
            ["yoga"]
        );
    }

    #[test]
    fn jsonld_category_title_then_description_then_default() {
        let cat = |name: &str, description: &str, rules: Value| {
            norm(
                &jsonld(json!({"name": name, "description": description})),
                rules,
            )
            .map(|e| e.category)
        };
        assert_eq!(
            cat(
                "Bow Families: paper lanterns",
                "A hands-on workshop.",
                json!({})
            ),
            Some(Category::Workshop)
        );
        assert_eq!(
            cat("Book talk", "Part of the exhibition.", json!({})),
            Some(Category::Talk)
        );
        let hunjan = "Bhajan Hunjan: speaking through materials";
        assert_eq!(cat(hunjan, "Forty years of work.", json!({})), None);
        assert_eq!(
            cat(
                hunjan,
                "Forty years of work.",
                json!({"default_category": "exhibition"})
            ),
            Some(Category::Exhibition)
        );
        let bow_arts = json!({"default_category": "exhibition", "skip_keywords": ["yoga"]});
        let late = jsonld(json!({
            "name": "Late Opening &amp; Curator Tours: Bhajan Hunjan: speaking through materials"
        }));
        assert_eq!(
            norm(&late, bow_arts).map(|e| e.category),
            Some(Category::Exhibition)
        );
    }
}
