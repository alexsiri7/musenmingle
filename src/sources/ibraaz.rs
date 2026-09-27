//! Ibraaz (93 Mortimer Street, Fitzrovia, W1) — reads the Nuxt payload of
//! the "What's On" page and of each event's page.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   with an empty `Disallow`.
//! * There is no `Event` JSON-LD (only the Organization/WebPage/WebSite
//!   graph), but the server-rendered pages embed their data as a Nuxt
//!   payload (`script#__NUXT_DATA__`, devalue-encoded JSON: a flat array
//!   whose objects and arrays hold indices into it). `/whats-on/` carries
//!   every current and forthcoming event (`solspace_calendar.events`): id,
//!   slug, title, `startDate`/`endDate`, categories, room and the printed
//!   date line (`datesText`). The sitemap lists no event pages, so this is
//!   the only index. Each `/whats-on/<slug>` page's payload adds the
//!   description (`contentSections`); at most [`MAX_DETAIL_PAGES`] a run,
//!   and an event whose page can't be read keeps its facts without one.
//! * `startDate`/`endDate` are London wall-clock with a bogus `+00:00`
//!   ("Thu 1 Oct, 6–8pm" is `2026-10-01T18:00:00+00:00`). A span over more
//!   than one day, or a whole day (00:00 to 23:59:59), is `all_day`; the
//!   page then prints dates only ("8 Jul – 22 Nov 2026", even where the
//!   payload has opening hours). Otherwise the time comes from the printed
//!   line ("Sat 3 Oct, 3–4.30pm"), which wins over the payload's end (17:00
//!   for that talk); its start must match the payload's. Anything but an
//!   exhibition spanning more than one day is a run of sessions and skipped.
//! * Category from the site's: talks → talk; workshops → workshop;
//!   exhibitions and library-in-residence (a residency installed in the
//!   library, open for months) → exhibition, in that order. Music,
//!   performance, film and special projects alone are skipped.

use async_trait::async_trait;
use chrono::{NaiveDateTime, NaiveTime};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::parse_time_range;
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc};

pub const KEY: &str = "ibraaz";
/// Upper bound on event pages fetched per run (≈ 60 s at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 30;
const LISTING_PATH: &str = "/whats-on/";
const IMAGE_BASE: &str = "https://ibraaz-website.imgix.net/";
const VENUE_NAME: &str = "Ibraaz";
const VENUE_ADDRESS: &str = "93 Mortimer Street, London W1W 7SS";
/// Site category slugs → category, in order of precedence.
const CATEGORIES: [(&str, Category); 4] = [
    ("talks", Category::Talk),
    ("workshops", Category::Workshop),
    ("exhibitions", Category::Exhibition),
    ("library-in-residence", Category::Exhibition),
];

pub struct Ibraaz {
    base_url: Url,
    max_detail_pages: usize,
}

impl Ibraaz {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            max_detail_pages: MAX_DETAIL_PAGES,
        }
    }

    pub fn with_max_detail_pages(mut self, n: usize) -> Self {
        self.max_detail_pages = n;
        self
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SiteCategory {
    pub slug: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RichText {
    pub html: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Image {
    pub path: String,
}

/// One event of the listing's `solspace_calendar.events`, keeping only the
/// fields used.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u64,
    pub slug: String,
    pub title: String,
    pub heading: Option<String>,
    pub start_date: String,
    pub end_date: Option<String>,
    pub location: Option<String>,
    #[serde(default)]
    pub categories: Vec<SiteCategory>,
    pub dates_text: Option<RichText>,
    #[serde(default, skip_serializing)]
    pub image: Vec<Image>,
}

/// Decode a devalue array (Nuxt's payload format) from its root. Objects
/// and arrays hold indices into `values`; negative indices are `undefined`
/// and friends; an array starting with a string is a tagged value, of which
/// only Vue's reactive wrappers matter here (others become null, as do
/// cyclic references).
fn devalue(values: &[Value], index: &Value, path: &mut Vec<usize>) -> Value {
    let Some(i) = index.as_u64().map(|i| i as usize) else {
        return Value::Null;
    };
    let Some(value) = values.get(i) else {
        return Value::Null;
    };
    if path.contains(&i) {
        return Value::Null;
    }
    path.push(i);
    let decoded = match value {
        Value::Array(items) => match items.first() {
            Some(Value::String(tag)) => match tag.as_str() {
                "Reactive" | "ShallowReactive" | "Ref" | "ShallowRef" => {
                    devalue(values, items.get(1).unwrap_or(&Value::Null), path)
                }
                _ => Value::Null,
            },
            _ => Value::Array(items.iter().map(|x| devalue(values, x, path)).collect()),
        },
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| (k.clone(), devalue(values, x, path)))
                .collect(),
        ),
        literal => literal.clone(),
    };
    path.pop();
    decoded
}

/// A page's decoded Nuxt payload.
pub fn nuxt_payload(html: &str) -> Result<Value, String> {
    let script = Selector::parse("script#__NUXT_DATA__").expect("valid selector");
    let doc = Html::parse_document(html);
    let text: String = doc
        .select(&script)
        .next()
        .ok_or("no Nuxt payload")?
        .text()
        .collect();
    let values: Vec<Value> =
        serde_json::from_str(&text).map_err(|e| format!("unreadable Nuxt payload: {e}"))?;
    Ok(devalue(&values, &json!(0), &mut Vec::new()))
}

fn find_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|v| find_key(v, key))),
        Value::Array(items) => items.iter().find_map(|v| find_key(v, key)),
        _ => None,
    }
}

/// The calendar events in a page's payload.
fn calendar_events(html: &str) -> Result<Vec<Value>, String> {
    let payload = nuxt_payload(html)?;
    find_key(&payload, "solspace_calendar")
        .and_then(|c| c.get("events"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "no calendar events in the Nuxt payload".into())
}

fn page_url(base: &Url, slug: &str) -> Result<Url, url::ParseError> {
    base.join(&format!("{LISTING_PATH}{slug}"))
}

/// The listing's events as [`RawEvent`]s (without descriptions), and the
/// events that could not be read, for the fetch to report. An error means
/// the page no longer carries the calendar at all.
pub fn parse_listing(html: &str, base: &Url) -> Result<(Vec<RawEvent>, Vec<String>), String> {
    let mut raws = Vec::new();
    let mut problems = Vec::new();
    for value in calendar_events(html)? {
        let item = match Item::deserialize(&value) {
            Ok(item) => item,
            Err(e) => {
                problems.push(format!("unreadable calendar event: {e}"));
                continue;
            }
        };
        let page = match page_url(base, &item.slug) {
            Ok(u) => u,
            Err(e) => {
                problems.push(format!("{:?}: bad slug {:?}: {e}", item.title, item.slug));
                continue;
            }
        };
        let mut payload = serde_json::to_value(&item).expect("item serialises");
        payload["url"] = json!(page.as_str());
        payload["image_url"] = json!(
            item.image
                .first()
                .map(|i| format!("{IMAGE_BASE}{}", i.path))
        );
        raws.push(RawEvent {
            source_event_id: item.id.to_string(),
            source_url: Some(page.to_string()),
            payload,
        });
    }
    Ok((raws, problems))
}

/// The description (HTML of its content sections) of event `slug` on its
/// page, `None` if it has none.
pub fn parse_detail(html: &str, slug: &str) -> Result<Option<String>, String> {
    let event = calendar_events(html)?
        .into_iter()
        .find(|e| e.get("slug").and_then(Value::as_str) == Some(slug))
        .ok_or_else(|| format!("no event {slug:?} in the Nuxt payload"))?;
    let sections: Vec<&str> = event
        .get("contentSections")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| s.get("typeHandle").and_then(Value::as_str) == Some("content"))
        .filter_map(|s| s.pointer("/richText/html").and_then(Value::as_str))
        .collect();
    Ok((!sections.is_empty()).then(|| sections.join("\n")))
}

pub fn category(categories: &[SiteCategory]) -> Option<Category> {
    CATEGORIES
        .iter()
        .find(|(slug, _)| categories.iter().any(|c| c.slug == *slug))
        .map(|(_, category)| *category)
}

/// A payload date-time's wall-clock reading; its offset is bogus.
fn wall_clock(s: &str, title: &str) -> Result<NaiveDateTime, SourceError> {
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .map(|t| t.naive_local())
        .map_err(|_| SourceError::Parse(format!("{title:?}: bad date {s:?}")))
}

/// Normalise an Ibraaz [`RawEvent`] payload (an [`Item`] with the absolute
/// `url`, `image_url` and the page's `description`).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let item: Item = serde_json::from_value(payload.clone())
        .map_err(|e| SourceError::Parse(format!("unreadable item: {e}")))?;
    let title = clean_text(&item.title);
    if title.is_empty() {
        return Err(SourceError::Parse("item without title".into()));
    }
    let Some(category) = category(&item.categories) else {
        return Ok(None);
    };
    let start = wall_clock(&item.start_date, &title)?;
    let end = match item.end_date.as_deref() {
        Some(s) => wall_clock(s, &title)?,
        None => start,
    };
    if end < start {
        return Err(SourceError::Parse(format!(
            "{title:?}: ends before it starts"
        )));
    }
    let (first, last) = (start.date(), end.date());
    if last > first && category != Category::Exhibition {
        return Ok(None);
    }
    let whole_day = start.time() == NaiveTime::MIN
        && end.time() == NaiveTime::from_hms_opt(23, 59, 59).unwrap();

    let (starts_at, ends_at, all_day) = if last > first || whole_day {
        (
            london_to_utc(first.and_time(NaiveTime::MIN)),
            (last > first).then(|| london_to_utc(last.and_time(NaiveTime::MIN))),
            true,
        )
    } else {
        let printed = item
            .dates_text
            .as_ref()
            .map(|t| clean_text(&t.html))
            .unwrap_or_default();
        let (from, to) = printed
            .rsplit_once(',')
            .and_then(|(_, time)| parse_time_range(&time.replace(' ', "")))
            .ok_or_else(|| SourceError::Parse(format!("{title:?}: no time in {printed:?}")))?;
        if from != start.time() {
            return Err(SourceError::Parse(format!(
                "{title:?}: printed time {printed:?} doesn't match {}",
                item.start_date
            )));
        }
        (
            london_to_utc(first.and_time(from)),
            to.filter(|t| *t > from)
                .map(|t| london_to_utc(first.and_time(t))),
            false,
        )
    };

    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(text("description")),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        price: Price::default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: item
            .categories
            .iter()
            .map(|c| clean_text(&c.title).to_lowercase())
            .collect(),
    }))
}

#[async_trait]
impl Source for Ibraaz {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listing_url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let html = ctx.get_text(&listing_url).await?;
        let (mut raws, problems) = parse_listing(&html, &self.base_url)
            .map_err(|e| SourceError::Parse(format!("{LISTING_PATH}: {e}")))?;
        for problem in problems {
            ctx.report_error(problem);
        }
        for raw in raws.iter_mut().take(self.max_detail_pages) {
            let slug = raw.payload["slug"].as_str().unwrap_or_default().to_string();
            let page =
                page_url(&self.base_url, &slug).map_err(|e| SourceError::Config(e.to_string()))?;
            let description = match ctx.get_text(&page).await {
                Ok(html) => parse_detail(&html, &slug),
                Err(e) => Err(e.to_string()),
            };
            match description {
                Ok(d) => raw.payload["description"] = json!(d),
                Err(e) => ctx.report_error(format!("{}: {e}", page.path())),
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

    fn item(categories: &[&str], start: &str, end: &str, printed: &str) -> Value {
        json!({
            "id": 1,
            "slug": "a-talk",
            "title": "A Talk",
            "heading": null,
            "startDate": start,
            "endDate": end,
            "location": "Majlis",
            "categories": categories
                .iter()
                .map(|slug| json!({"slug": slug, "title": slug}))
                .collect::<Vec<_>>(),
            "datesText": {"html": format!("<p>{printed}</p>")},
            "url": "https://ibraaz.org/whats-on/a-talk",
        })
    }

    fn normalised(payload: &Value) -> NewEvent {
        normalise_payload(payload).unwrap().unwrap()
    }

    #[test]
    fn decodes_devalue_references() {
        let html = r#"<script type="application/json" id="__NUXT_DATA__">
            [["ShallowReactive",1],{"data":2,"n":-1,"loop":0},{"a":3,"b":5,"c":6},"x",29493,[4,4],["Set"]]
            </script>"#;
        assert_eq!(
            nuxt_payload(html).unwrap(),
            json!({"data": {"a": "x", "b": [29493, 29493], "c": null}, "n": null, "loop": null})
        );
        assert!(nuxt_payload("<html></html>").is_err());
    }

    #[test]
    fn payload_times_are_london_wall_clock() {
        let bst = normalised(&item(
            &["talks"],
            "2026-10-01T18:00:00+00:00",
            "2026-10-01T20:00:00+00:00",
            "Thu 1 Oct, 6–8pm",
        ));
        assert_eq!(bst.starts_at.to_rfc3339(), "2026-10-01T17:00:00+00:00");
        assert_eq!(
            bst.ends_at.unwrap().to_rfc3339(),
            "2026-10-01T19:00:00+00:00"
        );
        assert!(!bst.all_day);
        let gmt = normalised(&item(
            &["talks"],
            "2026-11-04T18:00:00+00:00",
            "2026-11-04T20:00:00+00:00",
            "Wed 4 Nov, 6–8pm",
        ));
        assert_eq!(gmt.starts_at.to_rfc3339(), "2026-11-04T18:00:00+00:00");
    }

    #[test]
    fn printed_end_wins_but_the_start_must_agree() {
        let e = normalised(&item(
            &["talks"],
            "2026-10-03T15:00:00+00:00",
            "2026-10-03T17:00:00+00:00",
            "Sat 3 Oct, 3–4.30pm",
        ));
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-03T15:30:00+00:00");
        for printed in ["Sat 3 Oct, 4–5pm", "Sat 3 Oct", ""] {
            let payload = item(
                &["talks"],
                "2026-10-03T15:00:00+00:00",
                "2026-10-03T17:00:00+00:00",
                printed,
            );
            assert!(normalise_payload(&payload).is_err(), "{printed:?}");
        }
    }

    #[test]
    fn multi_day_and_whole_day_spans_are_all_day() {
        let run = normalised(&item(
            &["exhibitions"],
            "2026-07-08T11:00:00+00:00",
            "2026-11-22T18:00:00+00:00",
            "8 Jul – 22 Nov 2026",
        ));
        assert!(run.all_day);
        assert_eq!(run.starts_at.to_rfc3339(), "2026-07-07T23:00:00+00:00");
        assert_eq!(
            run.ends_at.unwrap().to_rfc3339(),
            "2026-11-22T00:00:00+00:00"
        );
        let day = normalised(&item(
            &["workshops"],
            "2026-10-10T00:00:00+00:00",
            "2026-10-10T23:59:59+00:00",
            "Sat 10 Oct",
        ));
        assert!(day.all_day);
        assert_eq!(day.starts_at.to_rfc3339(), "2026-10-09T23:00:00+00:00");
        assert_eq!(day.ends_at, None);
    }

    #[test]
    fn multi_day_non_exhibitions_are_skipped() {
        let payload = item(
            &["workshops"],
            "2026-10-10T11:00:00+00:00",
            "2026-10-11T16:00:00+00:00",
            "Sat 10 – Sun 11 Oct, 11am–4pm",
        );
        assert_eq!(normalise_payload(&payload).unwrap(), None);
    }

    #[test]
    fn maps_categories_in_order() {
        let cats = |slugs: &[&str]| {
            let payload = item(
                slugs,
                "2026-10-08T18:00:00+00:00",
                "2026-10-08T20:00:00+00:00",
                "Thu 8 Oct, 6–8pm",
            );
            normalise_payload(&payload).unwrap().map(|e| e.category)
        };
        assert_eq!(cats(&["music", "talks"]), Some(Category::Talk));
        assert_eq!(cats(&["workshops"]), Some(Category::Workshop));
        assert_eq!(
            cats(&["exhibitions", "library-in-residence"]),
            Some(Category::Exhibition)
        );
        assert_eq!(cats(&["library-in-residence"]), Some(Category::Exhibition));
        for skipped in ["music", "performance", "film", "special-projects"] {
            assert_eq!(cats(&[skipped]), None, "{skipped}");
        }
    }

    #[test]
    fn unreadable_dates_are_errors() {
        for (start, end) in [
            ("soon", "2026-10-03T17:00:00+00:00"),
            ("2026-10-03T15:00:00+00:00", "2026-10-02T17:00:00+00:00"),
        ] {
            let payload = item(&["talks"], start, end, "Sat 3 Oct, 3–5pm");
            assert!(normalise_payload(&payload).is_err(), "{start} {end}");
        }
    }
}
