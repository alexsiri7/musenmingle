//! 606 Club (90 Lots Road, Chelsea, SW10) — its gigs, from the site's
//! server-rendered weekly calendar and event pages (issue #225).
//!
//! * robots.txt (checked 2026-11-09, saved as a fixture) is `User-agent: *`
//!   / `Disallow: /admin/`.
//! * No JSON-LD, so CSS. `/events/` shows the current week, Monday to
//!   Sunday: one card a show (`#events .event-listing`: the event page
//!   link, a "Tue 10th Nov - 8:00pm" banner, the title and an optional
//!   series label such as "EFG LONDON JAZZ FESTIVAL"), and its "Next Week"
//!   button links the following week (`/events/?d=2026-11-16`). The weeks
//!   are read in turn, at most [`MAX_WEEKS`], up to the first with no shows
//!   (the programme ran about eight weeks ahead on 2026-11-09; past it the
//!   page says "Check back soon"). The calendar also lists the nights the
//!   club is shut ("Closed for Private Event", "Closed for the Holiday"):
//!   those cards are dropped without reading their pages.
//! * Each card's event page (at most [`MAX_DETAILS`] a run) gives the date
//!   with its year and the time in London wall clock ("Friday 13th
//!   November 2026 - 9:00 pm"; the banner leaves the year out), the
//!   "Music Charge: £25.00" (the price) and the description. Without the
//!   page (failed or over the cap, both reported) the event is left out.
//!   No end time is given.
//! * The club's own off-site series (Jazz on the Lawn at Fulham Palace)
//!   have their own pages outside the weekly calendar and are not read.
//! * Every show is music. The club calls itself a jazz club, so every show
//!   is tagged `jazz`; `crate::music` adds the rest from the series label
//!   and title.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_to_utc, month_number, parse_price,
};

pub const KEY: &str = "606-club";
pub const LISTING_PATH: &str = "/events/";
const EVENT_PREFIX: &str = "/events/view/";
/// At most this many weeks of the calendar a run.
pub const MAX_WEEKS: usize = 10;
/// At most this many event pages a run (about 50 shows on 2026-11-09).
pub const MAX_DETAILS: usize = 70;
const VENUE_NAME: &str = "606 Club";
const VENUE_ADDRESS: &str = "90 Lots Road, London SW10 0QD";

pub struct Club606 {
    base_url: Url,
    max_details: usize,
}

impl Club606 {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            max_details: MAX_DETAILS,
        }
    }

    /// Override the per-run cap on event pages (tests).
    pub fn with_max_details(mut self, n: usize) -> Self {
        self.max_details = n;
        self
    }

    /// The cards of the calendar's weeks, from the current one on.
    async fn cards(&self, ctx: &FetchContext) -> Result<Vec<Value>, SourceError> {
        let mut url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let mut cards: Vec<Value> = Vec::new();
        for week in 1..=MAX_WEEKS {
            let listing = match ctx.get_text(&url).await {
                Ok(html) => parse_week(&html, &url)
                    .ok_or_else(|| SourceError::Parse(format!("no calendar at {}", url.path()))),
                Err(e) => Err(e.into()),
            };
            let listing = match listing {
                Ok(listing) => listing,
                // The current week failing fails the run; a later one is a
                // soft error and the cards found so far are kept.
                Err(e) if week > 1 => {
                    ctx.report_error(format!("calendar week {week}: {e}"));
                    break;
                }
                Err(e) => return Err(e),
            };
            if listing.shows == 0 {
                break;
            }
            for card in listing.cards {
                if !cards.iter().any(|c| c["slug"] == card["slug"]) {
                    cards.push(card);
                }
            }
            match listing.next_week {
                Some(next) => url = next,
                None => break,
            }
        }
        Ok(cards)
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn text_of(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// One week of the calendar.
#[derive(Debug)]
pub struct Week {
    /// Its shows, the nights the club is closed left out.
    pub cards: Vec<Value>,
    /// How many cards it had, closed nights included.
    pub shows: usize,
    /// The following week's page.
    pub next_week: Option<Url>,
}

/// The week of the calendar fetched from `page_url`; `None` when it is not
/// the calendar at all (not the same as a week with no shows).
pub fn parse_week(html: &str, page_url: &Url) -> Option<Week> {
    let doc = Html::parse_document(html);
    let events = doc.select(&selector("#events")).next()?;
    let mut cards: Vec<Value> = Vec::new();
    let mut shows = 0;
    for card in events.select(&selector(".event-listing")) {
        shows += 1;
        let Some(url) = card
            .select(&selector("a[href]"))
            .filter_map(|a| page_url.join(a.value().attr("href")?.trim()).ok())
            .find(|u| {
                u.path()
                    .strip_prefix(EVENT_PREFIX)
                    .is_some_and(|s| !s.trim_matches('/').is_empty())
            })
        else {
            continue;
        };
        let slug = url.path()[EVENT_PREFIX.len()..]
            .trim_matches('/')
            .to_string();
        let title = card.select(&selector(".h4")).next().map(text_of);
        if title
            .as_deref()
            .is_some_and(|t| t.to_lowercase().starts_with("closed for"))
        {
            continue;
        }
        let label = card
            .select(&selector(".contained p"))
            .skip_while(|p| p.value().attr("class") != Some("h4"))
            .nth(1)
            .map(text_of)
            .filter(|s| !s.is_empty());
        if cards.iter().any(|c| c["slug"] == slug.as_str()) {
            continue;
        }
        cards.push(json!({
            "slug": slug,
            "url": url.as_str(),
            "title": title,
            "label": label,
        }));
    }
    let next_week = events
        .select(&selector("a.button[href]"))
        .find(|a| text_of(*a) == "Next Week")
        .and_then(|a| page_url.join(a.value().attr("href")?.trim()).ok());
    Some(Week {
        cards,
        shows,
        next_week,
    })
}

/// What an event page adds to its card: the date-and-time heading
/// (`when`), the "Music Charge" line (`charge`), the description and the
/// image. `None` when it is not an event page.
pub fn parse_detail(html: &str) -> Option<Value> {
    let doc = Html::parse_document(html);
    let bricks = doc
        .select(&selector(".box h1.banner ~ .contained .bricks"))
        .next()?;
    let when = bricks
        .select(&selector(".brick h3.shallow"))
        .next()
        .map(text_of);
    let charge = bricks
        .select(&selector(".brick h3"))
        .map(text_of)
        .find(|h| h.to_lowercase().contains("charge"));
    let image = bricks
        .select(&selector(".brick img[src]"))
        .find_map(|i| i.value().attr("src"))
        .map(|s| s.trim().to_string());
    let description = bricks
        .select(&selector(".brick"))
        .find(|b| {
            b.select(&selector("p")).next().is_some()
                && b.select(&selector("h3, img, .video-container"))
                    .next()
                    .is_none()
        })
        .map(|b| b.inner_html());
    Some(json!({
        "when": when,
        "charge": charge,
        "description": description,
        "image_url": image,
    }))
}

/// The stored payload for a card; `detail` is [`parse_detail`]'s output.
pub fn raw_event(card: &Value, detail: Value) -> RawEvent {
    RawEvent {
        source_event_id: card["slug"].as_str().unwrap_or_default().to_string(),
        source_url: card["url"].as_str().map(str::to_string),
        payload: json!({
            "card": card,
            "detail": detail,
        }),
    }
}

/// An event page's heading, "Friday 13th November 2026 - 9:00 pm" (the
/// ordinal may come apart from the day: "13 th"), in London wall clock.
pub fn parse_when(text: &str) -> Option<NaiveDateTime> {
    let (date, time) = text.split_once(" - ")?;
    let words: Vec<&str> = date
        .split_whitespace()
        .filter(|w| !matches!(*w, "st" | "nd" | "rd" | "th"))
        .collect();
    let [weekday, day, month, year] = words[..] else {
        return None;
    };
    let day = day.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let date =
        NaiveDate::from_ymd_opt(year.parse().ok()?, month_number(month)?, day.parse().ok()?)?;
    let weekday: Weekday = weekday.parse().ok()?;
    if date.weekday() != weekday {
        return None;
    }
    Some(date.and_time(parse_time(time)?))
}

/// "9:00 pm", "1:30pm".
fn parse_time(text: &str) -> Option<NaiveTime> {
    let t: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    let (num, pm) = match (t.strip_suffix("pm"), t.strip_suffix("am")) {
        (Some(n), _) => (n, true),
        (_, Some(n)) => (n, false),
        _ => return None,
    };
    let (h, m): (u32, u32) = match num.split_once([':', '.']) {
        Some((h, m)) => (h.parse().ok()?, m.parse().ok()?),
        None => (num.parse().ok()?, 0),
    };
    if !(1..=12).contains(&h) {
        return None;
    }
    let h = match (pm, h) {
        (true, 12) => 12,
        (true, h) => h + 12,
        (false, 12) => 0,
        (false, h) => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Normalise a 606 Club [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let card = &payload["card"];
    let detail = &payload["detail"];
    let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let title =
        text(&card["title"]).ok_or_else(|| SourceError::Parse("card without a title".into()))?;
    if !detail.is_object() {
        return Err(SourceError::Parse(format!("{title:?}: no event page")));
    }
    let when = detail["when"].as_str().unwrap_or_default();
    let starts_at = parse_when(when)
        .map(london_to_utc)
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: date {when:?}")))?;
    let mut hints = vec!["jazz".to_string()];
    hints.extend(text(&card["label"]));
    let tags = crate::music::tags_for("music", &hints, &title)
        .into_iter()
        .map(str::to_string)
        .collect();
    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(detail["description"].as_str()),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at: None,
        all_day: false,
        price: parse_price(detail["charge"].as_str().unwrap_or_default()),
        url: text(&card["url"]),
        image_url: text(&detail["image_url"]),
        category: Category::Music,
        tags,
        title,
    }))
}

#[async_trait]
impl Source for Club606 {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let cards = self.cards(ctx).await?;
        let mut out = Vec::new();
        for (n, card) in cards.iter().enumerate() {
            let slug = card["slug"].as_str().unwrap_or_default();
            if n >= self.max_details {
                ctx.report_error(format!(
                    "{slug}: over the {} event-page cap",
                    self.max_details
                ));
                continue;
            }
            let Some(Ok(url)) = card["url"].as_str().map(Url::parse) else {
                continue;
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html) {
                    Some(detail) => out.push(raw_event(card, detail)),
                    None => ctx.report_error(format!("{slug}: not an event page")),
                },
                Err(e) => ctx.report_error(format!("{slug}: {e}")),
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only the shows in the club's weekly calendar (What's On), at most ten weeks \
             ahead. The nights it lists as closed (\"Closed for Private Event\", \"Closed \
             for the Holiday\") and its off-site series such as Jazz on the Lawn at Fulham \
             Palace are left out on purpose.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    #[test]
    fn event_page_headings() {
        assert_eq!(
            parse_when("Friday 13 th November 2026 - 9:00 pm"),
            Some(at(2026, 11, 13, 21, 0))
        );
        assert_eq!(
            parse_when("Sunday 15th November 2026 - 1:30 pm"),
            Some(at(2026, 11, 15, 13, 30))
        );
        assert_eq!(
            parse_when("Friday 1 st January 2027 - 8:00 pm"),
            Some(at(2027, 1, 1, 20, 0))
        );
        // A weekday that doesn't match the date is a misread.
        assert_eq!(parse_when("Monday 13 th November 2026 - 9:00 pm"), None);
        assert_eq!(parse_when("Friday 13 th November 2026"), None);
        assert_eq!(parse_when("Friday 13 th November 2026 - TBC"), None);
        assert_eq!(parse_when("13 November 2026 - 9:00 pm"), None);
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("9:00 pm"), NaiveTime::from_hms_opt(21, 0, 0));
        assert_eq!(parse_time("12:00 pm"), NaiveTime::from_hms_opt(12, 0, 0));
        assert_eq!(parse_time("11am"), NaiveTime::from_hms_opt(11, 0, 0));
        assert_eq!(parse_time("13:00 pm"), None);
        assert_eq!(parse_time(""), None);
    }

    fn payload(label: Option<&str>, title: &str, detail: Value) -> Value {
        json!({
            "card": {
                "slug": "x",
                "url": "https://www.606club.co.uk/events/view/x/",
                "title": title,
                "label": label,
            },
            "detail": detail,
        })
    }

    #[test]
    fn a_show_is_timed_jazz_with_its_music_charge() {
        let detail = json!({
            "when": "Thursday 31 st December 2026 - 7:30 pm",
            "charge": "Music Charge: £170.00",
        });
        let ev = normalise_payload(&payload(None, "New Year's Eve 2026 with Imaani", detail))
            .unwrap()
            .unwrap();
        assert_eq!(ev.category, Category::Music);
        assert_eq!(ev.tags, ["jazz"]);
        assert!(!ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-12-31T19:30:00+00:00");
        assert_eq!(ev.ends_at, None);
        assert_eq!(ev.price.min.unwrap().to_string(), "170.00");
        assert_eq!(ev.venue_name.as_deref(), Some("606 Club"));
    }

    #[test]
    fn the_series_label_and_title_add_subtags() {
        let detail = json!({"when": "Friday 13 th November 2026 - 9:00 pm"});
        let ev = normalise_payload(&payload(
            Some("Experimental Night"),
            "Big Band Orchestra",
            detail,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(ev.tags, ["classical", "experimental", "jazz"]);
        // British Summer Time: a show in June is an hour behind in UTC.
        let detail = json!({"when": "Friday 12 th June 2026 - 8:00 pm"});
        let ev = normalise_payload(&payload(None, "Trio", detail))
            .unwrap()
            .unwrap();
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-06-12T19:00:00+00:00");
    }

    #[test]
    fn a_missing_page_or_unreadable_date_is_an_error() {
        let err = |detail: Value| {
            normalise_payload(&payload(None, "Trio", detail))
                .unwrap_err()
                .to_string()
        };
        assert!(err(Value::Null).contains("no event page"));
        assert!(err(json!({"when": "TBC"})).contains("date \"TBC\""));
        assert!(err(json!({})).contains("date \"\""));
    }
}
