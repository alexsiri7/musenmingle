//! Cafe OTO (Ashwin Street, Dalston, E8) — experimental and improvised
//! music from the site's server-rendered programme (issue #214).
//!
//! * robots.txt (checked 2026-11-08, saved as a fixture): the `User-agent: *`
//!   group disallows only `/admin` and `/blog` and asks for
//!   `Crawl-delay: 10`, which FetchContext honours. A run makes robots.txt,
//!   up to [`MAX_LISTING_PAGES`] listing pages and up to [`MAX_DETAILS`]
//!   residency pages, 10 s apart, so it asks the runner for a longer
//!   `fetch_timeout`.
//! * No JSON-LD on the listing or the event pages, so CSS. `/events/` shows
//!   12 cards (`.activity-container-inner .each-activity`) and links the
//!   next page from `#iscroll_other` (`?page=2`, …; the last page has no
//!   link). A card has a date line (the first `.each-header p`: "Sunday 8
//!   November 2026, 7.30pm", "2–5pm", or a residency's "18–19 November
//!   2026"), an optional promoter or series `span.prefix` ("Upset The Rhythm
//!   presents:"), which leads the title as on the event page, the title
//!   link (`/events/<slug>/`), an image and the prices. A card without a
//!   link ("CLOSED FOR PRIVATE FUNCTION") is not an event.
//! * Times are London wall clock. The site's FAQ (`/info/`) says the
//!   advertised time is the door time, music about 30 minutes later; the
//!   advertised time is stored, as the page shows it, so the scraper check
//!   agrees with the page.
//! * A residency card gives dates only. Its page lists one night per
//!   `.each-activity` ("Wednesday 18 November 2026, 7.30pm", with that
//!   night's prices): those are the event's sessions, and its intro text the
//!   description; a page without nights is a parse error (the layout
//!   changed). Without the page (failed or over the cap, reported) it is an
//!   all-day run from the card's dates. Runs longer than
//!   [`MAX_RANGE_DAYS`] are skipped.
//! * Price: the card's (or the nights') price spans, without the members'
//!   price (linked to `/membership/`): "£22 £20 Advance" → £20–£22, "Free
//!   Event" or "£0 Free Advance" → free, "SOLD OUT" → unknown.
//! * Category: music, tagged `experimental` (the venue's programme is
//!   "creative new music that exists outside of the mainstream", `/info/`;
//!   `crate::music` adds `jazz` and the rest from titles), except a fair
//!   (the Winter Fair), which is an expo. Cancelled events and off-site
//!   shows ("OFFSITE SHOW", "OFFSITE – LCMF 2026:", at places the listing
//!   doesn't name) are skipped, and `qa_scope` says so.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::the_showroom::{MAX_RANGE_DAYS, When, parse_when};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent, Session};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, parse_price, words,
};

pub const KEY: &str = "cafe-oto";
pub const LISTING_PATH: &str = "/events/";
/// At most this many listing pages a run (5 of 12 cards on 2026-11-08).
pub const MAX_LISTING_PAGES: usize = 10;
/// At most this many residency pages a run (10 residencies on 2026-11-08).
pub const MAX_DETAILS: usize = 15;
/// robots.txt's `Crawl-delay`, for the fetch timeout.
const CRAWL_DELAY_SECS: u64 = 10;
const VENUE_NAME: &str = "Cafe OTO";
const VENUE_ADDRESS: &str = "18–22 Ashwin Street, Dalston, London E8 3DL";

pub struct CafeOto {
    base_url: Url,
}

impl CafeOto {
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

/// The visible text of a price block, without the members' price (a link
/// to `/membership/`) and the `<style>` the site puts inside it.
fn price_text(block: ElementRef<'_>) -> String {
    let parts: Vec<&str> = block
        .descendants()
        .filter_map(|node| {
            let text = node.value().as_text()?;
            let hidden = node
                .ancestors()
                .take_while(|a| a.id() != block.id())
                .filter_map(ElementRef::wrap)
                .any(|a| {
                    let el = a.value();
                    matches!(el.name(), "style" | "script")
                        || (el.name() == "a"
                            && el
                                .attr("href")
                                .is_some_and(|h| h.starts_with("/membership")))
                });
            (!hidden).then_some(&**text)
        })
        .collect();
    clean_text(&parts.join(" "))
}

/// One listing page: its event cards and the next page's URL.
#[derive(Debug, Default)]
pub struct ListingPage {
    pub cards: Vec<Value>,
    pub next: Option<Url>,
}

/// The event cards of a listing page fetched from `page_url`, in page
/// order, and the link to the next page (same host, `/events/?page=N`).
pub fn parse_listing(html: &str, page_url: &Url) -> ListingPage {
    let doc = Html::parse_document(html);
    let header_p = selector("div.each-header > p");
    let link = selector("div.each-header > p > a[href]");
    let prefix = selector("span.prefix");
    let img = selector("div.each-image img[src]");
    let price = selector("div.each-price");
    let mut cards = Vec::new();
    for card in doc.select(&selector(
        "div.activity-container-inner > div.each-activity",
    )) {
        let Some(a) = card.select(&link).next() else {
            continue;
        };
        let Some(url) = a
            .value()
            .attr("href")
            .filter(|h| !h.trim().is_empty())
            .and_then(|h| page_url.join(h).ok())
            .filter(|u| u.host_str() == page_url.host_str())
        else {
            continue;
        };
        let Some(slug) = url
            .path()
            .strip_prefix(LISTING_PATH)
            .map(|s| s.trim_end_matches('/'))
            .filter(|s| !s.is_empty() && !s.contains('/'))
        else {
            continue;
        };
        cards.push(json!({
            "slug": slug,
            "url": url.as_str(),
            "date_text": card.select(&header_p).next().map(element_text),
            "prefix": card.select(&prefix).next().map(element_text).filter(|t| !t.is_empty()),
            "title": element_text(a),
            "image_url": card
                .select(&img)
                .next()
                .and_then(|i| i.value().attr("src"))
                .and_then(|s| page_url.join(s).ok())
                .map(|u| u.to_string()),
            "price_text": card.select(&price).next().map(price_text),
        }));
    }
    let next = doc
        .select(&selector("#iscroll_other a[href]"))
        .next()
        .and_then(|a| a.value().attr("href"))
        .and_then(|h| page_url.join(h).ok())
        .filter(|u| u.host_str() == page_url.host_str() && u.path() == LISTING_PATH);
    ListingPage { cards, next }
}

/// What a residency page adds: each night's date line and price text, and
/// the intro text.
pub fn parse_detail(html: &str) -> Value {
    let doc = Html::parse_document(html);
    let first_link = selector("div.each-header > p > a");
    let meta = selector("div.event-detail-meta");
    let nights: Vec<Value> = doc
        .select(&selector(
            "div.activity-container-inner > div.each-activity",
        ))
        .filter_map(|night| {
            let date_text = night.select(&first_link).next().map(element_text)?;
            Some(json!({
                "date_text": date_text,
                "price_text": night.select(&meta).next().map(price_text),
            }))
        })
        .collect();
    let intro: Vec<String> = doc
        .select(&selector("div.event-detail-intro-container"))
        .filter(|e| !element_text(*e).is_empty())
        .map(|e| e.inner_html())
        .collect();
    json!({
        "nights": nights,
        "description": (!intro.is_empty()).then(|| intro.join("\n")),
    })
}

/// The stored payload for a card; `listed_on` is the London date of the
/// fetch (for year-less dates); `detail` is [`parse_detail`]'s output for a
/// residency whose page was read.
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

/// Whether a card's date line is a run of days without a time (a
/// residency, whose page has the nights).
pub fn is_residency(date_text: &str, listed_on: NaiveDate) -> bool {
    matches!(parse_when(date_text, listed_on), Ok(When::Days(_, Some(_))))
}

fn london_midnight(d: NaiveDate) -> DateTime<Utc> {
    london_to_utc(d.and_time(NaiveTime::MIN))
}

fn timed(day: NaiveDate, from: NaiveTime, to: Option<NaiveTime>) -> Session {
    let starts_at = london_to_utc(day.and_time(from));
    Session {
        starts_at,
        ends_at: to
            .map(|t| london_to_utc(day.and_time(t)))
            .filter(|e| *e > starts_at),
    }
}

/// Normalise a payload built by [`raw_event`].
pub fn normalise_payload(p: &Value) -> Result<Option<NewEvent>, SourceError> {
    let card = &p["card"];
    let text = |v: &Value| v.as_str().map(clean_text).unwrap_or_default();
    let main_title = text(&card["title"]);
    if main_title.is_empty() {
        return Err(SourceError::Parse("card without a title".into()));
    }
    let prefix = text(&card["prefix"]);
    let title = if prefix.is_empty() {
        main_title
    } else {
        format!("{prefix} {main_title}")
    };
    let price_text = text(&card["price_text"]);
    let has_word = |s: &str, w: &str| words(s).iter().any(|x| x == w);
    if has_word(&price_text, "cancelled") || has_word(&prefix, "offsite") {
        return Ok(None);
    }
    let listed_on = p["listed_on"]
        .as_str()
        .and_then(|s| s.parse::<NaiveDate>().ok())
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no listed_on")))?;
    let date_text = text(&card["date_text"]);
    let detail = &p["detail"];

    let mut sessions = Vec::new();
    let mut prices = vec![price_text];
    let (starts_at, ends_at, all_day) = match parse_when(&date_text, listed_on)? {
        When::Timed(day, from, to) => {
            let s = timed(day, from, to);
            (s.starts_at, s.ends_at, false)
        }
        When::Days(first, last) => {
            if last.is_some_and(|l| (l - first).num_days() > MAX_RANGE_DAYS) {
                return Ok(None);
            }
            if !detail.is_null() {
                let nights = detail["nights"].as_array().filter(|n| !n.is_empty());
                let Some(nights) = nights else {
                    return Err(SourceError::Parse(format!(
                        "{title:?}: residency page had no nights"
                    )));
                };
                prices.clear();
                for night in nights {
                    let line = text(&night["date_text"]);
                    let When::Timed(day, from, to) = parse_when(&line, listed_on)? else {
                        return Err(SourceError::Parse(format!(
                            "{title:?}: night without a time {line:?}"
                        )));
                    };
                    sessions.push(timed(day, from, to));
                    prices.push(text(&night["price_text"]));
                }
            }
            (london_midnight(first), last.map(london_midnight), true)
        }
    };
    let category = if has_word(&title, "fair") {
        Category::Expo
    } else {
        Category::Music
    };

    let mut ev = NewEvent {
        dedupe_key: String::new(),
        description: clean_description(detail["description"].as_str()),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        sessions: Vec::new(),
        price: parse_price(&prices.join(" ")),
        url: card["url"].as_str().map(str::to_string),
        image_url: card["image_url"].as_str().map(str::to_string),
        tags: if category == Category::Music {
            vec!["experimental".to_string()]
        } else {
            Vec::new()
        },
        category,
        title,
    };
    ev.set_sessions(sessions);
    ev.dedupe_key = dedupe_key(&ev.title, ev.starts_at, Some(VENUE_NAME));
    Ok(Some(ev))
}

#[async_trait]
impl Source for CafeOto {
    fn key(&self) -> &str {
        KEY
    }

    fn fetch_timeout(&self) -> Option<Duration> {
        // The Crawl-delay per request (robots.txt, listing pages, residency
        // pages), plus a margin.
        let requests = 1 + MAX_LISTING_PAGES + MAX_DETAILS;
        Some(Duration::from_secs(CRAWL_DELAY_SECS * requests as u64 + 60))
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listed_on = london_date(Utc::now());
        let mut page_url = Some(
            self.base_url
                .join(LISTING_PATH)
                .map_err(|e| SourceError::Config(e.to_string()))?,
        );
        let mut cards: Vec<Value> = Vec::new();
        for n in 0..MAX_LISTING_PAGES {
            let Some(url) = page_url.take() else { break };
            let html = match ctx.get_text(&url).await {
                Ok(html) => html,
                Err(e) if n == 0 => return Err(e.into()),
                Err(e) => {
                    ctx.report_error(format!("listing page {}: {e}", n + 1));
                    break;
                }
            };
            let page = parse_listing(&html, &url);
            if n == 0 && page.cards.is_empty() {
                return Err(SourceError::Parse(format!(
                    "no event cards on {LISTING_PATH}"
                )));
            }
            for card in page.cards {
                if !cards.iter().any(|c| c["slug"] == card["slug"]) {
                    cards.push(card);
                }
            }
            page_url = page.next;
        }
        if page_url.is_some() {
            ctx.report_error(format!(
                "more than {MAX_LISTING_PAGES} listing pages; the rest are not read"
            ));
        }

        let mut details = 0;
        let mut out = Vec::new();
        for card in &cards {
            let residency = card["date_text"]
                .as_str()
                .is_some_and(|d| is_residency(d, listed_on));
            let detail = match card["url"].as_str().map(Url::parse) {
                Some(Ok(url)) if residency => {
                    let slug = card["slug"].as_str().unwrap_or_default();
                    details += 1;
                    if details > MAX_DETAILS {
                        ctx.report_error(format!(
                            "{slug}: over the {MAX_DETAILS} residency-page cap"
                        ));
                        None
                    } else {
                        match ctx.get_text(&url).await {
                            Ok(html) => Some(parse_detail(&html)),
                            Err(e) => {
                                ctx.report_error(format!("{slug}: {e}"));
                                None
                            }
                        }
                    }
                }
                _ => None,
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
            "Cancelled events, off-site shows (titled \"OFFSITE\", held somewhere \
             other than Cafe OTO) and private functions are left out on purpose. \
             Times are the advertised door times.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn payload(date: &str, prefix: Option<&str>, title: &str, price: &str) -> Value {
        let card = json!({
            "slug": "x",
            "url": "https://www.cafeoto.co.uk/events/x/",
            "date_text": date,
            "prefix": prefix,
            "title": title,
            "image_url": null,
            "price_text": price,
        });
        raw_event(&card, d(2026, 11, 8), None).payload
    }

    fn norm(date: &str, prefix: Option<&str>, title: &str, price: &str) -> Option<NewEvent> {
        normalise_payload(&payload(date, prefix, title, price)).unwrap()
    }

    #[test]
    fn a_timed_card_is_london_wall_clock() {
        let ev = norm("Sunday 8 November 2026, 2–5pm", None, "A Gig", "£18").unwrap();
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-11-08T14:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-11-08T17:00:00+00:00")
        );
        assert!(!ev.all_day);
        assert_eq!(ev.category, Category::Music);
        assert_eq!(ev.tags, ["experimental"]);
    }

    #[test]
    fn the_prefix_leads_the_title() {
        let ev = norm(
            "Monday 9 November 2026, 7.30pm",
            Some("Upset The Rhythm presents:"),
            "Wendy Eisenberg",
            "",
        )
        .unwrap();
        assert_eq!(ev.title, "Upset The Rhythm presents: Wendy Eisenberg");
    }

    #[test]
    fn cancelled_and_offsite_are_skipped() {
        let date = "Sunday 29 November 2026, 7.30pm";
        assert!(norm(date, None, "A Gig", "cancelled").is_none());
        assert!(norm(date, Some("OFFSITE SHOW"), "A Gig", "£12").is_none());
        assert!(norm(date, Some("OFFSITE – LCMF 2026:"), "A Gig", "£27").is_none());
    }

    #[test]
    fn a_fair_is_an_expo_and_free() {
        let ev = norm(
            "Sunday 6 December 2026, 11am–6pm",
            None,
            "CAFE OTO WINTER FAIR",
            "Free Event",
        )
        .unwrap();
        assert_eq!(ev.category, Category::Expo);
        assert!(ev.tags.is_empty());
        assert!(ev.price.is_free);
    }

    #[test]
    fn a_residency_without_its_page_is_an_all_day_run() {
        let ev = norm(
            "18–19 November 2026",
            None,
            "Grouper – Two-Day Residency",
            "",
        )
        .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at, london_midnight(d(2026, 11, 18)));
        assert_eq!(ev.ends_at, Some(london_midnight(d(2026, 11, 19))));
        assert!(ev.sessions.is_empty());
    }

    #[test]
    fn a_residency_with_its_page_has_one_session_a_night() {
        let mut p = payload("18–19 November 2026", None, "Grouper", "");
        p["detail"] = json!({"nights": [
            {"date_text": "Wednesday 18 November 2026, 7.30pm", "price_text": "£20"},
            {"date_text": "Thursday 19 November 2026, 7.30pm", "price_text": "£22 £18 Advance"},
        ]});
        let ev = normalise_payload(&p).unwrap().unwrap();
        assert!(!ev.all_day);
        assert_eq!(ev.sessions.len(), 2);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-11-18T19:30:00+00:00");
        assert_eq!(ev.price.min.unwrap().to_string(), "18");
        assert_eq!(ev.price.max.unwrap().to_string(), "22");

        p["detail"]["nights"] = json!([{"date_text": "18 November 2026", "price_text": ""}]);
        let err = normalise_payload(&p).unwrap_err().to_string();
        assert!(err.contains("night without a time"), "{err}");
    }

    #[test]
    fn residencies_are_runs_of_days_without_a_time() {
        let on = d(2026, 11, 8);
        assert!(is_residency("9–12 December 2026", on));
        assert!(!is_residency("Saturday 12 December 2026, 7pm", on));
        assert!(!is_residency("nonsense", on));
    }

    #[test]
    fn the_qa_scope_names_the_skips() {
        let scope = CafeOto::new(Url::parse("https://x.test/").unwrap())
            .qa_scope()
            .unwrap();
        for word in ["Cancelled", "OFFSITE", "private functions", "door"] {
            assert!(scope.contains(word), "{word}");
        }
    }
}
