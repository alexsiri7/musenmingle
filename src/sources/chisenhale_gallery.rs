//! Chisenhale Gallery (Bow, E3) — listing-only CSS scraper over the
//! "What's On" page.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   allows `/whats-on/` and `/project/` but asks for `Crawl-delay: 20` (and
//!   `Request-rate: 1/60`). So a run makes exactly two requests — robots.txt
//!   and the listing — and never fetches detail pages.
//! * There is no JSON-LD. The "Upcoming" block
//!   (`section.block-calendar li.list-item.event`) has everything per card:
//!   a type label ("Exhibition", "Artist Talk", "Panel Discussion"), a date
//!   line, a title, an optional artist line and (for exhibitions) an image.
//!   Detail pages would only add the description, which is not worth 20 s a
//!   page; descriptions are therefore absent.
//! * Dates have no year: `2 October – 6 December` (exhibitions) or
//!   `3 October, 3–5pm` (events). `fetch` records the London date of the run
//!   in the payload (`listed_on`) and [`infer_date`] resolves the year
//!   against it, so normalising a stored payload is deterministic: a date is
//!   placed in the year that puts it between 90 days before and 275 days
//!   after `listed_on` (a card still listed a few days after it happened
//!   stays in the past rather than jumping a year ahead). For ranges the end
//!   is resolved first and the start takes the end's year (or the year
//!   before). Explicit years are honoured.
//! * Times are London wall-clock ("3–5pm", "7–8:30pm", "11am–1pm"). A start
//!   without am/pm takes the end's, unless that would put it after the end.
//!   Exhibitions and dates without a time are stored as London midnight of
//!   their first and last day, as for the other gallery scrapers.
//! * Category comes from the type label: exhibitions, talks (talk,
//!   conversation, discussion, panel, tour, lecture, …) and workshops are in
//!   scope; anything else falls back to `map_category` over label and title
//!   and is skipped (`Ok(None)`) when nothing matches (screenings,
//!   performances). Open-ended dates ("Until …", "Ongoing") are skipped too.
//! * Price: the site-wide footer says "Free Entry" (gallery admission), which
//!   is applied to exhibitions only; event prices are unknown.
//! * Every item is placed at the gallery.

use async_trait::async_trait;
use chrono::{Datelike, Duration, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_text, dedupe_key, london_date, london_to_utc, map_category, parse_price,
};

pub const KEY: &str = "chisenhale-gallery";
const LISTING_PATH: &str = "/whats-on/";
const VENUE_NAME: &str = "Chisenhale Gallery";
const VENUE_ADDRESS: &str = "64 Chisenhale Road, London E3 5QZ";
/// Approximate location of the building.
const VENUE_LAT: f64 = 51.5345;
const VENUE_LNG: f64 = -0.0365;
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
/// A year-less date is placed no earlier than this many days before the
/// listing date…
const PAST_WINDOW_DAYS: i64 = 90;
/// …and less than this many days after it.
const FUTURE_WINDOW_DAYS: i64 = 275;

pub struct ChisenhaleGallery {
    base_url: Url,
}

impl ChisenhaleGallery {
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

/// Parse the "Upcoming" cards of the What's On page. `page_url` is the
/// address the page was fetched from (card links are resolved against it and
/// must stay on its host); `listed_on` is the London date of the fetch, kept
/// in each payload for year inference.
pub fn parse_listing(html: &str, page_url: &Url, listed_on: NaiveDate) -> Vec<RawEvent> {
    let doc = Html::parse_document(html);
    let admission = doc
        .select(&selector(".contact-opening_note"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty());
    let text_in = |card: ElementRef<'_>, s: &str| {
        card.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let mut out: Vec<RawEvent> = Vec::new();
    for card in doc.select(&selector(
        "section.block-calendar li.list-item.event > a[href]",
    )) {
        let Some(Ok(url)) = card.value().attr("href").map(|h| page_url.join(h)) else {
            continue;
        };
        if url.host_str() != page_url.host_str() || url.query().is_some() {
            continue;
        }
        let id = url.path().trim_matches('/').to_string();
        if id.is_empty() || out.iter().any(|r| r.source_event_id == id) {
            continue;
        }
        let image_url = card
            .select(&selector(".list-item-media img[src]"))
            .next()
            .and_then(|e| e.value().attr("src"))
            .filter(|s| !s.starts_with("data:"))
            .and_then(|s| page_url.join(s).ok())
            .map(|u| u.to_string());
        out.push(RawEvent {
            source_event_id: id,
            source_url: Some(url.to_string()),
            payload: json!({
                "url": url.as_str(),
                "type": text_in(card, ".list-item-details .type"),
                "date_text": text_in(card, ".list-item-details .date"),
                "title": text_in(card, ".list-item-details .title"),
                "artist": text_in(card, ".list-item-details .artist"),
                "image_url": image_url,
                "admission_text": admission,
                "listed_on": listed_on.to_string(),
            }),
        });
    }
    out
}

/// A date as printed: day, optional month, optional year.
type DayParts = (u32, Option<u32>, Option<i32>);

/// "[Sat] 3 october [2026]" (lower-cased) → parts. Months match on their
/// first three letters ("sept", "october").
fn parse_day(s: &str) -> Option<DayParts> {
    let mut tokens: Vec<&str> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens
        .first()
        .is_some_and(|t| WEEKDAYS.iter().any(|w| t.get(..3) == Some(*w)))
    {
        tokens.remove(0);
    }
    let month = |m: &str| MONTHS.iter().position(|p| m.get(..3) == Some(*p));
    let (day, month, year) = match tokens.as_slice() {
        [d] => (d, None, None),
        [d, m] => (d, Some(month(m)? as u32 + 1), None),
        [d, m, y] => (d, Some(month(m)? as u32 + 1), Some(y.parse().ok()?)),
        _ => return None,
    };
    let day: u32 = day.parse().ok()?;
    (1..=31).contains(&day).then_some((day, month, year))
}

/// Resolve a year-less `month`/`day` against the listing date: the year that
/// puts it in `[listed_on - 90 days, listed_on + 275 days)`.
pub fn infer_date(month: u32, day: u32, listed_on: NaiveDate) -> Option<NaiveDate> {
    let lo = listed_on - Duration::days(PAST_WINDOW_DAYS);
    let hi = listed_on + Duration::days(FUTURE_WINDOW_DAYS);
    (listed_on.year() - 1..=listed_on.year() + 1)
        .filter_map(|y| NaiveDate::from_ymd_opt(y, month, day))
        .find(|d| *d >= lo && *d < hi)
}

/// Parse the date part of a card ("2 October – 6 December", "3 October",
/// "2–6 December", "30 December 2026 – 3 January 2027") into its first and
/// last day. `Ok(None)` for open-ended text ("Until …", "From …",
/// "Ongoing").
pub fn parse_date_range(
    text: &str,
    listed_on: NaiveDate,
) -> Result<Option<(NaiveDate, NaiveDate)>, SourceError> {
    let lower = text.to_lowercase();
    let lower = lower.trim();
    if ["until", "from", "ongoing", "open", "continues"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return Ok(None);
    }
    let err = || SourceError::Parse(format!("unrecognised date {text:?}"));
    let (start_text, end_text) = match lower.split_once(['–', '—', '-']) {
        Some((a, b)) => (a, Some(b)),
        None => (lower, None),
    };
    let (sd, sm, sy) = parse_day(start_text).ok_or_else(err)?;
    let Some(end_text) = end_text else {
        let month = sm.ok_or_else(err)?;
        let date = match sy {
            Some(y) => NaiveDate::from_ymd_opt(y, month, sd),
            None => infer_date(month, sd, listed_on),
        }
        .ok_or_else(err)?;
        return Ok(Some((date, date)));
    };
    let (ed, em, ey) = parse_day(end_text).ok_or_else(err)?;
    let end_month = em.ok_or_else(err)?;
    let end = match ey {
        Some(y) => NaiveDate::from_ymd_opt(y, end_month, ed),
        None => infer_date(end_month, ed, listed_on),
    }
    .ok_or_else(err)?;
    let start_month = sm.unwrap_or(end_month);
    let start = match sy {
        Some(y) => NaiveDate::from_ymd_opt(y, start_month, sd),
        None => NaiveDate::from_ymd_opt(end.year(), start_month, sd)
            .filter(|d| *d <= end)
            .or_else(|| NaiveDate::from_ymd_opt(end.year() - 1, start_month, sd)),
    }
    .ok_or_else(err)?;
    if start > end {
        return Err(err());
    }
    Ok(Some((start, end)))
}

/// One side of a time range: hour, minute and am/pm if given.
fn parse_clock(s: &str) -> Option<(u32, u32, Option<bool>)> {
    let s = s.trim().to_lowercase();
    let (num, pm) = if let Some(n) = s.strip_suffix("pm") {
        (n.trim(), Some(true))
    } else if let Some(n) = s.strip_suffix("am") {
        (n.trim(), Some(false))
    } else {
        (s.as_str(), None)
    };
    let (h, m) = match num.split_once([':', '.']) {
        Some((h, m)) => (h.parse().ok()?, m.parse().ok()?),
        None => (num.parse().ok()?, 0),
    };
    let valid = m < 60
        && if pm.is_some() {
            (1..=12).contains(&h)
        } else {
            h < 24
        };
    valid.then_some((h, m, pm))
}

fn to_time(h: u32, m: u32, pm: Option<bool>) -> Option<NaiveTime> {
    let h = match pm {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        _ => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Parse a time or time range ("3–5pm", "7–8:30pm", "11am–1pm", "6.30pm",
/// "18:00–21:00") into start and optional end.
pub fn parse_time_range(text: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    let Some((a, b)) = text.split_once(['–', '—', '-']) else {
        let (h, m, pm) = parse_clock(text)?;
        return Some((to_time(h, m, pm)?, None));
    };
    let (eh, em, epm) = parse_clock(b)?;
    let end = to_time(eh, em, epm)?;
    let (sh, sm, spm) = parse_clock(a)?;
    let start = match (spm, epm) {
        (Some(_), _) | (None, None) => to_time(sh, sm, spm)?,
        (None, Some(pm)) => {
            let same = to_time(sh, sm, Some(pm))?;
            if same <= end || !pm {
                same
            } else {
                to_time(sh, sm, Some(false))?
            }
        }
    };
    Some((start, Some(end)))
}

/// Map a card's type label (and title as a fallback hint) to a category.
pub fn category(type_label: &str, title: &str) -> Option<Category> {
    let lower = type_label.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if has(&["exhibition"]) {
        Some(Category::Exhibition)
    } else if has(&["workshop"]) {
        Some(Category::Workshop)
    } else if has(&[
        "talk",
        "conversation",
        "discussion",
        "panel",
        "tour",
        "lecture",
        "symposium",
        "seminar",
        "reading",
    ]) {
        Some(Category::Talk)
    } else {
        map_category(&[type_label, title])
    }
}

/// Normalise a Chisenhale Gallery [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|t| !t.is_empty())
    };
    let work = text("title").ok_or_else(|| SourceError::Parse("card without title".into()))?;
    let date_text = text("date_text")
        .ok_or_else(|| SourceError::Parse(format!("{work:?}: card without date")))?;
    let listed_on = text("listed_on")
        .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
        .ok_or_else(|| SourceError::Parse("payload without listed_on date".into()))?;
    let type_label = text("type").unwrap_or_default();
    let Some(category) = category(&type_label, &work) else {
        return Ok(None);
    };
    let title = match text("artist") {
        Some(artist) if category == Category::Exhibition => format!("{artist}: {work}"),
        _ => work,
    };

    let (date_part, time_part) = match date_text.split_once(',') {
        Some((d, t)) => (d.trim(), Some(t.trim())),
        None => (date_text.as_str(), None),
    };
    let Some((first_day, last_day)) = parse_date_range(date_part, listed_on)? else {
        return Ok(None);
    };
    let times = match time_part.filter(|_| category != Category::Exhibition) {
        Some(t) => Some(parse_time_range(t).ok_or_else(|| {
            SourceError::Parse(format!("unrecognised time {t:?} in {date_text:?}"))
        })?),
        None => None,
    };
    let (start_time, end_time) = match times {
        Some((s, e)) => (s, e),
        None => (NaiveTime::MIN, None),
    };
    let starts_at = london_to_utc(first_day.and_time(start_time));
    let ends_at = match (times, end_time) {
        (Some(_), Some(e)) => Some(london_to_utc(last_day.and_time(e))),
        (Some(_), None) => None,
        (None, _) => Some(london_to_utc(last_day.and_time(NaiveTime::MIN))),
    }
    .filter(|e| *e > starts_at);

    let price = match text("admission_text") {
        Some(a) if category == Category::Exhibition => parse_price(&a),
        _ => Price::default(),
    };
    let tags = match text("type") {
        Some(t) => vec!["art".to_string(), t.to_lowercase()],
        None => vec!["art".to_string()],
    };

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: None,
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day: times.is_none(),
        price,
        url: text("url"),
        image_url: text("image_url"),
        category,
        tags,
    }))
}

#[async_trait]
impl Source for ChisenhaleGallery {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let html = ctx.get_text(&url).await?;
        let items = parse_listing(&html, &url, london_date(Utc::now()));
        if items.is_empty() {
            return Err(SourceError::Parse(
                "no event cards found on the What's On page".into(),
            ));
        }
        Ok(items)
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

    fn range(text: &str, listed_on: &str) -> Option<(String, String)> {
        parse_date_range(text, d(listed_on))
            .unwrap()
            .map(|(a, b)| (a.to_string(), b.to_string()))
    }

    fn pair(a: &str, b: &str) -> Option<(String, String)> {
        Some((a.into(), b.into()))
    }

    #[test]
    fn infers_years_from_the_listing_date() {
        let listed = "2026-09-26";
        // The fixture's cards.
        assert_eq!(
            range("2 October – 6 December", listed),
            pair("2026-10-02", "2026-12-06")
        );
        assert_eq!(range("3 October", listed), pair("2026-10-03", "2026-10-03"));
        // A card still listed shortly after it happened stays in the past.
        assert_eq!(
            range("3 October", "2026-10-04"),
            pair("2026-10-03", "2026-10-03")
        );
        // December listing, January event: next year.
        assert_eq!(
            range("10 January", "2026-12-15"),
            pair("2027-01-10", "2027-01-10")
        );
        // A range across the new year resolves the start from the end.
        assert_eq!(
            range("2 November – 6 February", "2026-10-01"),
            pair("2026-11-02", "2027-02-06")
        );
        // A running exhibition that opened months ago.
        assert_eq!(
            range("12 June – 11 October", listed),
            pair("2026-06-12", "2026-10-11")
        );
        // Far-ahead dates (up to ~9 months) stay ahead.
        assert_eq!(range("20 May", listed), pair("2027-05-20", "2027-05-20"));
        // Same-month ranges and explicit years.
        assert_eq!(
            range("2–6 December", listed),
            pair("2026-12-02", "2026-12-06")
        );
        assert_eq!(
            range("30 December 2026 – 3 January 2027", "2020-01-01"),
            pair("2026-12-30", "2027-01-03")
        );
        assert_eq!(
            range("Sat 3 Oct – Sun 4 Oct", listed),
            pair("2026-10-03", "2026-10-04")
        );
    }

    #[test]
    fn open_ended_and_bad_dates() {
        assert_eq!(range("Until 6 December", "2026-09-26"), None);
        assert_eq!(range("Ongoing", "2026-09-26"), None);
        for text in [
            "Autumn",
            "6 December 2026 – 2 October 2026",
            "3",
            "31 February",
        ] {
            assert!(parse_date_range(text, d("2026-09-26")).is_err(), "{text}");
        }
    }

    fn t(text: &str) -> Option<(String, Option<String>)> {
        parse_time_range(text).map(|(a, b)| (a.to_string(), b.map(|b| b.to_string())))
    }

    fn times(a: &str, b: Option<&str>) -> Option<(String, Option<String>)> {
        Some((a.into(), b.map(str::to_string)))
    }

    #[test]
    fn parses_card_times() {
        assert_eq!(t("3–5pm"), times("15:00:00", Some("17:00:00")));
        assert_eq!(t("7–8:30pm"), times("19:00:00", Some("20:30:00")));
        assert_eq!(t("11–1pm"), times("11:00:00", Some("13:00:00")));
        assert_eq!(t("12–2pm"), times("12:00:00", Some("14:00:00")));
        assert_eq!(t("11am–1pm"), times("11:00:00", Some("13:00:00")));
        assert_eq!(t("10–11am"), times("10:00:00", Some("11:00:00")));
        assert_eq!(t("6.30pm"), times("18:30:00", None));
        assert_eq!(t("12pm"), times("12:00:00", None));
        assert_eq!(t("18:00–21:00"), times("18:00:00", Some("21:00:00")));
        assert_eq!(t("tbc"), None);
        assert_eq!(t("13pm"), None);
    }

    fn payload(kind: &str, date: &str) -> Value {
        json!({
            "url": "https://chisenhale.org.uk/whats-on/x/",
            "type": kind,
            "date_text": date,
            "title": "Something",
            "artist": "Someone",
            "admission_text": "Free Entry",
            "listed_on": "2026-09-26",
        })
    }

    #[test]
    fn event_times_are_london_wall_clock() {
        let e = normalise_payload(&payload("Artist Talk", "3 October, 3–5pm"))
            .unwrap()
            .unwrap();
        assert_eq!(e.category, Category::Talk);
        assert_eq!(e.title, "Something");
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-03T14:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-03T16:00:00+00:00");
        assert_eq!(e.price, Price::default());
        // After the clocks go back: GMT.
        let e = normalise_payload(&payload("Curatorial Tour", "3 December, 7–8:30pm"))
            .unwrap()
            .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-12-03T19:00:00+00:00");
    }

    #[test]
    fn exhibitions_are_free_date_ranges_titled_with_the_artist() {
        let e = normalise_payload(&payload("Exhibition", "2 October – 6 December"))
            .unwrap()
            .unwrap();
        assert_eq!(e.category, Category::Exhibition);
        assert_eq!(e.title, "Someone: Something");
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-01T23:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-12-06T00:00:00+00:00");
        assert!(e.price.is_free);
    }

    #[test]
    fn out_of_scope_and_open_ended_cards_are_skips() {
        assert!(
            normalise_payload(&payload("Screening", "3 October, 7pm"))
                .unwrap()
                .is_none()
        );
        assert!(
            normalise_payload(&payload("Exhibition", "Until 6 December"))
                .unwrap()
                .is_none()
        );
        assert!(normalise_payload(&payload("Artist Talk", "3 October, soon")).is_err());
    }

    #[test]
    fn category_rules() {
        assert_eq!(category("Panel Discussion", ""), Some(Category::Talk));
        assert_eq!(category("Curatorial Tour", ""), Some(Category::Talk));
        assert_eq!(category("Family Workshop", ""), Some(Category::Workshop));
        assert_eq!(category("Performance", "An evening"), None);
    }
}
