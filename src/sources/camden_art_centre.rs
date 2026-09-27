//! Camden Art Centre (Arkwright Road, NW3) — exhibitions and courses from
//! the site's own programme feed plus each event's page.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   disallows only `/cms/wp-admin/`.
//! * `/whats-on/in-the-building` is a Vue shell with no event links in the
//!   HTML; the page fills itself from the site's JSON feed
//!   `/api/programmes?format=in-the-building&page=N` (9 items a page, with
//!   `meta.last_page`). The feed gives the title, the artist, ISO
//!   `start_date`/`end_date` (no times), the programme type, the permalink
//!   and an image. We read up to [`MAX_FEED_PAGES`] pages, building the page
//!   URLs ourselves (the feed's own links are absolute).
//! * For in-scope items we then fetch the event page (at most
//!   [`MAX_DETAIL_PAGES`] a run) for what the feed lacks, from the sidebar:
//!   the time ("27 Sep 2026" / "13:30-17:30", London wall clock), the price
//!   ("£59", "Free") and the description (JSON-LD `Event`/`Course`
//!   `description`, else the intro heading). The JSON-LD `Course` offers on
//!   workshop pages say `"price": 0` / "Free" for paid workshops, so the
//!   sidebar price is used, never the JSON-LD one.
//! * Dates come from the feed's ISO fields; the sidebar's first day must
//!   agree (sanity check). A single-day item with a time is timed; anything
//!   else is all-day (London midnights, `ends_at` the last day, none for a
//!   single day or an open "From" run). Runs longer than [`MAX_RANGE_DAYS`]
//!   are skipped.
//! * Categories by programme type: `exhibitions` → exhibition,
//!   `courses-workshops` → workshop, talks/events → talk. Skipped: studio
//!   `residencies` (not something the public attends), and `young-people`
//!   and `childrenandfamilies` programmes, which run as sessions on a few
//!   days across months ("Saturdays: 10 Oct, 24 Oct, …") and would read as
//!   open every day if stored as one range. Skipped items are not fetched.
//! * Everything listed here is at the Centre; `/whats-on/offsite` and
//!   `/whats-on/on-demand` are not read.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::jsonld::str_or_name;
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, parse_price};

pub const KEY: &str = "camden-art-centre";
/// Upper bound on feed pages read per run (9 items a page).
pub const MAX_FEED_PAGES: u64 = 5;
/// Upper bound on event pages fetched per run (≈ 40 s at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 20;
/// Longer runs are ongoing programmes, not exhibitions.
pub const MAX_RANGE_DAYS: i64 = 366;
const FEED_PATH: &str = "/api/programmes";
const FORMAT: &str = "in-the-building";
const EVENT_PREFIX: &str = "/whats-on/";
const VENUE_NAME: &str = "Camden Art Centre";
const VENUE_ADDRESS: &str = "Arkwright Road, London NW3 6DG";

pub struct CamdenArtCentre {
    base_url: Url,
}

impl CamdenArtCentre {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }

    fn feed_url(&self, page: u64) -> Result<Url, SourceError> {
        let mut url = self
            .base_url
            .join(FEED_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("format", FORMAT)
            .append_pair("page", &page.to_string());
        Ok(url)
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// One page of the programme feed: its items and `meta.last_page`.
pub fn parse_feed(feed: &Value) -> Result<(Vec<Value>, u64), SourceError> {
    let items = feed
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::Parse("programme feed without data".into()))?;
    let last_page = feed
        .pointer("/meta/last_page")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    Ok((items.clone(), last_page))
}

/// The event page path (`/whats-on/<slug>`) of a feed item, from its
/// permalink; only the path is kept so callers join it to their base URL.
pub fn item_path(item: &Value) -> Option<String> {
    let link = item.get("permalink").and_then(Value::as_str)?;
    let url = Url::parse("https://camdenartcentre.org/")
        .ok()?
        .join(link)
        .ok()?;
    let slug = url.path().strip_prefix(EVENT_PREFIX)?.trim_end_matches('/');
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then(|| format!("{EVENT_PREFIX}{slug}"))
}

fn type_slug(item: &Value) -> &str {
    item.pointer("/type/slug")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Category for a programme type slug; `None` means out of scope.
pub fn category(type_slug: &str) -> Option<Category> {
    match type_slug {
        "exhibitions" => Some(Category::Exhibition),
        "courses-workshops" => Some(Category::Workshop),
        s if s.contains("talk") || s == "events" => Some(Category::Talk),
        _ => None,
    }
}

/// What the event page adds to the feed: the sidebar's `Date` and `Price`
/// rows and a description.
pub fn parse_detail(html: &str) -> Value {
    let doc = Html::parse_document(html);
    let item_sel = selector(".sidebar__item");
    let mut date_text = None;
    let mut price_text = None;
    for row in doc.select(&selector(".main-content__sidebar .sidebar__row")) {
        let mut cells = row.select(&item_sel);
        let (Some(label), Some(value)) = (cells.next(), cells.next()) else {
            continue;
        };
        match element_text(label).as_str() {
            "Date" => date_text = Some(element_text(value)),
            "Price" => price_text = Some(element_text(value)),
            _ => {}
        }
    }
    let script_sel = selector(r#"script[type="application/ld+json"]"#);
    let jsonld_description = doc
        .select(&script_sel)
        .filter_map(|s| serde_json::from_str::<Value>(s.text().collect::<String>().trim()).ok())
        .find_map(|v| str_or_name(&v, "description").map(str::to_string));
    let description = jsonld_description.or_else(|| {
        doc.select(&selector(".main__intro__title"))
            .next()
            .map(element_text)
    });
    json!({
        "date_text": date_text,
        "price_text": price_text,
        "description": description,
    })
}

/// The stored payload for a feed item; `detail` is [`parse_detail`]'s
/// output, or `None` for skipped items (their pages are not fetched).
pub fn raw_event(item: &Value, path: &str, site: &Url, detail: Option<Value>) -> RawEvent {
    let url = site
        .join(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| path.to_string());
    RawEvent {
        source_event_id: path.to_string(),
        source_url: Some(url.clone()),
        payload: json!({ "url": url, "item": item, "detail": detail }),
    }
}

/// `13:30-17:30` (or with an en dash / spaces) anywhere in the sidebar date.
pub fn parse_time_range(text: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    let squashed: String = text
        .replace(['–', '—'], "-")
        .replace(" - ", "-")
        .replace(" -", "-")
        .replace("- ", "-");
    squashed.split_whitespace().find_map(|tok| {
        let mut parts = tok.splitn(2, '-');
        let start = NaiveTime::parse_from_str(parts.next()?, "%H:%M").ok()?;
        let end = parts
            .next()
            .and_then(|e| NaiveTime::parse_from_str(e, "%H:%M").ok());
        Some((start, end))
    })
}

fn london_midnight(d: NaiveDate) -> DateTime<Utc> {
    london_to_utc(d.and_time(NaiveTime::MIN))
}

fn iso_date(item: &Value, key: &str) -> Option<NaiveDate> {
    item.get(key)
        .and_then(Value::as_str)
        .and_then(|s| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
}

/// Normalise a payload built by [`raw_event`].
pub fn normalise_payload(p: &Value) -> Result<Option<NewEvent>, SourceError> {
    let item = &p["item"];
    let Some(category) = category(type_slug(item)) else {
        return Ok(None);
    };
    if item.get("always_online").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    let name = item
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("programme item without a title".into()))?;
    // Exhibitions headed by the artist ("Liz Larner" over "A hard line to
    // bend") keep the artist in the title.
    let artist = item
        .get("artist_names")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|a| !a.is_empty() && !name.contains(a.as_str()));
    let title = match (&artist, item.get("enable_artist_as_title")) {
        (Some(a), Some(Value::Bool(true))) => format!("{a}: {name}"),
        _ => name,
    };
    let start = iso_date(item, "start_date")
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no start_date")))?;
    let end = iso_date(item, "end_date").filter(|e| *e > start);
    if end.is_some_and(|e| (e - start).num_days() > MAX_RANGE_DAYS) {
        return Ok(None);
    }
    let detail = &p["detail"];
    let date_text = detail["date_text"].as_str().unwrap_or_default();
    // Sanity check: the page's first day must be the feed's start day.
    if let Some(day) = date_text
        .split(|c: char| !c.is_ascii_digit())
        .find(|t| !t.is_empty())
        && day.parse::<u32>().ok() != Some(chrono::Datelike::day(&start))
    {
        return Err(SourceError::Parse(format!(
            "{title:?}: page date {date_text:?} disagrees with start_date {start}"
        )));
    }
    let (starts_at, ends_at, all_day) = match (end, parse_time_range(date_text)) {
        (None, Some((from, to))) => {
            let s = london_to_utc(start.and_time(from));
            let e = to
                .map(|t| london_to_utc(start.and_time(t)))
                .filter(|e| *e > s);
            (s, e, false)
        }
        _ => (london_midnight(start), end.map(london_midnight), true),
    };
    let price = detail["price_text"]
        .as_str()
        .map(parse_price)
        .unwrap_or_default();
    let tags: Vec<String> = item["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str().or_else(|| t.get("name").and_then(Value::as_str)))
        .map(|t| clean_text(t).to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        title,
        description: clean_description(detail["description"].as_str()),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        price,
        url: p["url"].as_str().map(str::to_string),
        image_url: item
            .get("image")
            .and_then(Value::as_str)
            .map(str::to_string),
        category,
        tags,
    }))
}

#[async_trait]
impl Source for CamdenArtCentre {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let mut items: Vec<Value> = Vec::new();
        let mut page = 1;
        loop {
            let feed: Value = ctx.get_json(&self.feed_url(page)?).await?;
            let (mut found, last_page) = parse_feed(&feed)?;
            items.append(&mut found);
            if page >= last_page.min(MAX_FEED_PAGES) {
                break;
            }
            page += 1;
        }
        if items.is_empty() {
            return Err(SourceError::Parse("no items in the programme feed".into()));
        }
        let mut out: Vec<RawEvent> = Vec::new();
        let mut fetched = 0;
        for item in items {
            let Some(path) = item_path(&item) else {
                ctx.report_error(format!(
                    "programme item without an event link: {:?}",
                    item.get("title")
                ));
                continue;
            };
            if out.iter().any(|r| r.source_event_id == path) {
                continue;
            }
            if category(type_slug(&item)).is_none() {
                out.push(raw_event(&item, &path, &self.base_url, None));
                continue;
            }
            if fetched >= MAX_DETAIL_PAGES {
                continue;
            }
            fetched += 1;
            let url = self
                .base_url
                .join(&path)
                .map_err(|e| SourceError::Config(e.to_string()))?;
            match ctx.get_text(&url).await {
                Ok(html) => out.push(raw_event(
                    &item,
                    &path,
                    &self.base_url,
                    Some(parse_detail(&html)),
                )),
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

    fn payload(item: Value, date_text: &str) -> Value {
        json!({
            "url": "https://camdenartcentre.org/whats-on/x",
            "item": item,
            "detail": {"date_text": date_text, "price_text": "Free", "description": null},
        })
    }

    #[test]
    fn categories() {
        assert_eq!(category("exhibitions"), Some(Category::Exhibition));
        assert_eq!(category("courses-workshops"), Some(Category::Workshop));
        assert_eq!(category("talks"), Some(Category::Talk));
        assert_eq!(category("residencies"), None);
        assert_eq!(category("young-people"), None);
        assert_eq!(category("childrenandfamilies"), None);
        assert_eq!(category(""), None);
    }

    #[test]
    fn time_ranges() {
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert_eq!(
            parse_time_range("27 Sep 2026 13:30-17:30"),
            Some((t(13, 30), Some(t(17, 30))))
        );
        assert_eq!(
            parse_time_range("5 Dec 2026 11:30 – 13:45"),
            Some((t(11, 30), Some(t(13, 45))))
        );
        assert_eq!(parse_time_range("6 Dec 2026 14:00"), Some((t(14, 0), None)));
        assert_eq!(parse_time_range("11 Sep 2026/17 Jan 2027"), None);
    }

    #[test]
    fn open_ended_from_date_is_all_day_without_end() {
        let item = json!({"title": "Show", "type": {"slug": "exhibitions"},
            "start_date": "2026-10-01", "end_date": null});
        let ev = normalise_payload(&payload(item, "From 1 Oct 2026"))
            .unwrap()
            .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-30T23:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn page_date_must_agree_with_the_feed() {
        let item = json!({"title": "Class", "type": {"slug": "courses-workshops"},
            "start_date": "2026-10-17", "end_date": "2026-10-17"});
        let err = normalise_payload(&payload(item, "18 Oct 2026 13:00-17:00")).unwrap_err();
        assert!(err.to_string().contains("disagrees"), "{err}");
    }

    #[test]
    fn open_ended_programmes_are_skipped() {
        let item = json!({"title": "Forever", "type": {"slug": "exhibitions"},
            "start_date": "2026-01-01", "end_date": "2028-01-01"});
        assert_eq!(normalise_payload(&payload(item, "")).unwrap(), None);
    }
}
