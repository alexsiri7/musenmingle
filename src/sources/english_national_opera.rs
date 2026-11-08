//! English National Opera — its productions and concerts at the London
//! Coliseum (St Martin's Lane, WC2), from the site's server-rendered
//! "What's On" page and event pages (issue #223).
//!
//! * robots.txt (checked 2026-11-08) is a 404: no rules.
//! * `/whats-on/` lists every upcoming event on one page (12 on
//!   2026-11-08, no pager), as cards in `#whats-on-listing` linking
//!   `/events/<slug>/`. Its only JSON-LD is Yoast's page graph, so CSS; the
//!   listing gives only the links.
//! * Each event page (at most [`MAX_DETAILS`] a run) has `Event` JSON-LD,
//!   one node per performance, but it is not valid JSON (raw newlines and
//!   unescaped quotes inside strings), so it can't be read. The same
//!   performances are in the page's booking calendar data,
//!   `<script id="json-data">`: valid JSON, one entry per performance with
//!   its London wall-clock start (`raw_date`, "2026-11-10 19:30:00") and
//!   booking link. A production is one event with a session per
//!   performance (#207). The header gives the title, the composer credit
//!   ENO prints over its operas (`.surtitle`: "Puccini") and the venue; the
//!   sidebar the running time ("2hrs 15mins", each session's end) and the
//!   ticket price ("Tickets from £15").
//! * Skipped: events away from the Coliseum (ENO also stages in Greater
//!   Manchester), pages with no performances (schools events booked by
//!   form) and programme pages whose performances all belong to other
//!   events' pages (the Relaxed Programme, see [`drop_repeats`]).
//! * Everything is music; an event with a composer credit is an opera, so
//!   `classical` (through `crate::music`), and title keywords add the rest.

use async_trait::async_trait;
use chrono::Duration;
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent, Session};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, parse_london_wall_clock, parse_price,
};

pub const KEY: &str = "english-national-opera";
pub const LISTING_PATH: &str = "/whats-on/";
const EVENT_PREFIX: &str = "/events/";
/// At most this many event pages a run (11 on 2026-11-08).
pub const MAX_DETAILS: usize = 40;
const VENUE_NAME: &str = "London Coliseum";
const VENUE_ADDRESS: &str = "St Martin's Lane, London WC2N 4ES";

pub struct EnglishNationalOpera {
    base_url: Url,
    max_details: usize,
}

impl EnglishNationalOpera {
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

fn text_of(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// The slugs of the events the listing links, in page order; `None` when
/// it is not the What's On page.
pub fn parse_listing(html: &str, page_url: &Url) -> Option<Vec<String>> {
    let doc = Html::parse_document(html);
    let listing = doc.select(&selector("#whats-on-listing")).next()?;
    let mut slugs: Vec<String> = Vec::new();
    for a in listing.select(&selector("a.card__fill-link[href]")) {
        let Some(Ok(url)) = a.value().attr("href").map(|h| page_url.join(h.trim())) else {
            continue;
        };
        let Some(slug) = url.path().strip_prefix(EVENT_PREFIX) else {
            continue;
        };
        let slug = slug.trim_matches('/');
        if !slug.is_empty() && !slug.contains('/') && !slugs.iter().any(|s| s == slug) {
            slugs.push(slug.to_string());
        }
    }
    Some(slugs)
}

/// What an event page says: title, composer credit, venue, performances
/// (`{"starts", "booking"}`), running time, ticket line, description and
/// image. `None` when it is not an event page.
pub fn parse_detail(html: &str) -> Option<Value> {
    let doc = Html::parse_document(html);
    let header = doc.select(&selector("header.page-header--event")).next()?;
    let title = header
        .select(&selector("h1.page-header__heading"))
        .next()
        .map(text_of)?;
    let header_text = |s: &str| {
        header
            .select(&selector(s))
            .next()
            .map(text_of)
            .filter(|t| !t.is_empty())
    };
    let performances: Vec<Value> = doc
        .select(&selector("script#json-data"))
        .next()
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s.text().collect::<String>()).ok())
        .unwrap_or_default()
        .iter()
        .map(|p| json!({"starts": p["raw_date"], "booking": p["url"]}))
        .collect();
    let detail = |heading: &str| {
        doc.select(&selector("aside.event-details li.post-detail"))
            .find(|li| {
                li.select(&selector(".post-detail__heading"))
                    .next()
                    .is_some_and(|h| text_of(h).eq_ignore_ascii_case(heading))
            })
            .map(|li| {
                li.children()
                    .filter_map(ElementRef::wrap)
                    .filter(|c| !c.value().classes().any(|c| c == "post-detail__heading"))
                    .map(text_of)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|t| !t.is_empty())
    };
    let description: String = doc
        .select(&selector(
            "section.main-content .main-content__intro, section.main-content .main-content__text",
        ))
        .map(|e| e.inner_html())
        .collect();
    let image = doc
        .select(&selector(r#"meta[property="og:image"][content]"#))
        .next()
        .and_then(|m| m.value().attr("content"));
    Some(json!({
        "title": title,
        "composer": header_text(".surtitle"),
        "venue": header_text(".event-title__venue-location"),
        "performances": performances,
        "running_time": detail("Running time"),
        "tickets": detail("Tickets"),
        "description": (!description.is_empty()).then_some(description),
        "image_url": image,
    }))
}

/// The stored payload for the event page at `url`; `detail` is
/// [`parse_detail`]'s output.
pub fn raw_event(slug: &str, url: &Url, detail: Value) -> RawEvent {
    RawEvent {
        source_event_id: slug.to_string(),
        source_url: Some(url.to_string()),
        payload: json!({"url": url.as_str(), "detail": detail}),
    }
}

/// A performance's booking id: the last segment of its booking link
/// ("//booking.eno.org/book-online/76801/" → "76801").
fn booking_id(performance: &Value) -> Option<&str> {
    let url = performance["booking"].as_str()?.trim_end_matches('/');
    url.rsplit('/').next().filter(|id| !id.is_empty())
}

fn booking_ids(raw: &RawEvent) -> Vec<&str> {
    raw.payload["detail"]["performances"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(booking_id)
        .collect()
}

/// Drops ENO's programme pages: the Relaxed Programme lists one performance
/// each of other productions, with the same booking links, which would
/// otherwise repeat as an event of its own. Such a page's performances are
/// all on other pages, but no one other page has them all; a production
/// whose only performance is also in a programme (a subset of that page)
/// is kept.
pub fn drop_repeats(raws: Vec<RawEvent>) -> Vec<RawEvent> {
    let ids: Vec<Vec<&str>> = raws.iter().map(booking_ids).collect();
    let others = |i: usize| ids.iter().enumerate().filter(move |(j, _)| *j != i);
    let repeats: Vec<bool> = ids
        .iter()
        .enumerate()
        .map(|(i, own)| {
            !own.is_empty()
                && own
                    .iter()
                    .all(|id| others(i).any(|(_, other)| other.contains(id)))
                && !others(i).any(|(_, other)| own.iter().all(|id| other.contains(id)))
        })
        .collect();
    raws.into_iter()
        .zip(repeats)
        .filter_map(|(raw, repeat)| (!repeat).then_some(raw))
        .collect()
}

/// The minutes of a running time that opens with a duration: "2hrs
/// 15mins There is one interval.", "1hr 50mins", "2 hours".
pub fn running_minutes(text: &str) -> Option<i64> {
    let lower = text.to_lowercase();
    let mut words = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .peekable();
    let mut total = 0;
    while let Some(word) = words.next() {
        let digits = word.len() - word.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let Ok(n) = word[..digits].parse::<i64>() else {
            break;
        };
        let unit = match &word[digits..] {
            "" => words.next().unwrap_or_default(),
            unit => unit,
        };
        if unit.starts_with('h') {
            total += n * 60;
        } else if unit.starts_with("min") {
            total += n;
        } else {
            break;
        }
    }
    (total > 0).then_some(total)
}

/// Normalise an English National Opera [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let detail = &payload["detail"];
    let text = |key: &str| detail[key].as_str().filter(|s| !s.is_empty());
    let title = text("title").ok_or_else(|| SourceError::Parse("page without a title".into()))?;
    if !text("venue").is_some_and(|v| v.eq_ignore_ascii_case(VENUE_NAME)) {
        return Ok(None);
    }
    let running = text("running_time")
        .and_then(running_minutes)
        .map(Duration::minutes);
    let mut sessions = Vec::new();
    for p in detail["performances"].as_array().into_iter().flatten() {
        let starts = p["starts"].as_str().unwrap_or_default();
        let starts_at = parse_london_wall_clock(starts)
            .ok_or_else(|| SourceError::Parse(format!("{title:?}: performance {starts:?}")))?;
        sessions.push(Session {
            starts_at,
            ends_at: running.map(|r| starts_at + r),
        });
    }
    let Some(first) = sessions.iter().min().copied() else {
        return Ok(None);
    };
    let source_tags: Vec<String> = text("composer")
        .map(|c| vec!["opera".to_string(), c.to_string()])
        .unwrap_or_default();
    let tags = crate::music::tags_for("music", &source_tags, title)
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut event = NewEvent {
        sessions: Vec::new(),
        dedupe_key: String::new(),
        description: clean_description(text("description")),
        title: title.to_string(),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at: first.starts_at,
        ends_at: first.ends_at,
        all_day: false,
        price: parse_price(text("tickets").unwrap_or_default()),
        url: payload["url"].as_str().map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category: Category::Music,
        tags,
    };
    event.set_sessions(sessions);
    event.dedupe_key = dedupe_key(title, event.starts_at, Some(VENUE_NAME));
    Ok(Some(event))
}

#[async_trait]
impl Source for EnglishNationalOpera {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listing_url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let html = ctx.get_text(&listing_url).await?;
        let slugs = parse_listing(&html, &listing_url)
            .ok_or_else(|| SourceError::Parse("no What's On listing".into()))?;
        let mut out = Vec::new();
        for (n, slug) in slugs.iter().enumerate() {
            if n >= self.max_details {
                ctx.report_error(format!(
                    "{slug}: over the {} event-page cap",
                    self.max_details
                ));
                continue;
            }
            let Ok(url) = listing_url.join(&format!("{EVENT_PREFIX}{slug}/")) else {
                continue;
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html) {
                    Some(detail) => out.push(raw_event(slug, &url, detail)),
                    None => ctx.report_error(format!("{slug}: not an event page")),
                },
                Err(e) => ctx.report_error(format!("{slug}: {e}")),
            }
        }
        Ok(drop_repeats(out))
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only events at the London Coliseum that list their performances on the \
             page. Productions elsewhere (e.g. Aviva Studios, Manchester), schools \
             events booked by form and the Relaxed Programme page (its performances \
             are those of the productions it names) are left out on purpose.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_times() {
        assert_eq!(
            running_minutes("2hrs 15mins There is one interval."),
            Some(135)
        );
        assert_eq!(running_minutes("1hr 50mins"), Some(110));
        assert_eq!(running_minutes("2hrs"), Some(120));
        assert_eq!(running_minutes("2 hours 30 minutes"), Some(150));
        assert_eq!(
            running_minutes("2hrs 30mins. There is one interval."),
            Some(150)
        );
        assert_eq!(
            running_minutes("The run time for Einstein on the Beach is between 4 and 5 hours."),
            None
        );
        assert_eq!(running_minutes(""), None);
    }

    fn payload(detail: Value) -> Value {
        json!({"url": "https://www.eno.org/events/x/", "detail": detail})
    }

    fn tosca(venue: &str, performances: Value) -> Value {
        payload(json!({
            "title": "Tosca",
            "composer": "Puccini",
            "venue": venue,
            "performances": performances,
            "running_time": "2hrs 15mins There is one interval.",
            "tickets": "Tickets from £15 (Ticket price includes all fees.)",
        }))
    }

    #[test]
    fn a_production_is_one_event_with_a_session_per_performance() {
        let ev = normalise_payload(&tosca(
            "London Coliseum",
            json!([
                {"starts": "2026-11-10 19:30:00", "booking": "//booking.eno.org/book-online/76201"},
                {"starts": "2026-12-05 13:30:00", "booking": "//booking.eno.org/book-online/77201"},
            ]),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(ev.category, Category::Music);
        assert_eq!(ev.tags, ["classical"]);
        assert!(!ev.all_day);
        assert_eq!(ev.sessions.len(), 2);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-11-10T19:30:00+00:00");
        assert_eq!(
            ev.sessions[0].ends_at.unwrap().to_rfc3339(),
            "2026-11-10T21:45:00+00:00"
        );
        assert_eq!(
            ev.ends_at.unwrap().to_rfc3339(),
            "2026-12-05T15:45:00+00:00"
        );
        assert_eq!(ev.price.min.unwrap().to_string(), "15");
        assert_eq!(ev.venue_name.as_deref(), Some(VENUE_NAME));
    }

    #[test]
    fn summer_performances_are_london_wall_clock() {
        let ev = normalise_payload(&tosca(
            "London Coliseum",
            json!([{"starts": "2027-06-01 19:30:00", "booking": "x/1"}]),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(ev.starts_at.to_rfc3339(), "2027-06-01T18:30:00+00:00");
    }

    #[test]
    fn elsewhere_or_without_performances_is_skipped() {
        let one = json!([{"starts": "2027-05-29 15:00:00", "booking": "x/1"}]);
        assert!(
            normalise_payload(&tosca("Aviva Studios, Manchester", one))
                .unwrap()
                .is_none()
        );
        assert!(
            normalise_payload(&tosca("London Coliseum", json!([])))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn an_unreadable_performance_or_title_is_an_error() {
        let err = normalise_payload(&tosca(
            "London Coliseum",
            json!([{"starts": "TBC", "booking": "x/1"}]),
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("performance \"TBC\""), "{err}");
        let err = normalise_payload(&payload(json!({"venue": "London Coliseum"})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("without a title"), "{err}");
    }

    #[test]
    fn a_concert_without_a_composer_credit_gets_title_subtags_only() {
        let ev = normalise_payload(&payload(json!({
            "title": "Sessions at the London Coliseum: Black Country, New Road",
            "venue": "London Coliseum",
            "performances": [{"starts": "2026-11-13 19:30:00", "booking": "x/1"}],
        })))
        .unwrap()
        .unwrap();
        assert!(ev.tags.is_empty());
        assert_eq!(ev.ends_at, None);
    }

    fn raw(slug: &str, ids: &[&str]) -> RawEvent {
        let performances: Vec<Value> = ids
            .iter()
            .map(|id| json!({"booking": format!("//booking.eno.org/book-online/{id}/")}))
            .collect();
        raw_event(
            slug,
            &Url::parse("https://www.eno.org/events/x/").unwrap(),
            json!({"performances": performances}),
        )
    }

    #[test]
    fn pages_repeating_other_pages_performances_are_dropped() {
        let kept = drop_repeats(vec![
            raw("tosca", &["76201", "76801"]),
            raw("relaxed-programme", &["76801", "82601"]),
            raw("trial-by-jury", &["82601"]),
            raw("abracadopera", &[]),
        ]);
        let slugs: Vec<_> = kept.iter().map(|r| r.source_event_id.as_str()).collect();
        assert_eq!(slugs, ["tosca", "trial-by-jury", "abracadopera"]);
    }
}
