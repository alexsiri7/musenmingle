//! Royal Museums Greenwich (National Maritime Museum, Queen's House, Royal
//! Observatory, Cutty Sark, Prince Philip Maritime Collections Centre) —
//! reads the site's own "What's on" JSON feed.
//!
//! * robots.txt (checked 2026-09-27) answers 404, which `FetchContext`
//!   treats as "everything allowed".
//! * There is no JSON-LD `Event` on the listing or detail pages. The
//!   `/whats-on` page renders only carousels (a few cards per section); its
//!   script loads every item from `/whats-on-api`, a JSON array with the
//!   title, types ("Exhibitions", "Talks and tours", …), audience, location,
//!   `start_date`/`end_date` (ISO), `is_infinite`, the printed `times` line,
//!   price (empty on some free items, which set `free`), teaser description
//!   and the card's HTML (`renderedEvent`, read only for the image). One
//!   request per run; detail pages add nothing the feed lacks and are never
//!   fetched.
//! * Recurring programmes (`is_infinite`, whose dates are stale), courses,
//!   online events, members-only events (audience exactly "Members") and
//!   anything but an exhibition spanning more than one day are skipped.
//! * Exhibitions are `all_day` from `start_date` to `end_date`. Other items
//!   are read from `times`, "Thursday 1 October 2026 | 6.30pm" or
//!   "Wednesday 7 October | 1pm-1.45pm" (London wall-clock): its first part
//!   must be a single day, otherwise the item is a run of sessions ("Tuesday
//!   weekday evenings from …") and skipped. That printed day wins over
//!   `start_date`, which the feed sometimes leaves stale (Chuck Ragan: feed
//!   2026-12-12, page "Tuesday 26 January 2027"). The start time is the
//!   first later part that is a time ("6-8pm", "In conversation at 7pm"),
//!   a "Doors open at …" part only when no other has one; a day with no
//!   further parts is `all_day`.
//! * Category from the type: exhibition; talks and tours, conferences →
//!   talk; workshop; events and festivals → `map_category` over the title,
//!   else community for a festival, else skipped (concerts, evenings).
//!   Family fun and experiences are skipped.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::parse_time_range;
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_to_utc, map_category, parse_price, words,
};

pub const KEY: &str = "royal-museums-greenwich";
const FEED_PATH: &str = "/whats-on-api";

/// The feed's `location` → venue address. Other locations ("Online", "In
/// Greenwich") are skipped.
const VENUES: [(&str, &str); 5] = [
    (
        "National Maritime Museum",
        "Romney Road, Greenwich, London SE10 9NF",
    ),
    ("Queen's House", "Romney Road, Greenwich, London SE10 9NF"),
    (
        "Royal Observatory",
        "Blackheath Avenue, Greenwich, London SE10 8XJ",
    ),
    (
        "Cutty Sark",
        "King William Walk, Greenwich, London SE10 9HT",
    ),
    (
        "Prince Philip Maritime Collections Centre",
        "Nelson Mandela Road, Kidbrooke, London SE3 9QS",
    ),
];

pub struct RoyalMuseumsGreenwich {
    base_url: Url,
}

impl RoyalMuseumsGreenwich {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// One item of `/whats-on-api`, keeping only the fields used.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Item {
    pub title: String,
    #[serde(rename = "type", default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub audience: Vec<String>,
    pub location: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    #[serde(default)]
    pub is_infinite: bool,
    pub times: Option<String>,
    pub price: Option<String>,
    /// Set on free items, some of which leave `price` empty.
    #[serde(default)]
    pub free: bool,
    pub description: Option<String>,
    pub season: Option<String>,
    /// Site-relative page path.
    pub url: String,
    #[serde(rename = "renderedEvent", default, skip_serializing)]
    pub rendered_event: Option<String>,
}

/// The card image in an item's `renderedEvent` HTML, if any.
fn card_image(rendered: &str, base: &Url) -> Option<String> {
    let img = Selector::parse(".event-teaser__media img[src]").expect("valid selector");
    let doc = Html::parse_fragment(rendered);
    let src = doc.select(&img).next()?.value().attr("src")?;
    base.join(src).ok().map(String::from)
}

/// The feed items as [`RawEvent`]s (de-duplicated by page path), and the
/// items that could not be read, for the fetch to report.
pub fn raw_events(items: &[Value], base: &Url) -> (Vec<RawEvent>, Vec<String>) {
    let mut raws: Vec<RawEvent> = Vec::new();
    let mut problems = Vec::new();
    for value in items {
        let item = match Item::deserialize(value) {
            Ok(item) => item,
            Err(e) => {
                problems.push(format!("unreadable feed item: {e}"));
                continue;
            }
        };
        let page = match base.join(&item.url) {
            Ok(u) if u.host() == base.host() && u.path() != "/" => u,
            _ => {
                problems.push(format!(
                    "{:?}: unreadable page url {:?}",
                    item.title, item.url
                ));
                continue;
            }
        };
        let id = page.path().to_string();
        if raws.iter().any(|r| r.source_event_id == id) {
            continue;
        }
        let mut payload = serde_json::to_value(&item).expect("item serialises");
        payload["url"] = json!(page.as_str());
        payload["image_url"] = json!(
            item.rendered_event
                .as_deref()
                .and_then(|html| card_image(html, base))
        );
        raws.push(RawEvent {
            source_event_id: id,
            source_url: Some(page.to_string()),
            payload,
        });
    }
    (raws, problems)
}

/// The in-scope category for an item's types and title, or `None` to skip.
pub fn category<S: AsRef<str>>(types: &[S], title: &str) -> Option<Category> {
    let has = |want: &str| types.iter().any(|t| t.as_ref().eq_ignore_ascii_case(want));
    if has("Exhibitions") {
        Some(Category::Exhibition)
    } else if has("Workshops") {
        Some(Category::Workshop)
    } else if has("Talks and tours") || has("Conferences") {
        Some(Category::Talk)
    } else if has("Events and festivals") {
        map_category(&[title]).or_else(|| {
            words(title)
                .iter()
                .any(|w| w == "festival")
                .then_some(Category::Community)
        })
    } else {
        None
    }
}

/// A printed day, "Thursday 1 October 2026" or "Wednesday 7 October". A
/// day without a year is taken within half a year of `near` (the feed's
/// possibly stale `start_date`); `None` if its printed weekday matches no
/// such day.
pub fn parse_day(s: &str, near: NaiveDate) -> Option<NaiveDate> {
    const FORMATS: [&str; 2] = ["%A %d %B %Y", "%d %B %Y"];
    let s = clean_text(s);
    let parse = |text: &str| {
        FORMATS
            .iter()
            .find_map(|f| NaiveDate::parse_from_str(text, f).ok())
    };
    parse(&s).or_else(|| {
        (near.year() - 1..=near.year() + 1)
            .filter_map(|year| parse(&format!("{s} {year}")))
            .find(|day| (*day - near).num_days().abs() <= 183)
    })
}

/// A part of the `times` line that is a time: "6.30pm", "6-8pm",
/// "10.30am–12.30pm", "In conversation at 7pm".
pub fn parse_time(s: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    let s = clean_text(s);
    let clock = s.rsplit_once(" at ").map_or(s.as_str(), |(_, t)| t);
    parse_time_range(&clock.replace(' ', ""))
}

fn venue_address(location: &str) -> Option<&'static str> {
    VENUES
        .iter()
        .find(|(name, _)| *name == location)
        .map(|(_, address)| *address)
}

fn iso_day(payload: &Value, key: &str, title: &str) -> Result<NaiveDate, SourceError> {
    let s = payload.get(key).and_then(Value::as_str).unwrap_or_default();
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| SourceError::Parse(format!("{title:?}: bad {key} {s:?}")))
}

/// Normalise a Royal Museums Greenwich [`RawEvent`] payload (an [`Item`]
/// with the absolute `url` and the card's `image_url`).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let item: Item = serde_json::from_value(payload.clone())
        .map_err(|e| SourceError::Parse(format!("unreadable item: {e}")))?;
    let title = clean_text(&item.title);
    if title.is_empty() {
        return Err(SourceError::Parse("item without title".into()));
    }
    let members_only = item.audience.len() == 1 && item.audience[0] == "Members";
    if item.is_infinite || members_only {
        return Ok(None);
    }
    let Some(category) = category(&item.types, &title) else {
        return Ok(None);
    };
    let venue_name = item.location.as_deref().map(clean_text).unwrap_or_default();
    let Some(address) = venue_address(&venue_name) else {
        return Ok(None);
    };
    let first = iso_day(payload, "start_date", &title)?;
    let last = iso_day(payload, "end_date", &title)?;
    if last < first {
        return Err(SourceError::Parse(format!(
            "{title:?}: ends before it starts"
        )));
    }

    let (starts_at, ends_at, all_day) = if category == Category::Exhibition {
        (
            london_to_utc(first.and_time(NaiveTime::MIN)),
            (last > first).then(|| london_to_utc(last.and_time(NaiveTime::MIN))),
            true,
        )
    } else {
        if last != first {
            return Ok(None);
        }
        let times = item.times.as_deref().unwrap_or_default();
        let mut parts = times.split('|').map(clean_text).filter(|p| !p.is_empty());
        let Some(day) = parts.next().and_then(|p| parse_day(&p, first)) else {
            return Ok(None);
        };
        let rest: Vec<String> = parts.collect();
        if rest.is_empty() {
            (london_to_utc(day.and_time(NaiveTime::MIN)), None, true)
        } else {
            let (doors, others): (Vec<&String>, Vec<&String>) =
                rest.iter().partition(|p| p.starts_with("Doors"));
            let (start, end) = others
                .into_iter()
                .chain(doors)
                .find_map(|p| parse_time(p))
                .ok_or_else(|| SourceError::Parse(format!("{title:?}: no time in {times:?}")))?;
            (
                london_to_utc(day.and_time(start)),
                end.filter(|e| *e > start)
                    .map(|e| london_to_utc(day.and_time(e))),
                false,
            )
        }
    };

    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(&venue_name)),
        description: clean_description(item.description.as_deref()),
        title,
        address: Some(address.to_string()),
        venue_name: Some(venue_name),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        price: match item.price.as_deref().map(clean_text) {
            Some(p) if !p.is_empty() => parse_price(&p),
            _ if item.free => parse_price("Free"),
            _ => Price::default(),
        },
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: item
            .season
            .as_deref()
            .map(|s| clean_text(s).to_lowercase())
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect(),
    }))
}

#[async_trait]
impl Source for RoyalMuseumsGreenwich {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let feed = self
            .base_url
            .join(FEED_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let items: Vec<Value> = ctx.get_json(&feed).await?;
        let (raws, problems) = raw_events(&items, &self.base_url);
        for problem in problems {
            ctx.report_error(problem);
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

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn t(s: &str) -> NaiveTime {
        NaiveTime::parse_from_str(s, "%H:%M").unwrap()
    }

    fn item(types: &[&str], days: (&str, &str), times: &str) -> Value {
        json!({
            "title": "A Talk",
            "type": types,
            "audience": ["Adults"],
            "location": "National Maritime Museum",
            "start_date": days.0,
            "end_date": days.1,
            "is_infinite": false,
            "times": times,
            "url": "https://www.rmg.co.uk/whats-on/national-maritime-museum/a-talk",
        })
    }

    #[test]
    fn parses_days_and_times() {
        let near = d("2026-10-01");
        assert_eq!(
            parse_day("Thursday 1 October 2026", near),
            Some(d("2026-10-01"))
        );
        assert_eq!(
            parse_day("Wednesday 7 October", near),
            Some(d("2026-10-07"))
        );
        assert_eq!(parse_day("12 November", near), Some(d("2026-11-12")));
        assert_eq!(parse_day("Friday 1 October 2026", near), None);
        assert_eq!(parse_day("Friday 1 October", near), None);
        assert_eq!(parse_day("Saturdays from October 2026", near), None);
        assert_eq!(parse_time("6.30pm"), Some((t("18:30"), None)));
        assert_eq!(parse_time("6-8pm"), Some((t("18:00"), Some(t("20:00")))));
        assert_eq!(
            parse_time("10.30am–12.30pm"),
            Some((t("10:30"), Some(t("12:30"))))
        );
        assert_eq!(
            parse_time("Doors and bar open at 6pm"),
            Some((t("18:00"), None))
        );
        assert_eq!(parse_time("Selected times"), None);
    }

    #[test]
    fn maps_categories() {
        assert_eq!(category(&["Exhibitions"], "X"), Some(Category::Exhibition));
        assert_eq!(category(&["Talks and tours"], "X"), Some(Category::Talk));
        assert_eq!(category(&["Conferences"], "X"), Some(Category::Talk));
        assert_eq!(category(&["Workshops"], "X"), Some(Category::Workshop));
        let events = ["Events and festivals"];
        assert_eq!(
            category(&events, "In conversation with Luke Jerram"),
            Some(Category::Talk)
        );
        assert_eq!(
            category(&events, "Sea Shanty Festival 2026"),
            Some(Category::Community)
        );
        assert_eq!(category(&events, "Cutty Sark concert: Chuck Ragan"), None);
        assert_eq!(category(&["Family fun", "Experiences"], "X"), None);
        assert_eq!(category(&["Courses"], "Introduction to Astronomy"), None);
    }

    #[test]
    fn timed_talk() {
        let e = normalise_payload(&item(
            &["Talks and tours"],
            ("2026-10-09", "2026-10-09"),
            "Friday 9 October 2026 | 6.30-8pm",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-09T17:30:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-09T19:00:00+00:00");
        assert!(!e.all_day);
        assert_eq!(e.venue_name.as_deref(), Some("National Maritime Museum"));
    }

    #[test]
    fn doors_time_is_only_a_fallback() {
        let start = |times: &str| {
            let payload = item(&["Talks and tours"], ("2026-10-22", "2026-10-22"), times);
            normalise_payload(&payload)
                .unwrap()
                .unwrap()
                .starts_at
                .to_rfc3339()
        };
        assert_eq!(
            start("Thursday 22 October 2026 | Doors open at 6.30pm | In conversation at 7pm"),
            "2026-10-22T18:00:00+00:00"
        );
        assert_eq!(
            start("Thursday 22 October 2026 | Doors open at 6.30pm"),
            "2026-10-22T17:30:00+00:00"
        );
    }

    #[test]
    fn free_flag_stands_in_for_an_empty_price() {
        let mut payload = item(
            &["Talks and tours"],
            ("2026-10-02", "2026-10-02"),
            "Friday 2 October 2026 | 6-8pm",
        );
        payload["price"] = json!("");
        let price = |p: &Value| normalise_payload(p).unwrap().unwrap().price;
        assert!(!price(&payload).is_free);
        payload["free"] = json!(true);
        assert!(price(&payload).is_free);
    }

    #[test]
    fn printed_day_wins_over_a_stale_start_date() {
        let e = normalise_payload(&item(
            &["Talks and tours"],
            ("2026-12-12", "2026-12-12"),
            "Tuesday 26 January 2027 | 7-10pm",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2027-01-26T19:00:00+00:00");
    }

    #[test]
    fn yearless_day_takes_the_year_nearest_a_stale_start_date() {
        let start = |days: (&str, &str), times: &str| {
            normalise_payload(&item(&["Talks and tours"], days, times))
                .unwrap()
                .map(|e| e.starts_at.to_rfc3339())
        };
        let stale = ("2026-12-28", "2026-12-28");
        assert_eq!(
            start(stale, "Tuesday 5 January | 7pm").as_deref(),
            Some("2027-01-05T19:00:00+00:00")
        );
        assert_eq!(
            start(stale, "5 January | 7pm").as_deref(),
            Some("2027-01-05T19:00:00+00:00")
        );
        assert_eq!(
            start(("2027-01-04", "2027-01-04"), "Monday 28 December | 7pm").as_deref(),
            Some("2026-12-28T19:00:00+00:00")
        );
    }

    #[test]
    fn exhibition_is_all_day_over_its_run() {
        let e = normalise_payload(&item(
            &["Exhibitions"],
            ("2025-11-07", "2026-10-18"),
            "Open daily until 18 October 2026 | 10am-5pm",
        ))
        .unwrap()
        .unwrap();
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2025-11-07T00:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-17T23:00:00+00:00");
    }

    #[test]
    fn day_without_time_is_all_day() {
        let e = normalise_payload(&item(
            &["Workshops"],
            ("2026-10-30", "2026-10-30"),
            "Friday 30 October 2026",
        ))
        .unwrap()
        .unwrap();
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-30T00:00:00+00:00");
        assert_eq!(e.ends_at, None);
    }

    #[test]
    fn skips_out_of_scope_items() {
        let day = ("2026-10-06", "2026-10-06");
        let times = "Tuesday 6 October 2026 | 12-1pm";
        let mut recurring = item(&["Talks and tours"], day, times);
        recurring["is_infinite"] = json!(true);
        let mut members = item(&["Talks and tours"], day, times);
        members["audience"] = json!(["Members"]);
        let mut online = item(&["Talks and tours"], day, times);
        online["location"] = json!("Online");
        let multi_day = item(
            &["Talks and tours"],
            ("2026-10-30", "2026-11-01"),
            "Friday 30 October and Sunday 1 November 2026 | 5.30-7pm",
        );
        let series = item(
            &["Workshops"],
            ("2026-11-03", "2026-11-03"),
            "Tuesday weekday evenings from 3 November 2026 to 23 February 2027 | 5-7pm",
        );
        let family = item(&["Family fun"], day, times);
        for payload in [recurring, members, online, multi_day, series, family] {
            assert_eq!(normalise_payload(&payload).unwrap(), None, "{payload}");
        }
    }

    #[test]
    fn unreadable_dates_and_times_are_errors() {
        for payload in [
            item(&["Talks and tours"], ("soon", "2026-10-06"), ""),
            item(&["Exhibitions"], ("2026-10-06", "2026-10-01"), ""),
            item(
                &["Talks and tours"],
                ("2026-10-06", "2026-10-06"),
                "Tuesday 6 October 2026 | Selected times",
            ),
        ] {
            assert!(normalise_payload(&payload).is_err(), "{payload}");
        }
    }

    #[test]
    fn raw_events_dedupe_and_read_the_card_image() {
        let base: Url = "https://www.rmg.co.uk".parse().unwrap();
        let card = r#"<div class="event-teaser"><div class="event-teaser__media">
            <img src="/sites/default/files/a.jpg.webp?itok=x" /></div></div>"#;
        let items = [
            json!({"title": "A", "url": "/whats-on/cutty-sark/a", "renderedEvent": card}),
            json!({"title": "A again", "url": "/whats-on/cutty-sark/a"}),
            json!({"title": "Moved", "url": "/cutty-sark/attractions/b"}),
            json!({"title": "Elsewhere", "url": "https://example.com/x"}),
            json!({"title": "No url"}),
            json!({"title": "Null type", "url": "/whats-on/c", "type": null}),
        ];
        let (raws, problems) = raw_events(&items, &base);
        let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
        assert_eq!(ids, ["/whats-on/cutty-sark/a", "/cutty-sark/attractions/b"]);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert_eq!(raws[0].source_event_id, "/whats-on/cutty-sark/a");
        assert_eq!(
            raws[0].payload["image_url"],
            "https://www.rmg.co.uk/sites/default/files/a.jpg.webp?itok=x"
        );
        assert!(raws[0].payload.get("renderedEvent").is_none());
    }
}
