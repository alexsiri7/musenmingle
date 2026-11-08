//! Wilton's Music Hall (Graces Alley, Whitechapel, E1) — its music and
//! opera, from the site's server-rendered "What's On" pages and event pages
//! (issue #222).
//!
//! * robots.txt (checked 2026-11-08, saved as a fixture) is Yoast's
//!   `User-agent: *` / `Disallow:` with nothing disallowed.
//! * No JSON-LD `Event` (only Yoast's page graph), so CSS. `/whats-on/`
//!   filters by the venue's own genre (`?category=<genre>`: Cabaret,
//!   Comedy, Family, Film, Heritage Tour, Magic, Music, Music Hall, Musical
//!   Theatre, New Writing, Opera, Theatre), so only the [`GENRES`] views
//!   are read: the rest of the programme is left out by the venue's label,
//!   not by guessing from titles. A view lists twelve cards a page
//!   (`ul.WhatsOnList li.WhatsonItem`: the event page link, title and
//!   image) and its pager links the next (`&event-page=2`; at most
//!   [`MAX_PAGES`] a genre). An empty view has no list at all, but still
//!   its genre filter, which tells it from a page that isn't the listing.
//! * Each card's event page (at most [`MAX_DETAILS`] a run) gives the
//!   performances: its "Tickets & Times" table has one row per
//!   performance, "Tue 23 Feb 2027" | "7 pm" (the year left out in the
//!   current one: "Mon 9 Nov", see `year_less_date`), in London wall clock.
//!   A run is one event with a session per performance. The block under
//!   the heading ("£12.50 – £23.00 full price | £10.00 – £20.50
//!   concession", "Running time: 1 hours 50 minutes, including interval")
//!   gives the price and each performance's end. Without the page (failed
//!   or over the cap, both reported) the event is left out: the card's
//!   date line has no year and says "Various Times" for runs.
//! * Music subtags come from the venue's genre and the title through
//!   `crate::music` (Opera → classical, "City of London Sinfonia" →
//!   classical, "Jazz Noël" → jazz).

use async_trait::async_trait;
use chrono::{Duration, NaiveDate, NaiveTime, Utc, Weekday};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent, Session};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, month_number,
    parse_price, year_less_date,
};

pub const KEY: &str = "wiltons-music-hall";
pub const LISTING_PATH: &str = "/whats-on/";
const EVENT_PREFIX: &str = "/whats-on/";
/// The venue's genres read (`?category=` value, label), in that order.
pub const GENRES: &[(&str, &str)] = &[("music", "Music"), ("opera", "Opera")];
/// At most this many listing pages a genre (one each on 2026-11-08).
pub const MAX_PAGES: u32 = 5;
/// At most this many event pages a run (12 on 2026-11-08).
pub const MAX_DETAILS: usize = 40;
const VENUE_NAME: &str = "Wilton's Music Hall";
const VENUE_ADDRESS: &str = "Graces Alley, London E1 8JB";

pub struct WiltonsMusicHall {
    base_url: Url,
    max_details: usize,
}

impl WiltonsMusicHall {
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

    /// Page `page` of the listing filtered to `genre`.
    fn listing_url(&self, genre: &str, page: u32) -> Result<Url, SourceError> {
        let mut url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        url.query_pairs_mut().append_pair("category", genre);
        if page > 1 {
            url.query_pairs_mut()
                .append_pair("event-page", &page.to_string());
        }
        Ok(url)
    }

    /// The cards of every genre view, in [`GENRES`] order; an event listed
    /// under more than one genre keeps the first.
    async fn genre_cards(&self, ctx: &FetchContext) -> Result<Vec<Value>, SourceError> {
        let mut cards: Vec<Value> = Vec::new();
        for (g, (genre, label)) in GENRES.iter().enumerate() {
            for page in 1..=MAX_PAGES {
                let url = self.listing_url(genre, page)?;
                let listing = match ctx.get_text(&url).await {
                    Ok(html) => parse_listing(&html, &url, label, page).ok_or_else(|| {
                        SourceError::Parse(format!("no What's On listing at {}", url.path()))
                    }),
                    Err(e) => Err(e.into()),
                };
                let listing = match listing {
                    Ok(listing) => listing,
                    // The first listing page failing fails the run; a later
                    // one is a soft error and the cards found so far are kept.
                    Err(e) if g > 0 || page > 1 => {
                        ctx.report_error(format!("{genre} listing page {page}: {e}"));
                        break;
                    }
                    Err(e) => return Err(e),
                };
                for card in listing.cards {
                    if !cards.iter().any(|c| c["slug"] == card["slug"]) {
                        cards.push(card);
                    }
                }
                if !listing.has_next {
                    break;
                }
                if page == MAX_PAGES {
                    ctx.report_error(format!("{genre}: over the {MAX_PAGES}-page cap"));
                }
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

/// One listing page.
#[derive(Debug)]
pub struct ListingPage {
    pub cards: Vec<Value>,
    /// Whether its pager links page `page + 1`.
    pub has_next: bool,
}

/// The cards on page `page` of the listing fetched from `page_url` and
/// filtered to the genre labelled `label`, one per event page; `None` when
/// it is not the What's On page at all (not the same as an empty view).
pub fn parse_listing(html: &str, page_url: &Url, label: &str, page: u32) -> Option<ListingPage> {
    let doc = Html::parse_document(html);
    doc.select(&selector(".whatsoncategory")).next()?;
    let mut cards: Vec<Value> = Vec::new();
    for card in doc.select(&selector("ul.WhatsOnList li.WhatsonItem")) {
        let Some(slug) = card
            .select(&selector(
                ".WhatsonItem_img a[href], .WhatsonItemTitle a[href]",
            ))
            .filter_map(|a| page_url.join(a.value().attr("href")?.trim()).ok())
            .find_map(|u| {
                let slug = u.path().strip_prefix(EVENT_PREFIX)?.trim_matches('/');
                (!slug.is_empty() && !slug.contains('/')).then(|| slug.to_string())
            })
        else {
            continue;
        };
        if cards.iter().any(|c| c["slug"] == slug.as_str()) {
            continue;
        }
        let Ok(url) = page_url.join(&format!("{EVENT_PREFIX}{slug}/")) else {
            continue;
        };
        let image = card
            .select(&selector(".whatson-event-listing-img-area img[src]"))
            .find_map(|i| page_url.join(i.value().attr("src")?.trim()).ok());
        cards.push(json!({
            "slug": slug,
            "url": url.as_str(),
            "title": card.select(&selector(".WhatsonItemTitle h3 a")).next().map(text_of),
            "genre": label,
            "image_url": image.map(|u| u.to_string()),
        }));
    }
    let next = (page + 1).to_string();
    let has_next = doc
        .select(&selector(".whatson-event-listing-pager a[href]"))
        .any(|a| text_of(a) == next);
    Some(ListingPage { cards, has_next })
}

/// What an event page adds to its card: the "Tickets & Times" rows as
/// `[date, time]`, the price and running-time block under the heading
/// (`info`) and the description. `None` when it is not an event page.
pub fn parse_detail(html: &str) -> Option<Value> {
    let doc = Html::parse_document(html);
    doc.select(&selector(".EventDetailHeading_Title h1"))
        .next()?;
    // The table is on the page twice (mobile and desktop layouts).
    let mut performances: Vec<Value> = Vec::new();
    for row in doc.select(&selector("table.whatson-event-details-list tbody tr")) {
        let cells: Vec<String> = row.select(&selector("td")).take(2).map(text_of).collect();
        let [date, time] = &cells[..] else {
            continue;
        };
        let p = json!([date, time]);
        if !date.is_empty() && !performances.contains(&p) {
            performances.push(p);
        }
    }
    let info = doc
        .select(&selector(".whatson-single-page-date-time"))
        .next()
        .map(text_of)
        .filter(|s| !s.is_empty());
    let description = doc
        .select(&selector(
            ".EventDescriptionLeft .whatson-event-details-content",
        ))
        .next()
        .map(|e| e.inner_html());
    Some(json!({
        "performances": performances,
        "info": info,
        "description": description,
    }))
}

/// The stored payload for a card; `listed_on` is the London date of the
/// fetch; `detail` is [`parse_detail`]'s output.
pub fn raw_event(card: &Value, listed_on: NaiveDate, detail: Value) -> RawEvent {
    RawEvent {
        source_event_id: card["slug"].as_str().unwrap_or_default().to_string(),
        source_url: card["url"].as_str().map(str::to_string),
        payload: json!({
            "card": card,
            "detail": detail,
            "listed_on": listed_on.to_string(),
        }),
    }
}

/// A performance's date: "Tue 23 Feb 2027", or "Mon 9 Nov" in the current
/// year, placed by [`year_less_date`] from `listed_on`.
pub fn parse_performance_date(text: &str, listed_on: NaiveDate) -> Option<NaiveDate> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (weekday, day, month, year) = match words[..] {
        [w, d, m] => (w, d, m, None),
        [w, d, m, y] => (w, d, m, Some(y.parse().ok()?)),
        _ => return None,
    };
    let weekday: Weekday = weekday.parse().ok()?;
    let (day, month) = (day.parse().ok()?, month_number(month)?);
    match year {
        Some(y) => NaiveDate::from_ymd_opt(y, month, day),
        None => year_less_date(day, month, Some(weekday), listed_on),
    }
}

/// A performance's time: "7 pm", "2:30 pm".
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

/// The running time in minutes from "Running time: 2 hours 20 minutes,
/// including interval" or "Running time: 75 minutes, no interval".
pub fn running_minutes(info: &str) -> Option<i64> {
    let lower = info.to_lowercase();
    let (_, rest) = lower.split_once("running time")?;
    let words: Vec<&str> = rest
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut total = 0;
    for pair in words.chunks(2) {
        let [n, unit] = pair else { break };
        let Ok(n) = n.parse::<i64>() else { break };
        if unit.starts_with("hour") {
            total += n * 60;
        } else if unit.starts_with("min") {
            total += n;
        } else {
            break;
        }
    }
    (total > 0).then_some(total)
}

/// Normalise a Wilton's Music Hall [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let card = &payload["card"];
    let detail = &payload["detail"];
    let text = |key: &str| card[key].as_str().filter(|s| !s.is_empty());
    let title = text("title").ok_or_else(|| SourceError::Parse("card without a title".into()))?;
    let genre = text("genre").unwrap_or_default();
    let listed_on = payload["listed_on"]
        .as_str()
        .and_then(|d| d.parse::<NaiveDate>().ok())
        .ok_or_else(|| SourceError::Parse("payload without listed_on".into()))?;
    if !detail.is_object() {
        return Err(SourceError::Parse(format!("{title:?}: no event page")));
    }
    let info = detail["info"].as_str().unwrap_or_default();
    let running = running_minutes(info).map(Duration::minutes);
    let mut sessions = Vec::new();
    for p in detail["performances"].as_array().into_iter().flatten() {
        let (date, time) = (p[0].as_str().unwrap_or_default(), p[1].as_str());
        let day = parse_performance_date(date, listed_on)
            .ok_or_else(|| SourceError::Parse(format!("{title:?}: performance date {date:?}")))?;
        let session = match time.and_then(parse_time) {
            Some(t) => {
                let starts_at = london_to_utc(day.and_time(t));
                Session {
                    starts_at,
                    ends_at: running.map(|r| starts_at + r),
                }
            }
            None => Session {
                starts_at: london_to_utc(day.and_time(NaiveTime::MIN)),
                ends_at: None,
            },
        };
        sessions.push(session);
    }
    let Some(first) = sessions.iter().min().copied() else {
        return Err(SourceError::Parse(format!(
            "{title:?}: no performances on its page"
        )));
    };
    let tags = crate::music::tags_for("music", &[genre.to_string()], title)
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut event = NewEvent {
        sessions: Vec::new(),
        dedupe_key: String::new(),
        description: clean_description(detail["description"].as_str()),
        title: title.to_string(),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at: first.starts_at,
        ends_at: first.ends_at,
        all_day: false,
        price: parse_price(info),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category: Category::Music,
        tags,
    };
    event.set_sessions(sessions);
    event.dedupe_key = dedupe_key(title, event.starts_at, Some(VENUE_NAME));
    Ok(Some(event))
}

#[async_trait]
impl Source for WiltonsMusicHall {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listed_on = london_date(Utc::now());
        let cards = self.genre_cards(ctx).await?;
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
                    Some(detail) => out.push(raw_event(card, listed_on, detail)),
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
            "Only the venue's own Music and Opera genres (its What's On filter). \
             Cabaret, Comedy, Family, Film, Heritage Tour, Magic, Music Hall, Musical \
             Theatre, New Writing and Theatre events, and room hire, are left out on \
             purpose.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("7 pm"), Some(t(19, 0)));
        assert_eq!(parse_time("2:30 pm"), Some(t(14, 30)));
        assert_eq!(parse_time("12 pm"), Some(t(12, 0)));
        assert_eq!(parse_time("11am"), Some(t(11, 0)));
        assert_eq!(parse_time("Various Times"), None);
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("13 pm"), None);
    }

    #[test]
    fn performance_dates_with_and_without_a_year() {
        let today = d(2026, 11, 8);
        assert_eq!(
            parse_performance_date("Tue 23 Feb 2027", today),
            Some(d(2027, 2, 23))
        );
        assert_eq!(
            parse_performance_date("Mon 9 Nov", today),
            Some(d(2026, 11, 9))
        );
        // A year-less date in January is next year's.
        assert_eq!(
            parse_performance_date("Thu 14 Jan", today),
            Some(d(2027, 1, 14))
        );
        assert_eq!(parse_performance_date("9 Nov", today), None);
        assert_eq!(parse_performance_date("Various Times", today), None);
    }

    #[test]
    fn running_times() {
        assert_eq!(
            running_minutes("£10 | Running time: 2 hours 20 minutes, including interval"),
            Some(140)
        );
        assert_eq!(
            running_minutes("Running time: 1 hours 50 minutes, including interval"),
            Some(110)
        );
        assert_eq!(
            running_minutes("Running time: 75 minutes, no interval"),
            Some(75)
        );
        assert_eq!(
            running_minutes("Running time: 2 hours, including interval"),
            Some(120)
        );
        assert_eq!(running_minutes("Running time: TBC"), None);
        assert_eq!(running_minutes("£12 full price"), None);
    }

    fn payload(genre: &str, title: &str, detail: Value) -> Value {
        json!({
            "card": {
                "slug": "x",
                "url": "https://wiltons.org.uk/whats-on/x/",
                "title": title,
                "genre": genre,
            },
            "detail": detail,
            "listed_on": "2026-11-08",
        })
    }

    #[test]
    fn a_run_is_one_event_with_a_session_per_performance() {
        let detail = json!({
            "performances": [["Mon 23 Nov", "7 pm"], ["Sat 28 Nov", "2:30 pm"], ["Sat 28 Nov", "7 pm"]],
            "info": "£13.50 – £28.00 full price | £11.00 – £25.50 concession Running time: 2 hours 50 minutes",
        });
        let ev = normalise_payload(&payload("Opera", "The Marriage of Figaro", detail))
            .unwrap()
            .unwrap();
        assert_eq!(ev.category, Category::Music);
        assert_eq!(ev.tags, ["classical"]);
        assert!(!ev.all_day);
        assert_eq!(ev.sessions.len(), 3);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-11-23T19:00:00+00:00");
        assert_eq!(
            ev.sessions[0].ends_at.unwrap().to_rfc3339(),
            "2026-11-23T21:50:00+00:00"
        );
        assert_eq!(
            ev.ends_at.unwrap().to_rfc3339(),
            "2026-11-28T21:50:00+00:00"
        );
        assert_eq!(ev.price.min.unwrap().to_string(), "11.00");
        assert_eq!(ev.price.max.unwrap().to_string(), "28.00");
    }

    #[test]
    fn a_performance_without_a_time_is_all_day() {
        let detail = json!({"performances": [["Wed 9 Dec 2026", ""]], "info": null});
        let ev = normalise_payload(&payload("Music", "Songs", detail))
            .unwrap()
            .unwrap();
        assert!(ev.all_day);
        assert!(ev.tags.is_empty());
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-12-09T00:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn a_missing_page_unreadable_date_or_no_performances_is_an_error() {
        let err = |detail: Value| {
            normalise_payload(&payload("Music", "Jazz Noël", detail))
                .unwrap_err()
                .to_string()
        };
        assert!(err(Value::Null).contains("no event page"));
        assert!(
            err(json!({"performances": [["TBC", "7 pm"]]})).contains("performance date \"TBC\"")
        );
        assert!(err(json!({"performances": []})).contains("no performances"));
    }
}
