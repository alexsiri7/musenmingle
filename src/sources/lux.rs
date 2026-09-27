//! LUX (artists' moving image, Waterlow Park, N19) — CSS scraper over the
//! "What's on" page and the detail pages of its upcoming events.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   disallows only `/wp-admin/`.
//! * There is no `Event` JSON-LD (only Yoast's Organization/WebPage graph).
//!   `/whats-on/` is an Elementor page with two loop grids of the same card
//!   template: "Upcoming and Current Events" (unpaginated) and the paginated
//!   "Event Archive" (`#event-listings`). Only the first is read. A card has
//!   the event's taxonomy as `event_catogories-<slug>` classes (the site's
//!   spelling), the title (`h5`, sometimes a `<br>`-separated subtitle, kept
//!   as "Title: Subtitle"), then text widgets in order: location ("LUX",
//!   "Online (Zoom or Youtube)", a partner venue), first day ("6 November,
//!   2026"), last day ("– 6 November, 2026") and a price line.
//! * Only the detail page (`/event/<slug>/`) has the time ("6pm – 7:30pm",
//!   "9am-5:30pm", "5-6pm BST", an exhibition's "Thursday – Saturday / 12pm
//!   – 4pm") as the third text widget beside the `h1`, and the description.
//!   Detail pages come in more than one Elementor template, so they are read
//!   by structure rather than widget ids. They are fetched only for cards
//!   that are in scope, at most [`MAX_DETAIL_PAGES`] per run; in-scope cards
//!   past the cap are left for a later run.
//! * Times are London wall-clock. Exhibitions are `all_day` (London
//!   midnight of the first and last day), as is a single day without a time
//!   line. Anything but an exhibition spanning more than one day is a run of
//!   sessions and skipped.
//! * Category from the taxonomy: exhibition; workshop; lecture, symposium,
//!   in-dialogue and screening/talk → talk; festival → community. Anything
//!   else (education alone, screenings, …) is skipped, as are online events
//!   (location "Online…" or an "[Online]" title), password-protected posts
//!   (WordPress's `post-password-required`: the venue hasn't published the
//!   page yet) and partner venues whose location doesn't say London.
//! * Events at LUX are placed at the Waterlow Park Centre; elsewhere the
//!   location line is the venue, without coordinates.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::parse_time_range;
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, parse_price};

pub const KEY: &str = "lux";
/// Upper bound on detail pages fetched per run (≈ 40 s at 1 req / 2 s).
pub const MAX_DETAIL_PAGES: usize = 20;
const LISTING_PATH: &str = "/whats-on/";
const UPCOMING_GRID: &str = ".elementor-widget-loop-grid:not(#event-listings)";
const VENUE_NAME: &str = "LUX";
const VENUE_ADDRESS: &str = "Waterlow Park Centre, Dartmouth Park Hill, London N19 5JF";
/// LUX (OSM way 721275319).
const VENUE_LAT: f64 = 51.5684;
const VENUE_LNG: f64 = -0.1427;

pub struct Lux {
    base_url: Url,
}

impl Lux {
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

/// `/event/<slug>/` from a card's link, as `(path, slug)`.
fn event_path(href: &str) -> Option<(String, String)> {
    let url = Url::parse(href).ok()?;
    let slug = url.path().strip_prefix("/event/")?.strip_suffix('/')?;
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    valid.then(|| (url.path().to_string(), slug.to_string()))
}

/// One card of the upcoming grid, as printed.
#[derive(Debug, serde::Serialize)]
pub struct Card {
    pub path: String,
    pub slug: String,
    pub title: String,
    /// The `event_catogories-*` class suffixes.
    pub categories: Vec<String>,
    pub protected: bool,
    pub location: Option<String>,
    pub first_day: Option<String>,
    pub last_day: Option<String>,
    pub price_text: Option<String>,
    pub image_url: Option<String>,
}

/// The cards of the "Upcoming and Current Events" grid, de-duplicated by
/// slug, or an error when the page has no such grid (a template change).
pub fn parse_listing(html: &str) -> Result<Vec<Card>, SourceError> {
    let doc = Html::parse_document(html);
    let grid = doc.select(&selector(UPCOMING_GRID)).next().ok_or_else(|| {
        SourceError::Parse("no upcoming events grid on the What's on page".into())
    })?;
    let mut cards: Vec<Card> = Vec::new();
    for item in grid.select(&selector(".e-loop-item")) {
        let Some(link) = item.select(&selector("h5 a[href]")).next() else {
            continue;
        };
        let Some((path, slug)) = link.value().attr("href").and_then(event_path) else {
            continue;
        };
        if cards.iter().any(|c| c.slug == slug) {
            continue;
        }
        let title = link
            .text()
            .map(clean_text)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(": ");
        let classes: Vec<&str> = item.value().classes().collect();
        let texts: Vec<String> = item
            .select(&selector(".elementor-widget-text-editor"))
            .filter(|w| w.select(&selector("a[rel=tag]")).next().is_none())
            .map(element_text)
            .collect();
        let text = |i: usize| texts.get(i).filter(|t| !t.is_empty()).cloned();
        cards.push(Card {
            path,
            slug,
            title,
            categories: classes
                .iter()
                .filter_map(|c| c.strip_prefix("event_catogories-"))
                .map(str::to_string)
                .collect(),
            protected: classes.contains(&"post-password-required"),
            location: text(0),
            first_day: text(1),
            last_day: text(2),
            price_text: text(3),
            image_url: item
                .select(&selector(
                    ".elementor-widget-theme-post-featured-image img[src]",
                ))
                .next()
                .and_then(|i| i.value().attr("src"))
                .map(str::to_string),
        });
    }
    Ok(cards)
}

/// What only the detail page has.
#[derive(Debug, Default, serde::Serialize)]
pub struct Detail {
    pub time_text: Option<String>,
    pub description: Option<String>,
}

pub fn parse_detail(html: &str) -> Detail {
    let doc = Html::parse_document(html);
    let header = doc
        .select(&selector(".elementor-widget-theme-post-title"))
        .next()
        .and_then(|title| title.parent())
        .and_then(ElementRef::wrap);
    let time_text = header.and_then(|h| {
        h.select(&selector(
            ".elementor-inner-section .elementor-widget-text-editor",
        ))
        .nth(2)
        .map(element_text)
        .filter(|t| !t.is_empty())
    });
    let paragraphs: Vec<String> = doc
        .select(&selector(".elementor-widget-theme-post-content"))
        .next()
        .map(|content| {
            content
                .select(&selector("p"))
                .map(element_text)
                .filter(|t| {
                    !t.trim_matches('\u{feff}').is_empty() && !t.starts_with("(Listening Time")
                })
                .collect()
        })
        .unwrap_or_default();
    Detail {
        time_text,
        description: (!paragraphs.is_empty()).then(|| paragraphs.join("\n")),
    }
}

/// "6 November, 2026", optionally after a dash ("– 6 November, 2026").
pub fn parse_day(s: &str) -> Option<NaiveDate> {
    let s = clean_text(s);
    let s = s.trim_start_matches(['–', '—', '-', ' ']);
    NaiveDate::parse_from_str(s, "%d %B, %Y").ok()
}

/// A time line ("6pm – 7:30pm", "5-6pm BST") into start and optional end.
pub fn parse_time(s: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    let s = clean_text(s);
    let s = s
        .strip_suffix("BST")
        .or_else(|| s.strip_suffix("GMT"))
        .unwrap_or(&s);
    parse_time_range(&s.replace(' ', ""))
}

/// The in-scope category for an event's taxonomy slugs, or `None` to skip it.
pub fn category<S: AsRef<str>>(slugs: &[S]) -> Option<Category> {
    let has = |want: &[&str]| slugs.iter().any(|s| want.contains(&s.as_ref()));
    if has(&["exhibition"]) {
        Some(Category::Exhibition)
    } else if has(&["workshop"]) {
        Some(Category::Workshop)
    } else if has(&["lecture", "symposium", "in-dialogue", "screening-talk"]) {
        Some(Category::Talk)
    } else if has(&["festival"]) {
        Some(Category::Community)
    } else {
        None
    }
}

fn is_online(payload: &Value) -> bool {
    let starts = |key: &str, prefix: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .and_then(|t| t.get(..prefix.len()))
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
    };
    starts("location", "online") || starts("title", "[online]")
}

/// The category of an in-scope card or event payload; `None` means skip it.
pub fn in_scope(payload: &Value) -> Option<Category> {
    if payload.get("protected").and_then(Value::as_bool) == Some(true) || is_online(payload) {
        return None;
    }
    let slugs: Vec<&str> = payload
        .get("categories")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    category(&slugs)
}

/// Normalise a LUX [`RawEvent`] payload (a [`Card`], the [`Detail`] fields
/// and the absolute `url`).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let Some(category) = in_scope(payload) else {
        return Ok(None);
    };
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event without title".into()))?;
    let (venue_name, address, lat, lng) = match text("location").map(clean_text) {
        Some(l) if l.eq_ignore_ascii_case(VENUE_NAME) => (
            VENUE_NAME.to_string(),
            VENUE_ADDRESS.to_string(),
            Some(VENUE_LAT),
            Some(VENUE_LNG),
        ),
        Some(l) if l.to_lowercase().contains("london") => (l.clone(), l, None, None),
        _ => return Ok(None),
    };
    let day = |key: &str| {
        let s = text(key).unwrap_or_default();
        parse_day(s).ok_or_else(|| SourceError::Parse(format!("{title:?}: bad {key} {s:?}")))
    };
    let (first, last) = (day("first_day")?, day("last_day")?);
    if last < first {
        return Err(SourceError::Parse(format!(
            "{title:?}: ends before it starts"
        )));
    }
    let exhibition = category == Category::Exhibition;
    if !exhibition && last != first {
        return Ok(None);
    }
    let time = match text("time_text") {
        Some(t) if !exhibition => Some(
            parse_time(t)
                .ok_or_else(|| SourceError::Parse(format!("{title:?}: bad time {t:?}")))?,
        ),
        _ => None,
    };
    let (starts_at, ends_at) = match time {
        Some((start, end)) => (
            london_to_utc(first.and_time(start)),
            end.filter(|e| *e > start)
                .map(|e| london_to_utc(first.and_time(e))),
        ),
        None => (
            london_to_utc(first.and_time(NaiveTime::MIN)),
            (last > first).then(|| london_to_utc(last.and_time(NaiveTime::MIN))),
        ),
    };

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(&venue_name)),
        description: clean_description(text("description")),
        title,
        venue_name: Some(venue_name),
        address: Some(address),
        lat,
        lng,
        starts_at,
        ends_at,
        all_day: time.is_none(),
        price: text("price_text").map(parse_price).unwrap_or_default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: vec!["moving image".to_string()],
    }))
}

/// The [`RawEvent`] for `card`, whose page is at `page_url`, with what its
/// detail page added (if it was fetched).
pub fn card_event(card: &Card, page_url: &Url, detail: Option<&Detail>) -> RawEvent {
    let mut payload = serde_json::to_value(card).expect("card serialises");
    payload["url"] = json!(page_url.as_str());
    if let Some(detail) = detail {
        payload["time_text"] = json!(detail.time_text);
        payload["description"] = json!(detail.description);
    }
    RawEvent {
        source_event_id: card.slug.clone(),
        source_url: Some(page_url.to_string()),
        payload,
    }
}

#[async_trait]
impl Source for Lux {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = |path: &str| {
            self.base_url
                .join(path)
                .map_err(|e| SourceError::Config(e.to_string()))
        };
        let cards = parse_listing(&ctx.get_text(&url(LISTING_PATH)?).await?)?;
        let mut raws = Vec::new();
        let mut details_fetched = 0;
        for card in &cards {
            let page_url = url(&card.path)?;
            let raw = card_event(card, &page_url, None);
            if in_scope(&raw.payload).is_none() {
                raws.push(raw);
                continue;
            }
            if details_fetched == MAX_DETAIL_PAGES {
                continue;
            }
            details_fetched += 1;
            match ctx.get_text(&page_url).await {
                Ok(html) => raws.push(card_event(card, &page_url, Some(&parse_detail(&html)))),
                Err(e) => ctx.report_error(format!("{}: {e}", card.path)),
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

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn t(s: &str) -> NaiveTime {
        NaiveTime::parse_from_str(s, "%H:%M").unwrap()
    }

    fn event(categories: &[&str], location: &str, days: (&str, &str), time: Option<&str>) -> Value {
        json!({
            "title": "Artist Talk",
            "categories": categories,
            "protected": false,
            "location": location,
            "first_day": days.0,
            "last_day": days.1,
            "time_text": time,
        })
    }

    #[test]
    fn parses_days_and_times() {
        assert_eq!(parse_day("6 November, 2026"), Some(d("2026-11-06")));
        assert_eq!(
            parse_day(" &#8211; 12 December, 2026"),
            Some(d("2026-12-12"))
        );
        assert_eq!(parse_day("December 2026"), None);
        assert_eq!(
            parse_time("6pm – 7:30pm"),
            Some((t("18:00"), Some(t("19:30"))))
        );
        assert_eq!(
            parse_time("9am-5:30pm"),
            Some((t("09:00"), Some(t("17:30"))))
        );
        assert_eq!(
            parse_time("5-6pm BST"),
            Some((t("17:00"), Some(t("18:00"))))
        );
        assert_eq!(parse_time("Thursday – Saturday / 12pm – 4pm"), None);
    }

    #[test]
    fn maps_categories() {
        assert_eq!(category(&["exhibition"]), Some(Category::Exhibition));
        assert_eq!(category(&["workshop"]), Some(Category::Workshop));
        assert_eq!(category(&["education", "lecture"]), Some(Category::Talk));
        assert_eq!(category(&["screening-talk"]), Some(Category::Talk));
        assert_eq!(category(&["festival"]), Some(Category::Community));
        assert_eq!(category(&["education"]), None);
        assert_eq!(category::<&str>(&[]), None);
    }

    #[test]
    fn timed_talk_at_lux() {
        let e = normalise_payload(&event(
            &["screening-talk"],
            "LUX",
            ("30 July, 2026", "– 30 July, 2026"),
            Some("6pm – 7:30pm"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-07-30T17:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-07-30T18:30:00+00:00");
        assert!(!e.all_day);
        assert_eq!(e.venue_name.as_deref(), Some("LUX"));
        assert_eq!(e.lat, Some(VENUE_LAT));
    }

    #[test]
    fn exhibition_ignores_opening_hours_and_is_all_day() {
        let e = normalise_payload(&event(
            &["exhibition"],
            "LUX",
            ("9 October, 2026", "– 12 December, 2026"),
            Some("Thursday – Saturday / 12pm – 4pm"),
        ))
        .unwrap()
        .unwrap();
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-08T23:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-12-12T00:00:00+00:00");
    }

    #[test]
    fn multi_day_non_exhibition_is_skipped() {
        let days = ("12 September, 2026", "– 13 September, 2026");
        for time in [None, Some("10am – 4pm")] {
            let payload = event(&["festival"], "LUX", days, time);
            assert_eq!(normalise_payload(&payload).unwrap(), None, "{payload}");
        }
    }

    #[test]
    fn single_day_without_time_is_all_day() {
        let e = normalise_payload(&event(
            &["festival"],
            "LUX",
            ("13 September, 2026", "– 13 September, 2026"),
            None,
        ))
        .unwrap()
        .unwrap();
        assert!(e.all_day);
        assert_eq!(e.ends_at, None);
    }

    #[test]
    fn partner_venue_in_london_has_no_coordinates() {
        let e = normalise_payload(&event(
            &["symposium"],
            "Marshgate Building, UCL East, London",
            ("6 November, 2026", "– 6 November, 2026"),
            Some("9am-5:30pm"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            e.venue_name.as_deref(),
            Some("Marshgate Building, UCL East, London")
        );
        assert_eq!(e.lat, None);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-11-06T09:00:00+00:00");
    }

    #[test]
    fn skips_online_protected_unmapped_and_outside_london() {
        let days = ("1 October, 2026", "– 1 October, 2026");
        let online = event(
            &["lecture"],
            "Online (Zoom or Youtube)",
            days,
            Some("5-6pm"),
        );
        let mut online_title = event(&["workshop"], "LUX", days, None);
        online_title["title"] = json!("[Online] Distributing your work");
        let mut protected = event(&["exhibition"], "LUX", days, None);
        protected["protected"] = json!(true);
        let screening = event(&["screening"], "LUX", days, None);
        let away = event(&["lecture"], "Glasgow Film Theatre", days, Some("6pm"));
        for payload in [online, online_title, protected, screening, away] {
            assert_eq!(normalise_payload(&payload).unwrap(), None, "{payload}");
        }
    }

    #[test]
    fn bad_dates_and_times_are_errors() {
        let days = ("1 October, 2026", "– 1 October, 2026");
        for payload in [
            event(&["lecture"], "LUX", ("Autumn", "– 1 October, 2026"), None),
            event(
                &["lecture"],
                "LUX",
                ("2 October, 2026", "– 1 October, 2026"),
                None,
            ),
            event(&["lecture"], "LUX", days, Some("evening")),
        ] {
            assert!(normalise_payload(&payload).is_err(), "{payload}");
        }
    }
}
