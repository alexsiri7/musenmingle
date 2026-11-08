//! Union Chapel (Compton Terrace, Islington, N1) — the arty end of a
//! working chapel's mixed programme, from the site's server-rendered
//! "What's On" page and event pages (issue #221).
//!
//! * robots.txt (checked 2026-11-08, saved as a fixture) is Cloudflare's
//!   managed block: `User-agent: *` gets `Allow: /` (with a content signal
//!   `ai-train=no`, which ingest doesn't touch); only AI-training crawlers
//!   are disallowed.
//! * No JSON-LD on the listing or the event pages, so CSS. `/whats-on`
//!   holds every card at once (`#items .item`, about 120 on 2026-11-08, not
//!   in date order, a few already past). A card has its event page link
//!   (`/whats-on/<slug>`, written as absolute `https://unionchapel.org.uk`
//!   URLs; the page is read from the listing's own origin), a background
//!   image, the title (the first `.card-title`) and the date with its year
//!   (`.card-inner strong`: "Fri 6 Nov 2026"). Past cards are dropped.
//! * Scope (issue #209, option (b)): the programme is mostly mainstream
//!   gigs, comedy, podcasts, films and building tours, and neither the
//!   cards nor the pages carry usable genre labels (the "Concert" filter
//!   class is on comedy too). So only titles naming an arty music genre
//!   (`crate::music`'s keywords: "An Organ Recital with …", "… Requiem",
//!   "… Choir", "… Orchestra") or one of the chapel's own music series
//!   ([`OWN_SERIES`]: Daylight Music, the Sunday jazz jam) are music, and
//!   titles naming a talk (`map_category`) are talks; the rest, sing-alongs
//!   and cancelled events are skipped. `qa_scope` tells the scraper check.
//! * Each in-scope card's event page (at most [`MAX_DETAILS`] a run) gives
//!   the time, price and description; without it (failed or over the cap,
//!   reported) the event is left out, since the card has no time. The
//!   sidebar's WHEN lines are free text in London wall clock: "Doors
//!   6.30pm", "Starts 7pm", "Show Starts: 7.45pm", "Onstage 8pm", "Doors &
//!   Bar 18:00", "Ends 20:30", "Curfew 10.30pm", "2pm - 4:30pm" or a time on
//!   the date line ("Sunday 8th November 2026. 10:50am."). The start is the
//!   start line's time, else the doors', else an unlabelled range's or the
//!   date line's ([`event_times`]); the end is "Ends" or the range's end, not
//!   the curfew. A page with no time at all is all day. TICKET PRICE ("FREE
//!   event", "Suggested donation £5 at the door.") is read with
//!   `parse_price`; otherwise the price is only on the ticket sites, which
//!   are never fetched. The description is the page's paragraphs up to the
//!   venue's boilerplate (Margins Cafe dinners, "More Information:"); a
//!   cafe note above the text is passed over.
//!   A page headed "CANCELLED" is skipped.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, map_category,
    month_number, padded_words, parse_price,
};

pub const KEY: &str = "union-chapel";
pub const LISTING_PATH: &str = "/whats-on";
const EVENT_PREFIX: &str = "/whats-on/";
/// At most this many event pages a run (9 in-scope cards on 2026-11-08).
pub const MAX_DETAILS: usize = 30;
const VENUE_NAME: &str = "Union Chapel";
const VENUE_ADDRESS: &str = "Compton Terrace, London N1 2UN";
/// The chapel's own music series whose titles don't name a genre, as
/// [`padded_words`] phrases, with their music subtag.
const OWN_SERIES: &[(&str, &str)] = &[("daylight music", "contemporary"), ("sunday jam", "jazz")];
/// Title words or phrases (as [`padded_words`]) of an event that is out of
/// scope even when it names a genre.
const SKIP_TITLE: &[&str] = &[
    "sing along",
    "sing a long",
    "singalong",
    "cancelled",
    "postponed",
];

pub struct UnionChapel {
    base_url: Url,
    max_details: usize,
}

impl UnionChapel {
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

/// The category and music subtags of an event titled `title`; `None` when
/// it is out of scope.
pub fn scope(title: &str) -> Option<(Category, Vec<String>)> {
    let words = padded_words(title);
    let has = |phrase: &str| words.contains(&format!(" {phrase} "));
    if SKIP_TITLE.iter().any(|p| has(p)) {
        return None;
    }
    let mut tags: Vec<String> = crate::music::tags_for("music", &[], title)
        .into_iter()
        .map(str::to_string)
        .collect();
    for (series, tag) in OWN_SERIES {
        if has(series) && !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_string());
        }
    }
    if !tags.is_empty() {
        return Some((Category::Music, tags));
    }
    (map_category(&[title]) == Some(Category::Talk)).then(|| (Category::Talk, Vec::new()))
}

/// A card's printed date ("Fri 6 Nov 2026").
pub fn parse_card_date(text: &str) -> Option<NaiveDate> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let [_, day, month, year] = words[..] else {
        return None;
    };
    NaiveDate::from_ymd_opt(year.parse().ok()?, month_number(month)?, day.parse().ok()?)
}

/// The image URL in a card's `background-image: url("…")` style.
fn background_image(style: &str) -> Option<&str> {
    let (_, rest) = style.split_once("url(")?;
    let (url, _) = rest.split_once(')')?;
    Some(url.trim().trim_matches(['"', '\'']).trim()).filter(|u| !u.is_empty())
}

/// The cards on the What's On page fetched from `page_url`, in page order,
/// one per event page; `None` when the page has no card list at all (not
/// the same as an empty one).
pub fn parse_listing(html: &str, page_url: &Url) -> Option<Vec<Value>> {
    let doc = Html::parse_document(html);
    let list = doc.select(&selector("#items")).next()?;
    let mut cards: Vec<Value> = Vec::new();
    for card in list.select(&selector(".item")) {
        let Some(slug) = card
            .select(&selector("a[href]"))
            .filter_map(|a| page_url.join(a.value().attr("href")?.trim()).ok())
            .find_map(|u| {
                u.path()
                    .strip_prefix(EVENT_PREFIX)
                    .filter(|s| !s.is_empty() && !s.contains('/'))
                    .map(str::to_string)
            })
        else {
            continue;
        };
        if cards.iter().any(|c| c["slug"] == slug.as_str()) {
            continue;
        }
        let Ok(url) = page_url.join(&format!("{EVENT_PREFIX}{slug}")) else {
            continue;
        };
        let image = card
            .select(&selector(".card-image[style]"))
            .filter_map(|e| background_image(e.value().attr("style")?))
            .find_map(|u| page_url.join(u).ok());
        cards.push(json!({
            "slug": slug,
            "url": url.as_str(),
            "title": card.select(&selector(".card-title")).next().map(text_of),
            "date": card.select(&selector(".card-inner strong")).next().map(text_of),
            "image_url": image.map(|u| u.to_string()),
        }));
    }
    Some(cards)
}

/// Whether a card is worth its event page: in scope and not over before
/// `listed_on`. A card whose date can't be read is, so that `normalise`
/// reports it.
pub fn wanted(card: &Value, listed_on: NaiveDate) -> bool {
    card["title"].as_str().and_then(scope).is_some()
        && !card["date"]
            .as_str()
            .and_then(parse_card_date)
            .is_some_and(|d| d < listed_on)
}

/// What an event page adds to its card: the sidebar's WHEN lines, its
/// TICKET PRICE text, the description and whether it is headed
/// "CANCELLED". `None` when it is not an event page.
pub fn parse_detail(html: &str) -> Option<Value> {
    let doc = Html::parse_document(html);
    let article = doc.select(&selector("#content article")).next()?;
    let sidebar = doc.select(&selector(".sidebar .pl-4")).next()?;

    let mut when: Vec<String> = Vec::new();
    let mut price: Vec<String> = Vec::new();
    let mut section = String::new();
    for e in sidebar.child_elements() {
        let text = text_of(e);
        match e.value().name() {
            "h6" => section = text.to_uppercase(),
            "p" if text.is_empty() => {}
            "p" if section == "WHEN" => when.push(text),
            "p" if section == "TICKET PRICE" => price.push(text),
            _ => {}
        }
    }

    let headings = selector("h1, h2, h3, h4, h5, h6");
    let cancelled = article
        .select(&headings)
        .any(|h| padded_words(&text_of(h)).contains(" cancelled "));
    let mut description: Vec<String> = Vec::new();
    for e in article.select(&selector("h1, h2, h3, h4, h5, h6, p")) {
        let text = text_of(e);
        let lower = text.to_lowercase();
        if lower.starts_with("more information") {
            break;
        }
        // Notes about the cafe can sit above the text too; below it, they
        // start the boilerplate.
        if lower.contains("margins") || lower.contains("dinner") {
            if description.is_empty() {
                continue;
            }
            break;
        }
        if e.value().name() == "p" && !text.is_empty() {
            description.push(e.html());
        }
    }
    Some(json!({
        "when": when,
        "price": (!price.is_empty()).then(|| price.join(" ")),
        "description": (!description.is_empty()).then(|| description.join("\n")),
        "cancelled": cancelled,
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

/// One clock time: "7pm", "6.30pm", "12:00pm", "6.30PM", "10:50am." or
/// 24-hour "18:30".
fn parse_clock(token: &str) -> Option<NaiveTime> {
    let t = token
        .trim_matches(|c: char| matches!(c, '.' | ',' | ':' | '(' | ')'))
        .to_lowercase();
    let (num, pm) = match (t.strip_suffix("pm"), t.strip_suffix("am")) {
        (Some(n), _) => (n, Some(true)),
        (_, Some(n)) => (n, Some(false)),
        _ => (t.as_str(), None),
    };
    let (h, m): (u32, u32) = match num.split_once(['.', ':']) {
        Some((h, m)) if m.len() == 2 => (h.parse().ok()?, m.parse().ok()?),
        Some(_) => return None,
        // A bare number is a time only with am/pm.
        None if pm.is_some() => (num.parse().ok()?, 0),
        None => return None,
    };
    let h = match pm {
        Some(_) if !(1..=12).contains(&h) => return None,
        Some(true) if h == 12 => 12,
        Some(true) => h + 12,
        Some(false) if h == 12 => 0,
        _ => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// A WHEN line's label (the lowercase words before its first time) and
/// its times.
fn when_line(line: &str) -> (String, Vec<NaiveTime>) {
    let mut label = Vec::new();
    let mut times = Vec::new();
    for token in line.split(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '—')) {
        match parse_clock(token) {
            Some(t) => times.push(t),
            None if times.is_empty() && !token.is_empty() => label.push(token.to_lowercase()),
            None => {}
        }
    }
    (label.join(" "), times)
}

/// The start and end of an event from its page's WHEN lines (the first is
/// the date line); `None` when they give no time.
pub fn event_times(when: &[String]) -> Option<(NaiveTime, Option<NaiveTime>)> {
    let (mut start, mut doors, mut end, mut plain) = (None, None, None, None);
    for (n, line) in when.iter().enumerate() {
        let (label, times) = when_line(line);
        let Some(&first) = times.first() else {
            continue;
        };
        if label.contains("start") || label.contains("onstage") {
            start = start.or(Some(first));
        } else if label.starts_with("doors") || label.starts_with("dorrs") {
            doors = doors.or(Some(first));
        } else if label.starts_with("ends") {
            end = end.or(Some(first));
        } else if label.is_empty() || n == 0 {
            plain = plain.or(Some((first, times.get(1).copied())));
        }
    }
    let start = start.or(doors).or(plain.map(|(s, _)| s))?;
    let end = end.or(plain.and_then(|(_, e)| e)).filter(|e| *e > start);
    Some((start, end))
}

/// Normalise a Union Chapel [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let card = &payload["card"];
    let detail = &payload["detail"];
    let text = |key: &str| card[key].as_str().filter(|s| !s.is_empty());
    let title = text("title").ok_or_else(|| SourceError::Parse("card without a title".into()))?;
    let Some((category, tags)) = scope(title) else {
        return Ok(None);
    };
    let date = text("date")
        .and_then(parse_card_date)
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: date {:?}", text("date"))))?;
    let listed_on = payload["listed_on"]
        .as_str()
        .and_then(|d| d.parse::<NaiveDate>().ok())
        .ok_or_else(|| SourceError::Parse("payload without listed_on".into()))?;
    if date < listed_on {
        return Ok(None);
    }
    if !detail.is_object() {
        return Err(SourceError::Parse(format!("{title:?}: no event page")));
    }
    if detail["cancelled"].as_bool() == Some(true) {
        return Ok(None);
    }
    let when: Vec<String> = detail["when"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let (starts_at, ends_at, all_day) = match event_times(&when) {
        Some((start, end)) => (
            london_to_utc(date.and_time(start)),
            end.map(|e| london_to_utc(date.and_time(e))),
            false,
        ),
        None => (london_to_utc(date.and_time(NaiveTime::MIN)), None, true),
    };
    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(title, starts_at, Some(VENUE_NAME)),
        description: clean_description(detail["description"].as_str()),
        title: title.to_string(),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        price: detail["price"]
            .as_str()
            .map(parse_price)
            .unwrap_or_default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags,
    }))
}

#[async_trait]
impl Source for UnionChapel {
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
        let cards = parse_listing(&listing, &listing_url)
            .ok_or_else(|| SourceError::Parse(format!("no event cards on {LISTING_PATH}")))?;
        let mut out = Vec::new();
        for (n, card) in cards.iter().filter(|c| wanted(c, listed_on)).enumerate() {
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
                    None => ctx.report_error(format!("{slug}: no event article on its page")),
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
            "Only the arty end of the programme: events whose titles name a classical, \
             jazz or other art-music genre (organ recitals, choirs, orchestras, a \
             Requiem), the chapel's own Daylight Music and Sunday jazz jam, and talks. \
             Mainstream gigs, comedy, podcasts, films, building tours, sing-alongs and \
             cancelled events are left out on purpose; prices are on the ticket sites, \
             so only a page's own TICKET PRICE is read.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn lines(ls: &[&str]) -> Vec<String> {
        ls.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn clocks() {
        assert_eq!(parse_clock("7pm"), Some(t(19, 0)));
        assert_eq!(parse_clock("6.30PM"), Some(t(18, 30)));
        assert_eq!(parse_clock("12:00pm"), Some(t(12, 0)));
        assert_eq!(parse_clock("10:50am."), Some(t(10, 50)));
        assert_eq!(parse_clock("18:30"), Some(t(18, 30)));
        assert_eq!(parse_clock("2026."), None);
        assert_eq!(parse_clock("8th"), None);
        assert_eq!(parse_clock("16+"), None);
        assert_eq!(parse_clock("13pm"), None);
    }

    #[test]
    fn the_start_line_wins_then_doors_then_a_range_or_the_date_line() {
        assert_eq!(
            event_times(&lines(&[
                "Wednesday 14th April 2027",
                "Doors 6.30pm",
                "Starts 7pm",
                "(approx runtime one hour)",
                "Curfew 10.30pm",
            ])),
            Some((t(19, 0), None))
        );
        assert_eq!(
            event_times(&lines(&[
                "Saturday 5th December 2026",
                "Doors: 6.30pm",
                "Show Starts: 7.45pm",
                "Curfew: 10:30pm",
            ])),
            Some((t(19, 45), None))
        );
        assert_eq!(
            event_times(&lines(&[
                "Monday 30th November 2026",
                "Doors 6.30pm",
                "Onstage 8pm"
            ])),
            Some((t(20, 0), None))
        );
        assert_eq!(
            event_times(&lines(&[
                "Monday 21st December 2026",
                "VIP Check-in: 5.45pm - 6pm",
                "Doors 7pm",
                "Curfew 10.30pm",
            ])),
            Some((t(19, 0), None))
        );
        assert_eq!(
            event_times(&lines(&[
                "Sunday 20th December 2026",
                "Doors & Bar 18:00",
                "Starts 19:00",
                "Ends 20:30",
            ])),
            Some((t(19, 0), Some(t(20, 30))))
        );
        assert_eq!(
            event_times(&lines(&["Sunday 8th November 2026", "2pm - 4:30pm"])),
            Some((t(14, 0), Some(t(16, 30))))
        );
        assert_eq!(
            event_times(&lines(&[
                "Sunday 8th November 2026. 10:50am.",
                "All are welcome"
            ])),
            Some((t(10, 50), None))
        );
        assert_eq!(event_times(&lines(&["Sunday 8th November 2026"])), None);
    }

    #[test]
    fn scope_from_the_title() {
        let music = |title: &str| scope(title).map(|(c, tags)| (c, tags.join(",")));
        assert_eq!(
            music("An Organ Recital with Katy Silverman"),
            Some((Category::Music, "classical".into()))
        );
        assert_eq!(music("Sunday Jam"), Some((Category::Music, "jazz".into())));
        assert_eq!(
            music("Daylight Music 412: Anna Meredith"),
            Some((Category::Music, "contemporary".into()))
        );
        assert_eq!(
            music("Poets in conversation"),
            Some((Category::Talk, String::new()))
        );
        for title in [
            "Jake Bugg 27 FEB",
            "Live At The Chapel with Tim Key",
            "Union Chapel Guided Tour | April 2027",
            "Banff Mountain Film Festival World Tour - Red Film Programme - 11 March 27",
            "Choir!Choir!Choir!: An EPIC QUEEN Sing-Along",
            "Organ Recital (CANCELLED)",
        ] {
            assert_eq!(scope(title), None, "{title}");
        }
    }

    #[test]
    fn past_cards_are_not_wanted_but_unreadable_dates_are() {
        let card = |date: &str| json!({"title": "Organ recital", "date": date});
        let today = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        assert!(!wanted(&card("Sat 7 Nov 2026"), today));
        assert!(wanted(&card("Sun 8 Nov 2026"), today));
        assert!(wanted(&card("TBC"), today));
        assert!(!wanted(
            &json!({"title": "Jake Bugg", "date": "Sun 8 Nov 2026"}),
            today
        ));
    }

    #[test]
    fn card_dates() {
        assert_eq!(
            parse_card_date("Fri 6 Nov 2026"),
            NaiveDate::from_ymd_opt(2026, 11, 6)
        );
        assert_eq!(parse_card_date("6 Nov 2026"), None);
        assert_eq!(parse_card_date("Fri 6 Nov"), None);
    }

    fn payload(date: &str, title: &str, detail: Value) -> Value {
        json!({
            "card": {
                "slug": "x",
                "url": "https://unionchapel.org.uk/whats-on/x",
                "title": title,
                "date": date,
            },
            "detail": detail,
            "listed_on": "2026-11-08",
        })
    }

    #[test]
    fn a_page_without_a_time_is_all_day() {
        let detail = json!({"when": ["Wednesday 9th December 2026"], "cancelled": false});
        let ev = normalise_payload(&payload("Wed 9 Dec 2026", "Organ recital", detail))
            .unwrap()
            .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-12-09T00:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn past_and_cancelled_events_are_skipped_and_a_missing_page_is_an_error() {
        let detail = json!({"when": ["Doors 7pm"], "cancelled": false});
        assert!(
            normalise_payload(&payload("Sat 7 Nov 2026", "Organ recital", detail.clone()))
                .unwrap()
                .is_none()
        );
        let cancelled = json!({"when": ["Doors 7pm"], "cancelled": true});
        assert!(
            normalise_payload(&payload("Mon 9 Nov 2026", "Orchestra", cancelled))
                .unwrap()
                .is_none()
        );
        let err = normalise_payload(&payload("Mon 9 Nov 2026", "Orchestra", Value::Null))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no event page"), "{err}");
    }
}
