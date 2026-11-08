//! Stone Nest (136 Shaftesbury Avenue, W1) — concerts in a former chapel
//! and its basement bar, Below Stone Nest, from the site's server-rendered
//! events page (issue #220).
//!
//! * robots.txt (checked 2026-11-08, saved as a fixture) is empty: nothing
//!   is disallowed.
//! * The site is Webflow, with no JSON-LD, so CSS. `/events` holds two
//!   lists: `.events-current` (the upcoming events, 8 on 2026-11-08) and
//!   `.events-past`, which is ignored. Webflow renders every variant of a
//!   field and hides the unused ones with `w-condition-invisible`, so only
//!   visible elements are read ([`visible_text`]). A card
//!   (`.collection-item-event`) has the event page link, a date line
//!   (`.whatson-date`), a title, a subtitle (often a festival or "Presented
//!   by …"), a one-line summary, the room (`.whatson-place`: "Stone Nest" or
//!   "Below Stone Nest") and an image.
//! * Date lines are London wall clock without a year: "13 November
//!   7.30pm", "15 November 3pm & 6pm" (two sessions), "23/25/27/28 November
//!   7.30pm" (sessions, #207) or a run "23 November – 28 November 7.30pm"
//!   (a session each day). The year puts the last day no more than
//!   [`PAST_DAYS`] before the fetch; earlier days follow it back. A line
//!   without a time is all day.
//! * Each card's event page (at most [`MAX_DETAILS`] a run) adds the full
//!   description (`.info .rte`) and whether it is free ("Free admission"
//!   instead of "Book Now"); prices are only on the ticket site, so price
//!   is otherwise unknown. Without the page (failed or over the cap,
//!   reported) the card's summary is the description.
//! * Category: the venue has no genre labels and its programme is music
//!   (gigs, jazz, opera, choirs, club nights), so every event is music; the
//!   occasional theatre, dance or comedy show ([`NOT_MUSIC`] in the title or
//!   subtitle) and cancelled events are skipped. Music subtags come from
//!   `crate::music`'s keywords in the card's title, subtitle and summary
//!   ("EFG London Jazz Festival" → jazz).

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent, Session};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, month_number,
    padded_words,
};

pub const KEY: &str = "stone-nest";
pub const LISTING_PATH: &str = "/events";
const EVENT_PREFIX: &str = "/events/";
/// At most this many event pages a run (8 upcoming events on 2026-11-08).
pub const MAX_DETAILS: usize = 40;
const VENUE_NAME: &str = "Stone Nest";
const VENUE_ADDRESS: &str = "136 Shaftesbury Avenue, London W1D 5EZ";
/// How far before the fetch's day an event's last day may fall (a card
/// still listed on the days after).
const PAST_DAYS: i64 = 7;
/// Words or phrases (as [`padded_words`]) in a title or subtitle of an
/// event that is not a concert.
const NOT_MUSIC: &[&str] = &[
    "theatre",
    "monologue",
    "monologues",
    "comedy",
    "contemporary dance",
    "dance school",
    "cancelled",
    "postponed",
];

pub struct StoneNest {
    base_url: Url,
    max_details: usize,
}

impl StoneNest {
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
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn hidden(e: ElementRef<'_>) -> bool {
    e.value().classes().any(|c| c == "w-condition-invisible")
}

/// Whether Webflow shows `e`: neither it nor an ancestor is hidden.
fn is_visible(e: ElementRef<'_>) -> bool {
    !hidden(e) && !e.ancestors().filter_map(ElementRef::wrap).any(hidden)
}

/// The visible text of `e` (hidden descendants left out), cleaned.
fn visible_text(e: ElementRef<'_>) -> String {
    let parts: Vec<&str> = e
        .descendants()
        .filter_map(|n| {
            let shown = !n
                .ancestors()
                .filter_map(ElementRef::wrap)
                .take_while(|a| a.id() != e.id())
                .any(hidden);
            n.value().as_text().filter(|_| shown).map(|t| &**t)
        })
        .collect();
    clean_text(&parts.join(" "))
}

/// The visible text of the first visible match of `sel` in `scope`, if not
/// empty.
fn first_text(scope: ElementRef<'_>, sel: &str) -> Option<String> {
    scope
        .select(&selector(sel))
        .find(|e| is_visible(*e))
        .map(visible_text)
        .filter(|t| !t.is_empty())
}

/// The upcoming event cards on the events page fetched from `page_url`, in
/// page order, one per event page; `None` when the page has no upcoming
/// list at all (not the same as an empty one).
pub fn parse_listing(html: &str, page_url: &Url) -> Option<Vec<Value>> {
    let doc = Html::parse_document(html);
    let list = doc.select(&selector(".events-current")).next()?;
    let mut cards: Vec<Value> = Vec::new();
    for card in list.select(&selector(".collection-item-event")) {
        let Some(url) = card
            .select(&selector("a.whatson-link[href]"))
            .filter_map(|a| page_url.join(a.value().attr("href")?).ok())
            .next()
        else {
            continue;
        };
        let Some(slug) = url
            .path()
            .strip_prefix(EVENT_PREFIX)
            .filter(|s| !s.is_empty() && !s.contains('/'))
            .map(str::to_string)
        else {
            continue;
        };
        if url.host_str() != page_url.host_str()
            || url.query().is_some()
            || cards.iter().any(|c| c["slug"] == slug.as_str())
        {
            continue;
        }
        let image = card
            .select(&selector("img.whatson-img"))
            .find(|e| is_visible(*e))
            .and_then(|img| page_url.join(img.value().attr("src")?).ok());
        cards.push(json!({
            "slug": slug,
            "url": url.as_str(),
            "date": first_text(card, ".whatson-date"),
            "title": first_text(card, ".whatson-title .heading"),
            "subtitle": first_text(card, ".whatson-title > .body-text"),
            "summary": first_text(card, ".whatson-info-short"),
            "place": first_text(card, ".whatson-place"),
            "image_url": image.map(|u| u.to_string()),
        }));
    }
    Some(cards)
}

/// What an event page adds to its card: the description and whether it is
/// free. `None` when it is not an event page.
pub fn parse_detail(html: &str) -> Option<Value> {
    let doc = Html::parse_document(html);
    let root = doc.root_element();
    first_text(root, "h1.heading.event")?;
    let description: Vec<String> = root
        .select(&selector(".info .rte"))
        .filter(|e| is_visible(*e))
        .map(|e| e.inner_html())
        .collect();
    let free = root
        .select(&selector(
            ".tickets-wrap .tickets-free, .tickets-wrap .tickets-free-link",
        ))
        .any(is_visible);
    Some(json!({
        "description": (!description.is_empty()).then(|| description.join("\n")),
        "free": free,
    }))
}

/// The stored payload for a card; `listed_on` is the London date of the
/// fetch (for year-less dates); `detail` is [`parse_detail`]'s output when
/// the event page was read.
pub fn raw_event(card: &Value, listed_on: NaiveDate, detail: Option<Value>) -> RawEvent {
    RawEvent {
        source_event_id: card["slug"].as_str().unwrap_or_default().to_string(),
        source_url: card["url"].as_str().map(str::to_string),
        payload: json!({
            "card": card,
            "detail": detail.unwrap_or(Value::Null),
            "listed_on": listed_on.to_string(),
        }),
    }
}

/// One clock time ("7.30pm", "3pm", "11am", "12.30pm").
fn parse_clock(token: &str) -> Option<NaiveTime> {
    let t = token.trim().to_lowercase();
    let (num, pm) = match (t.strip_suffix("pm"), t.strip_suffix("am")) {
        (Some(n), _) => (n.trim_end(), true),
        (_, Some(n)) => (n.trim_end(), false),
        _ => return None,
    };
    let (h, m): (u32, u32) = match num.split_once(['.', ':']) {
        Some((h, m)) if m.len() == 2 => (h.parse().ok()?, m.parse().ok()?),
        Some(_) => return None,
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

/// Days and months written as "23/25/27/28 November" or "30 November / 2
/// December": a bare day takes the next written month.
fn day_months(text: &str) -> Option<Vec<(u32, u32)>> {
    let mut pending: Vec<u32> = Vec::new();
    let mut out = Vec::new();
    for token in text
        .split(|c: char| c.is_whitespace() || c == '/' || c == ',')
        .filter(|t| !t.is_empty())
    {
        if let Some(day) = token.parse::<u32>().ok().filter(|d| (1..=31).contains(d)) {
            pending.push(day);
        } else {
            let month = month_number(token).filter(|_| !pending.is_empty())?;
            out.extend(pending.drain(..).map(|d| (d, month)));
        }
    }
    (pending.is_empty() && !out.is_empty()).then_some(out)
}

/// The dates of year-less days and months in order, the last one no more
/// than [`PAST_DAYS`] before `listed_on`; earlier ones go back a year when
/// their month is later than the next one's.
fn with_years(days: &[(u32, u32)], listed_on: NaiveDate) -> Option<Vec<NaiveDate>> {
    let &(last_day, last_month) = days.last()?;
    let from = listed_on - chrono::Duration::days(PAST_DAYS);
    let mut year = (from.year()..=from.year() + 1)
        .find(|y| NaiveDate::from_ymd_opt(*y, last_month, last_day).is_some_and(|d| d >= from))?;
    let mut out = Vec::with_capacity(days.len());
    let mut next_month = last_month;
    for &(day, month) in days.iter().rev() {
        if month > next_month {
            year -= 1;
        }
        out.push(NaiveDate::from_ymd_opt(year, month, day)?);
        next_month = month;
    }
    out.reverse();
    Some(out)
}

/// When an event happens, from its date line.
#[derive(Debug, PartialEq)]
pub enum When {
    /// These days, at these times (none: all day).
    Days(Vec<NaiveDate>, Vec<NaiveTime>),
    /// Every day of a run, from the first to the last, at these times
    /// (none: one all-day run).
    Run(NaiveDate, NaiveDate, Vec<NaiveTime>),
}

/// Parse a date line ("13 November 7.30pm", "15 November 3pm & 6pm",
/// "23/25/27/28 November 7.30pm", "23 November – 28 November 7.30pm").
pub fn parse_date_line(text: &str, listed_on: NaiveDate) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {text:?}"));
    // The times start at the first word with a digit after the last month.
    let words: Vec<&str> = text.split_whitespace().collect();
    let last_month = words
        .iter()
        .rposition(|w| month_number(w).is_some())
        .ok_or_else(err)?;
    let dates = words[..=last_month].join(" ");
    let times = words[last_month + 1..].join(" ");
    let times = times
        .split(['&', ','])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(parse_clock)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(err)?;
    match dates.split_once(['–', '—']) {
        Some((from, to)) => {
            let to = day_months(to).filter(|d| d.len() == 1).ok_or_else(err)?;
            let from = match from.trim().parse::<u32>() {
                Ok(day) => vec![(day, to[0].1)],
                Err(_) => day_months(from).filter(|d| d.len() == 1).ok_or_else(err)?,
            };
            let dates = with_years(&[from[0], to[0]], listed_on).ok_or_else(err)?;
            if dates[1] < dates[0] {
                return Err(err());
            }
            Ok(When::Run(dates[0], dates[1], times))
        }
        None => {
            let days = day_months(&dates).ok_or_else(err)?;
            Ok(When::Days(
                with_years(&days, listed_on).ok_or_else(err)?,
                times,
            ))
        }
    }
}

fn sessions(days: &[NaiveDate], times: &[NaiveTime]) -> Vec<Session> {
    days.iter()
        .flat_map(|d| {
            let times = if times.is_empty() {
                &[NaiveTime::MIN][..]
            } else {
                times
            };
            times.iter().map(|t| Session {
                starts_at: london_to_utc(d.and_time(*t)),
                ends_at: None,
            })
        })
        .collect()
}

/// Whether an event with this title and subtitle is out of scope (not
/// music, or cancelled).
pub fn skipped(title: &str, subtitle: Option<&str>) -> bool {
    let hay = padded_words(&format!("{title} {}", subtitle.unwrap_or_default()));
    NOT_MUSIC.iter().any(|w| hay.contains(&format!(" {w} ")))
}

/// Normalise a Stone Nest [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let card = &payload["card"];
    let detail = &payload["detail"];
    let text = |key: &str| card[key].as_str().filter(|s| !s.is_empty());
    let title = text("title").ok_or_else(|| SourceError::Parse("card without a title".into()))?;
    let subtitle = text("subtitle");
    if skipped(title, subtitle) {
        return Ok(None);
    }
    if !text("place").is_some_and(|p| p.contains(VENUE_NAME)) {
        return Err(SourceError::Parse(format!(
            "{title:?}: place {:?} is not Stone Nest",
            text("place")
        )));
    }
    let listed_on = payload["listed_on"]
        .as_str()
        .and_then(|d| d.parse::<NaiveDate>().ok())
        .ok_or_else(|| SourceError::Parse("payload without listed_on".into()))?;
    let date = text("date").ok_or_else(|| SourceError::Parse(format!("{title:?}: no date")))?;

    let (days, times, continuous) = match parse_date_line(date, listed_on)? {
        When::Days(days, times) => (days, times, false),
        When::Run(first, last, times) => {
            let continuous = times.is_empty();
            let days = first.iter_days().take_while(|d| *d <= last).collect();
            (days, times, continuous)
        }
    };
    let sessions = sessions(&days, &times);
    let starts_at = sessions
        .first()
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no days in {date:?}")))?
        .starts_at;

    let mut event = NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(title, starts_at, Some(VENUE_NAME)),
        description: clean_description(detail["description"].as_str().or(text("summary"))),
        title: title.to_string(),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at: None,
        all_day: false,
        price: Price {
            is_free: detail["free"].as_bool() == Some(true),
            ..Price::default()
        },
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category: Category::Music,
        tags: crate::music::tags_for(
            "music",
            &[subtitle, text("summary")]
                .into_iter()
                .flatten()
                .map(str::to_string)
                .collect::<Vec<_>>(),
            title,
        )
        .into_iter()
        .map(str::to_string)
        .collect(),
    };
    event.set_sessions(sessions);
    if continuous {
        // One all-day run from the first day to the last, not a session a day.
        event.sessions.clear();
    }
    Ok(Some(event))
}
#[async_trait]
impl Source for StoneNest {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listed_on = london_date(Utc::now());
        let listing_url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let listing = ctx.get_text(&listing_url).await?;
        let cards = parse_listing(&listing, &listing_url).ok_or_else(|| {
            SourceError::Parse(format!("no upcoming events list on {LISTING_PATH}"))
        })?;
        let mut out = Vec::new();
        for (n, card) in cards.iter().enumerate() {
            let slug = card["slug"].as_str().unwrap_or_default();
            let detail = if n >= self.max_details {
                ctx.report_error(format!(
                    "{slug}: over the {} event-page cap",
                    self.max_details
                ));
                None
            } else {
                match card["url"].as_str().map(Url::parse) {
                    Some(Ok(url)) => match ctx.get_text(&url).await {
                        Ok(html) => parse_detail(&html).or_else(|| {
                            ctx.report_error(format!("{slug}: no event heading on its page"));
                            None
                        }),
                        Err(e) => {
                            ctx.report_error(format!("{slug}: {e}"));
                            None
                        }
                    },
                    _ => None,
                }
            };
            out.push(raw_event(card, listed_on, detail));
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only the upcoming events list (not past events). Theatre, dance and comedy \
             shows and cancelled events are left out on purpose; prices are on the \
             ticket site, so only free events have a price.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    const LISTED: (i32, u32, u32) = (2026, 11, 8);

    fn parse(s: &str) -> When {
        parse_date_line(s, d(LISTED.0, LISTED.1, LISTED.2)).unwrap()
    }

    #[test]
    fn clocks() {
        assert_eq!(parse_clock("7.30pm"), Some(t(19, 30)));
        assert_eq!(parse_clock("3pm"), Some(t(15, 0)));
        assert_eq!(parse_clock("11am"), Some(t(11, 0)));
        assert_eq!(parse_clock("12.30pm"), Some(t(12, 30)));
        assert_eq!(parse_clock("12am"), Some(t(0, 0)));
        assert_eq!(parse_clock("7:30 pm"), Some(t(19, 30)));
        assert_eq!(parse_clock("19.30"), None);
        assert_eq!(parse_clock("doors 7pm"), None);
    }

    #[test]
    fn date_lines() {
        assert_eq!(
            parse("13 November 7.30pm"),
            When::Days(vec![d(2026, 11, 13)], vec![t(19, 30)])
        );
        assert_eq!(
            parse("15 November 3pm & 6pm"),
            When::Days(vec![d(2026, 11, 15)], vec![t(15, 0), t(18, 0)])
        );
        assert_eq!(
            parse("23/25/27/28 November 7.30pm"),
            When::Days(
                vec![
                    d(2026, 11, 23),
                    d(2026, 11, 25),
                    d(2026, 11, 27),
                    d(2026, 11, 28)
                ],
                vec![t(19, 30)]
            )
        );
        assert_eq!(
            parse("23 November – 28 November 7.30pm"),
            When::Run(d(2026, 11, 23), d(2026, 11, 28), vec![t(19, 30)])
        );
        assert_eq!(
            parse("30 – 31 December"),
            When::Run(d(2026, 12, 30), d(2026, 12, 31), vec![])
        );
        assert_eq!(
            parse("5 December"),
            When::Days(vec![d(2026, 12, 5)], vec![])
        );
    }

    #[test]
    fn year_less_dates_roll_on_and_back() {
        // Early in the year, next year; a week back, still this one.
        assert_eq!(
            parse("16 January 8pm"),
            When::Days(vec![d(2027, 1, 16)], vec![t(20, 0)])
        );
        assert_eq!(
            parse("2 November 8pm"),
            When::Days(vec![d(2026, 11, 2)], vec![t(20, 0)])
        );
        // Days spanning the new year follow the last one back.
        assert_eq!(
            parse("30 December / 2 January 8pm"),
            When::Days(vec![d(2026, 12, 30), d(2027, 1, 2)], vec![t(20, 0)])
        );
        // A run whose first day is weeks back is still this one.
        assert_eq!(
            parse("1 October – 9 November"),
            When::Run(d(2026, 10, 1), d(2026, 11, 9), vec![])
        );
    }

    #[test]
    fn unrecognised_date_lines_are_errors() {
        for s in [
            "TBC",
            "13 November doors 7pm",
            "November 7.30pm",
            "13/14 7.30pm",
            "28 November – 23 November",
        ] {
            assert!(
                parse_date_line(s, d(LISTED.0, LISTED.1, LISTED.2)).is_err(),
                "{s}"
            );
        }
    }

    fn payload(date: &str, title: &str, subtitle: &str) -> Value {
        json!({
            "card": {
                "slug": "x",
                "url": "https://www.stonenest.org/events/x",
                "date": date,
                "title": title,
                "subtitle": subtitle,
                "summary": "A summary.",
                "place": "Below Stone Nest",
            },
            "detail": null,
            "listed_on": "2026-11-08",
        })
    }

    #[test]
    fn sessions_and_runs() {
        let ev = normalise_payload(&payload("15 November 3pm & 6pm", "Duo", ""))
            .unwrap()
            .unwrap();
        assert_eq!(ev.sessions.len(), 2);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-11-15T15:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2026-11-15T18:00:00+00:00")
        );
        assert!(!ev.all_day);
        assert_eq!(ev.venue_name.as_deref(), Some("Stone Nest"));
        assert_eq!(ev.description.as_deref(), Some("A summary."));

        let ev = normalise_payload(&payload("13 November 7.30pm", "Solo", ""))
            .unwrap()
            .unwrap();
        assert!(ev.sessions.is_empty());
        assert_eq!(ev.ends_at, None);

        let ev = normalise_payload(&payload("23 November – 25 November 7pm", "Run", ""))
            .unwrap()
            .unwrap();
        assert_eq!(ev.sessions.len(), 3);

        let ev = normalise_payload(&payload("23 November – 25 November", "Show", ""))
            .unwrap()
            .unwrap();
        assert!(ev.all_day && ev.sessions.is_empty());
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-11-23T00:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2026-11-25T00:00:00+00:00")
        );

        let ev = normalise_payload(&payload("5 December", "Day", ""))
            .unwrap()
            .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn subtags_from_the_card_and_skips() {
        let ev = normalise_payload(&payload(
            "13 November 7.30pm",
            "Trio",
            "EFG London Jazz Festival",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(ev.tags, ["jazz"]);
        assert_eq!(ev.category, Category::Music);
        for (title, subtitle) in [
            (
                "TOLD-RE-TOLD",
                "MA Dance: Performance, London Contemporary Dance School",
            ),
            ("National Youth Monologues", "Southern Regional Final"),
            ("A Hero of Our Time", "A theatre show"),
            ("Gig (CANCELLED)", ""),
        ] {
            assert!(
                normalise_payload(&payload("13 November 7.30pm", title, subtitle))
                    .unwrap()
                    .is_none(),
                "{title}"
            );
        }
    }

    #[test]
    fn a_place_elsewhere_is_an_error() {
        let mut p = payload("13 November 7.30pm", "Gig", "");
        p["card"]["place"] = json!("Somerset House");
        let err = normalise_payload(&p).unwrap_err().to_string();
        assert!(err.contains("not Stone Nest"), "{err}");
    }
}
