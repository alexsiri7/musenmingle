//! Somerset House (Strand) — hand-written scraper over the site's embedded
//! page data.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *` with
//!   no `Disallow` lines, so `/whats-on` is allowed.
//! * There is no schema.org JSON-LD on the listing or detail pages, and the
//!   rendered cards carry only utility classes and human dates ("Every Tue &
//!   Sat"). Instead of CSS selectors we read the SSR app state that every page
//!   embeds in `<script id="props" type="application/json">`: the listing's
//!   `data.page.items.edges[].node` objects have ISO dates, event types,
//!   `priceFree` and pagination info. Detail pages have no dates, so the
//!   listing is the only source and no detail pages are fetched.
//! * Quirk: the props JSON is invalid as served (a code snippet contains
//!   `<\!--`, an illegal JSON escape), so `<\!` is rewritten to `<!` before
//!   parsing.
//! * Pagination is cumulative ("load more": `?page=2` repeats page 1's items);
//!   every page up to [`MAX_LISTING_PAGES`] is requested and items are
//!   de-duplicated by path, which is also correct for true paging.
//! * Times are naive London wall-clock times. Quirk: some single-evening
//!   events have a placeholder `00:00` start with the end at start +
//!   `duration` (e.g. `00:00 → 03:00` for "6–9pm"); for those the start time
//!   is taken from the human `timeText`. Exhibitions are date-only ranges.
//! * Known site data errors are stored as published: some ends are an hour
//!   off (`18:00 → 23:00` for "6–10pm") and some `dateStart`s disagree with
//!   the human date text.
//! * Permanent or standing items (the Courtauld Gallery, a twice-weekly tour)
//!   are ranges longer than [`MAX_RANGE_DAYS`] and are skipped (`Ok(None)`).
//! * Items are at Somerset House unless their free-text `space` contains a
//!   postcode outside WC2R (the "3 Evenings" series is in Wapping).

use async_trait::async_trait;
use chrono::{Duration, NaiveTime};
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, map_category,
    parse_london_wall_clock, parse_price,
};

pub const KEY: &str = "somerset-house";
/// Upper bound on listing pages fetched per run (12 items per page).
pub const MAX_LISTING_PAGES: u32 = 10;
/// Ranges longer than this are permanent or standing programmes.
pub const MAX_RANGE_DAYS: i64 = 366;
const LISTING_PATH: &str = "/whats-on";
const SITE: &str = "https://www.somersethouse.org.uk";
const VENUE_NAME: &str = "Somerset House";
const VENUE_ADDRESS: &str = "Strand, London WC2R 1LA";
const VENUE_OUTWARD_CODE: &str = "WC2R";
const VENUE_LAT: f64 = 51.5110;
const VENUE_LNG: f64 = -0.1171;

pub struct SomersetHouse {
    base_url: Url,
    max_listing_pages: u32,
}

impl SomersetHouse {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            max_listing_pages: MAX_LISTING_PAGES,
        }
    }
}

#[derive(Debug)]
pub struct ListingPage {
    pub events: Vec<RawEvent>,
    /// Descriptions of listing nodes that could not become a [`RawEvent`].
    pub rejected: Vec<String>,
    pub total_pages: u32,
}

/// Parse one `/whats-on?page=N` page: one [`RawEvent`] per listing node, with
/// the node JSON as payload and its path below `/whats-on/` as the source id.
/// Nodes without such a path are listed in [`ListingPage::rejected`].
pub fn parse_listing(html: &str) -> Result<ListingPage, SourceError> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("script#props").expect("valid selector");
    let script = doc
        .select(&sel)
        .next()
        .ok_or_else(|| SourceError::Parse("no script#props on the listing page".into()))?;
    let json = script.text().collect::<String>().replace("<\\!", "<!");
    let props: Value = serde_json::from_str(&json)
        .map_err(|e| SourceError::Parse(format!("script#props is not valid JSON: {e}")))?;
    let items = props.pointer("/data/page/items");
    let edges = items
        .and_then(|i| i.get("edges"))
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::Parse("script#props has no data.page.items.edges".into()))?;
    let site = Url::parse(SITE).expect("valid url");
    let mut events = Vec::new();
    let mut rejected = Vec::new();
    for edge in edges {
        match listing_event(edge, &site) {
            Some(raw) => events.push(raw),
            None => {
                let label = ["/node/url", "/node/title"]
                    .iter()
                    .find_map(|p| edge.pointer(p).and_then(Value::as_str))
                    .unwrap_or("?");
                rejected.push(format!("unusable listing item {label:?}"));
            }
        }
    }
    let total_pages = items
        .and_then(|i| i.pointer("/pageInfo/totalPages"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(1);
    Ok(ListingPage {
        events,
        rejected,
        total_pages,
    })
}

fn listing_event(edge: &Value, site: &Url) -> Option<RawEvent> {
    let node = edge.get("node")?;
    let path = node.get("url").and_then(Value::as_str)?;
    let id = path
        .strip_prefix(LISTING_PATH)?
        .strip_prefix('/')
        .filter(|id| !id.is_empty())?;
    Some(RawEvent {
        source_event_id: id.to_string(),
        source_url: Some(site.join(path).ok()?.to_string()),
        payload: node.clone(),
    })
}

/// Hour and minute of a clock token such as `7` or `8.30` starting at `i`,
/// and the index just after it.
fn clock_token(chars: &[char], i: usize) -> Option<(u32, u32, usize)> {
    let mut end = i;
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }
    if end == i || (i > 0 && chars[i - 1].is_ascii_digit()) {
        return None;
    }
    let hour: u32 = chars[i..end].iter().collect::<String>().parse().ok()?;
    let minute_digits = chars.get(end + 1..end + 3);
    let has_minutes = chars.get(end) == Some(&'.')
        && minute_digits.is_some_and(|d| d.iter().all(char::is_ascii_digit))
        && !chars.get(end + 3).is_some_and(char::is_ascii_digit);
    if has_minutes {
        let minute: u32 = chars[end + 1..end + 3]
            .iter()
            .collect::<String>()
            .parse()
            .ok()?;
        Some((hour, minute, end + 3))
    } else {
        Some((hour, 0, end))
    }
}

fn skip_spaces(chars: &[char], mut i: usize) -> usize {
    while chars.get(i).is_some_and(|c| *c == ' ') {
        i += 1;
    }
    i
}

/// `Some(true)` for `pm`, `Some(false)` for `am` as a whole word at `i`.
fn meridiem(chars: &[char], i: usize) -> Option<bool> {
    let word: String = chars.get(i..i + 2)?.iter().collect::<String>();
    if chars.get(i + 2).is_some_and(|c| c.is_alphabetic()) {
        return None;
    }
    match word.to_lowercase().as_str() {
        "am" => Some(false),
        "pm" => Some(true),
        _ => None,
    }
}

/// First start time in a human time text: `"6–9pm"` → 18:00, `"From 7pm"` →
/// 19:00, `"6.30–10.30pm"` → 18:30. In a range the meridiem comes from the
/// end time. Returns `None` when no time with a known meridiem is found.
pub fn parse_start_time(text: &str) -> Option<NaiveTime> {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        let Some((hour, minute, end)) = clock_token(&chars, i) else {
            continue;
        };
        if !(1..=12).contains(&hour) || minute > 59 {
            continue;
        }
        let j = skip_spaces(&chars, end);
        let pm = match chars.get(j) {
            Some('-' | '–' | '—') => {
                let k = skip_spaces(&chars, j + 1);
                clock_token(&chars, k)
                    .and_then(|(_, _, e)| meridiem(&chars, skip_spaces(&chars, e)))
            }
            _ => meridiem(&chars, j),
        };
        let Some(pm) = pm else { continue };
        let hour = match (pm, hour) {
            (false, 12) => 0,
            (true, 12) => 12,
            (true, h) => h + 12,
            (false, h) => h,
        };
        return NaiveTime::from_hms_opt(hour, minute, 0);
    }
    None
}

/// Outward code (e.g. `E1W`) of the first UK postcode in `text`.
fn postcode_outward(text: &str) -> Option<String> {
    let tokens: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    tokens.windows(2).find_map(|pair| {
        let (outward, inward) = (pair[0], pair[1].as_bytes());
        let outward_ok = (2..=4).contains(&outward.len())
            && outward.chars().all(|c| c.is_ascii_alphanumeric())
            && outward.starts_with(|c: char| c.is_ascii_alphabetic())
            && outward.chars().any(|c| c.is_ascii_digit());
        let inward_ok = inward.len() == 3
            && inward[0].is_ascii_digit()
            && inward[1..].iter().all(u8::is_ascii_alphabetic);
        (outward_ok && inward_ok).then(|| outward.to_ascii_uppercase())
    })
}

struct Venue {
    name: String,
    address: String,
    lat: Option<f64>,
    lng: Option<f64>,
}

fn venue(space: &str) -> Venue {
    match postcode_outward(space) {
        Some(outward) if outward != VENUE_OUTWARD_CODE => {
            let lines: Vec<&str> = space
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            Venue {
                name: clean_text(space.split([',', '\r', '\n']).next().unwrap_or(space)),
                address: clean_text(&lines.join(", ")),
                lat: None,
                lng: None,
            }
        }
        _ => Venue {
            name: VENUE_NAME.to_string(),
            address: VENUE_ADDRESS.to_string(),
            lat: Some(VENUE_LAT),
            lng: Some(VENUE_LNG),
        },
    }
}

fn node_str<'a>(node: &'a Value, pointer: &str) -> Option<&'a str> {
    node.pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// Normalise one listing node (a [`RawEvent`] payload).
pub fn normalise_payload(node: &Value) -> Result<Option<NewEvent>, SourceError> {
    let title = node_str(node, "/title")
        .map(clean_text)
        .ok_or_else(|| SourceError::Parse("listing item without title".into()))?;
    let listed_start = node_str(node, "/dateStart")
        .and_then(parse_london_wall_clock)
        .ok_or_else(|| SourceError::Parse(format!("no dateStart for {title:?}")))?;
    let listed_end = node_str(node, "/dateEnd").and_then(parse_london_wall_clock);
    if listed_end.is_some_and(|e| e - listed_start > Duration::days(MAX_RANGE_DAYS)) {
        return Ok(None);
    }

    let date = london_date(listed_start);
    let placeholder_start = listed_start == london_to_utc(date.and_time(NaiveTime::MIN));
    let duration = node
        .get("duration")
        .and_then(Value::as_i64)
        .filter(|d| *d > 0);
    let (starts_at, ends_at) = match duration {
        Some(minutes) if placeholder_start => {
            match node_str(node, "/timeText").and_then(parse_start_time) {
                Some(time) => {
                    let start = london_to_utc(date.and_time(time));
                    (start, Some(start + Duration::minutes(minutes)))
                }
                None => (listed_start, None),
            }
        }
        _ => (listed_start, listed_end.filter(|e| *e > listed_start)),
    };

    let venue = venue(node_str(node, "/space").unwrap_or_default());

    let price = if node.get("priceFree").and_then(Value::as_bool) == Some(true) {
        parse_price("Free")
    } else {
        Price::default()
    };

    let event_types: &[Value] = node
        .get("eventTypes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut hints = vec![title.as_str()];
    hints.extend(event_types.iter().filter_map(|t| node_str(t, "/title")));
    let category = map_category(&hints).unwrap_or(Category::Community);
    let mut tags: Vec<String> = Vec::new();
    for slug in event_types.iter().filter_map(|t| node_str(t, "/slug")) {
        if !tags.iter().any(|t| t == slug) {
            tags.push(slug.to_string());
        }
    }

    let url = node_str(node, "/url")
        .and_then(|path| Url::parse(SITE).ok()?.join(path).ok())
        .map(String::from);
    let image_url = node_str(node, "/listingImage/original")
        .or_else(|| node_str(node, "/heroMedia/image/original"))
        .map(str::to_string);

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(&venue.name)),
        description: clean_description(node.get("listingText").and_then(Value::as_str)),
        title,
        venue_name: Some(venue.name),
        address: Some(venue.address),
        lat: venue.lat,
        lng: venue.lng,
        starts_at,
        ends_at,
        price,
        url,
        image_url,
        category,
        tags,
    }))
}

#[async_trait]
impl Source for SomersetHouse {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let page_url = |n: u32| {
            self.base_url
                .join(&format!("{LISTING_PATH}?page={n}"))
                .map_err(|e| SourceError::Config(e.to_string()))
        };
        let first = parse_listing(&ctx.get_text(&page_url(1)?).await?)?;
        let last_page = first.total_pages.min(self.max_listing_pages);
        let mut out = Vec::new();
        let mut rejected = Vec::new();
        let mut merge = |page: ListingPage| {
            for raw in page.events {
                if !out
                    .iter()
                    .any(|r: &RawEvent| r.source_event_id == raw.source_event_id)
                {
                    out.push(raw);
                }
            }
            // Pages are cumulative, so the same bad item recurs on every page.
            for item in page.rejected {
                if !rejected.contains(&item) {
                    rejected.push(item);
                }
            }
        };
        merge(first);
        for n in 2..=last_page {
            let page = match ctx.get_text(&page_url(n)?).await {
                Ok(html) => parse_listing(&html),
                Err(e) => Err(e.into()),
            };
            match page {
                Ok(page) => merge(page),
                Err(e) => ctx.report_error(format!("page {n}: {e}")),
            }
        }
        for item in rejected {
            ctx.report_error(item);
        }
        if out.is_empty() {
            return Err(SourceError::Parse(
                "no events found on the listing page".into(),
            ));
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

    #[test]
    fn start_time_from_human_text() {
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0);
        for (text, expected) in [
            ("6–9pm", t(18, 0)),
            ("7-8.30pm", t(19, 0)),
            ("6.30–9pm", t(18, 30)),
            ("From 7pm", t(19, 0)),
            ("6–7.30 pm", t(18, 0)),
            ("Sessions from 8pm", t(20, 0)),
            ("Fri 16 Oct 7pm\r\nSat 17 Oct 6.30pm", t(19, 0)),
            ("12–6pm", t(12, 0)),
            ("12am", t(0, 0)),
            ("10am–6pm", t(10, 0)),
            ("", None),
            ("6–9 Programme", None),
            ("6–9 amazing", None),
            ("Open daily", None),
        ] {
            assert_eq!(parse_start_time(text), expected, "{text:?}");
        }
    }

    #[test]
    fn placeholder_start_without_a_clock_time_keeps_the_date_only() {
        let node = serde_json::json!({
            "title": "Talk",
            "dateStart": "2026-10-07T00:00",
            "dateEnd": "2026-10-07T03:00",
            "duration": 180,
            "timeText": "Open daily",
        });
        let event = normalise_payload(&node).unwrap().unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-06T23:00:00+00:00");
        assert_eq!(event.ends_at, None);
    }

    #[test]
    fn postcode_outward_code() {
        assert_eq!(
            postcode_outward("Wapping Hydraulic Power Station, E1W 3SF").as_deref(),
            Some("E1W")
        );
        assert_eq!(
            postcode_outward("Strand, London wc2r 1la").as_deref(),
            Some("WC2R")
        );
        assert_eq!(
            postcode_outward(VENUE_ADDRESS).as_deref(),
            Some(VENUE_OUTWARD_CODE)
        );
        assert_eq!(postcode_outward("Lancaster Rooms\r\nNew Wing"), None);
    }
}
