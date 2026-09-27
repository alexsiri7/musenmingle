//! The Showroom (Penfold Street, NW8) — exhibitions and events from the
//! site's server-rendered listings, plus the event detail pages.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): only Cloudflare's
//!   "content signals" preamble in comments, with no `Content-Signal:` line
//!   and no rules, so everything is allowed.
//! * No `Event` JSON-LD on the listings or the detail pages, so CSS. The
//!   listings `/exhibitions/` and `/events/` each hold a current section
//!   (current and forthcoming) followed by an archive section
//!   (`div[data-archive]`); only cards outside the archive are read. A page
//!   with no current cards is normal between shows; a page with no cards at
//!   all (archive included) means the layout changed and is an error.
//! * A card is an `article` with a link to `/exhibitions/<slug>` or
//!   `/events/<slug>`, an `h2`/`h3` title, a `<p>` date line holding `time`
//!   elements and an `img`. The `time` elements' date attributes are
//!   malformed (`<time datetime"2026-09-25">`, no `=`), so the visible text
//!   is parsed instead.
//! * Current event cards carry no type label, so each event's detail page
//!   is read (at most [`MAX_DETAILS`] a run) for its type ("Workshop
//!   Series", "Listening Session"), its booking/price note and its
//!   description. A failed detail page is reported and the listing data is
//!   kept. Exhibition detail pages are not fetched.
//! * Date lines: "22 October 2026, 6.30–9pm", "15 August 2026, 12pm",
//!   "25 September 2026 – 19 December 2026" (parsed with Chisenhale's
//!   `parse_date_range` / `parse_time_range`). A single day with a time is
//!   timed in London wall clock; a single day without one, a "From <day>"
//!   and every range are all-day (London midnights, `ends_at` the last day,
//!   none for a single day or an open "From" run). Runs longer than
//!   [`MAX_RANGE_DAYS`] are skipped.
//! * Multi-session events (#207): a ranged event whose detail note lists
//!   its session days ("Fortnightly on Tuesdays at 4.30 – 6.30pm / 20
//!   October, 3 November, … 26 January") becomes one event with those
//!   sessions (`normalise::session_days`, which requires the list to start
//!   and end on the range's days), timed by the note's clock range when it
//!   has one, else all-day sessions. Otherwise it stays one all-day range.
//! * Categories: exhibitions → exhibition. Events by type label, then title:
//!   film screenings and children's / family sessions are skipped;
//!   otherwise `map_category` ("Artist talk" → talk, "Workshop Series" →
//!   workshop) and anything else (listening sessions, performances,
//!   parties) is community.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::{parse_date_range, parse_time_range};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, day_sessions, dedupe_key, london_date, london_to_utc,
    map_category, parse_price, session_days, words,
};

pub const KEY: &str = "the-showroom";
/// Longer runs are ongoing displays, not exhibitions.
pub const MAX_RANGE_DAYS: i64 = 366;
/// At most this many event detail pages a run.
pub const MAX_DETAILS: usize = 8;
pub const EXHIBITIONS_PATH: &str = "/exhibitions/";
pub const EVENTS_PATH: &str = "/events/";
const VENUE_NAME: &str = "The Showroom";
const VENUE_ADDRESS: &str = "63 Penfold Street, London NW8 8PQ";

/// Which listing a card came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Exhibitions,
    Events,
}

impl Section {
    fn prefix(self) -> &'static str {
        match self {
            Section::Exhibitions => "/exhibitions/",
            Section::Events => "/events/",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Section::Exhibitions => "exhibitions",
            Section::Events => "events",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Section::Exhibitions => EXHIBITIONS_PATH,
            Section::Events => EVENTS_PATH,
        }
    }
}

pub struct TheShowroom {
    base_url: Url,
}

impl TheShowroom {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }

    fn url(&self, path: &str) -> Result<Url, SourceError> {
        self.base_url
            .join(path)
            .map_err(|e| SourceError::Config(e.to_string()))
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// The card path (`/events/<slug>`) of a link, absolute or relative: a
/// single slug segment under the section's prefix.
pub fn card_path(href: &str, section: Section) -> Option<String> {
    let path = match Url::parse(href) {
        Ok(u) => u.path().to_string(),
        Err(_) => href.split(['?', '#']).next()?.to_string(),
    };
    let slug = path.strip_prefix(section.prefix())?.trim_end_matches('/');
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then(|| format!("{}{slug}", section.prefix()))
}

/// One listing page: the current cards, and how many cards the page has in
/// all (archive included) to tell a quiet season from a layout change.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub cards: Vec<Value>,
    pub total: usize,
}

fn in_archive(e: ElementRef<'_>) -> bool {
    e.ancestors()
        .filter_map(ElementRef::wrap)
        .any(|a| a.value().attr("data-archive").is_some())
}

/// The current (non-archive) cards of one listing page, in page order, one
/// per path.
pub fn parse_listing(html: &str, section: Section) -> Listing {
    let doc = Html::parse_document(html);
    let title_sel = selector("h1, h2, h3");
    let p_sel = selector("p");
    let time_sel = selector("time");
    let link_sel = selector("a[href]");
    let img_sel = selector("img[src]");
    let mut cards: Vec<Value> = Vec::new();
    let mut total = 0;
    for card in doc.select(&selector("main article")) {
        let Some(path) = card
            .select(&link_sel)
            .filter_map(|a| a.value().attr("href"))
            .find_map(|h| card_path(h, section))
        else {
            continue;
        };
        total += 1;
        if in_archive(card) {
            continue;
        }
        let Some(title) = card
            .select(&title_sel)
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        if cards.iter().any(|c| c["path"] == path.as_str()) {
            continue;
        }
        let date_text = card
            .select(&p_sel)
            .find(|p| p.select(&time_sel).next().is_some())
            .map(element_text)
            .filter(|t| !t.is_empty());
        let image = card
            .select(&img_sel)
            .next()
            .and_then(|img| img.value().attr("src"))
            .filter(|s| !s.starts_with("data:"))
            .map(str::to_string);
        cards.push(json!({
            "path": path,
            "title": title,
            "date_text": date_text,
            "image": image,
        }));
    }
    Listing { cards, total }
}

/// What an event's detail page adds: its type label, the short booking /
/// price note under the date, and the description.
pub fn parse_detail(html: &str) -> Value {
    let doc = Html::parse_document(html);
    let first = |s: &str| {
        doc.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let description = doc
        .select(&selector("main article div.block-prose"))
        .map(element_text)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    json!({
        "type": first("main article header p.mb-lh"),
        "info": first("main article header div.prose"),
        "description": (!description.is_empty()).then_some(description),
    })
}

/// The stored payload for a card; `listed_on` is the London date of the
/// fetch, kept for year inference; `detail` is [`parse_detail`]'s output
/// (null when not fetched).
pub fn raw_event(
    card: &Value,
    section: Section,
    site: &Url,
    listed_on: NaiveDate,
    detail: Option<Value>,
) -> RawEvent {
    let path = card["path"].as_str().unwrap_or_default().to_string();
    let url = site
        .join(&path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| path.clone());
    let image = card["image"]
        .as_str()
        .and_then(|src| site.join(src).ok())
        .map(|u| u.to_string());
    RawEvent {
        source_event_id: path,
        source_url: Some(url.clone()),
        payload: json!({
            "url": url,
            "section": section.name(),
            "card": card,
            "detail": detail.unwrap_or(Value::Null),
            "image_url": image,
            "listed_on": listed_on.to_string(),
        }),
    }
}

/// When a date line says an event happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// All day from the first to the last day (`None`: a single day or an
    /// open "From" run).
    Days(NaiveDate, Option<NaiveDate>),
    /// A day, London wall-clock start and optional end.
    Timed(NaiveDate, NaiveTime, Option<NaiveTime>),
}

/// Parse a date line ("22 October 2026, 6.30–9pm", "15 August 2026, 12pm",
/// "25 September 2026 – 19 December 2026", "From 1 October 2026").
pub fn parse_when(text: &str, listed_on: NaiveDate) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {text:?}"));
    let text = clean_text(text);
    let (date_part, time_part) = match text.split_once(',') {
        Some((d, t)) => (d.trim(), t.trim()),
        None => (text.trim(), ""),
    };
    let lower = date_part.to_lowercase();
    if let Some(rest) = lower.strip_prefix("from ") {
        let (day, _) = parse_date_range(rest, listed_on)?.ok_or_else(err)?;
        return Ok(When::Days(day, None));
    }
    let (first, last) = parse_date_range(date_part, listed_on)?.ok_or_else(err)?;
    if first != last {
        return Ok(When::Days(first, Some(last)));
    }
    if time_part.is_empty() {
        return Ok(When::Days(first, None));
    }
    let (from, to) = parse_time_range(time_part).ok_or_else(err)?;
    Ok(When::Timed(first, from, to))
}

/// Category for an event from its type label (may be empty) and title;
/// `None` means out of scope.
pub fn event_category(type_label: &str, title: &str) -> Option<Category> {
    let w = words(&format!("{type_label} {title}"));
    let has = |names: &[&str]| names.iter().any(|n| w.iter().any(|x| x == n));
    if has(&[
        "film",
        "films",
        "screening",
        "screenings",
        "children",
        "childrens",
        "family",
        "families",
        "kids",
    ]) {
        None
    } else {
        Some(map_category(&[type_label, title]).unwrap_or(Category::Community))
    }
}

/// The first clock range in `text` ("at 4.30 – 6.30pm", "2.30-4.30pm."):
/// a run starting at a digit and ending in "am"/"pm" within 20 characters
/// that `parse_time_range` accepts.
pub fn find_clock_range(text: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    let lower = text.to_lowercase();
    lower
        .char_indices()
        .filter(|(i, c)| {
            c.is_ascii_digit()
                && !lower[..*i]
                    .chars()
                    .next_back()
                    .is_some_and(|p| p.is_ascii_digit() || p == '.' || p == ':')
        })
        .find_map(|(i, _)| {
            let rest: String = lower[i..].chars().take(20).collect();
            let end = ["am", "pm"]
                .iter()
                .filter_map(|m| rest.find(m).map(|j| j + 2))
                .min()?;
            parse_time_range(&rest[..end])
        })
}

fn london_midnight(d: NaiveDate) -> DateTime<Utc> {
    london_to_utc(d.and_time(NaiveTime::MIN))
}

/// Normalise a payload built by [`raw_event`].
pub fn normalise_payload(p: &Value) -> Result<Option<NewEvent>, SourceError> {
    let card = &p["card"];
    let detail = &p["detail"];
    let title = card["title"]
        .as_str()
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("card without a title".into()))?;
    let category = if p["section"] == "exhibitions" {
        Category::Exhibition
    } else {
        match event_category(detail["type"].as_str().unwrap_or_default(), &title) {
            Some(c) => c,
            None => return Ok(None),
        }
    };
    let listed_on = p["listed_on"]
        .as_str()
        .and_then(|s| s.parse::<NaiveDate>().ok())
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no listed_on")))?;
    let date_text = card["date_text"]
        .as_str()
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no date line")))?;
    let info = detail["info"].as_str().unwrap_or_default();
    let mut sessions = Vec::new();
    let (starts_at, ends_at, all_day) = match parse_when(date_text, listed_on)? {
        When::Days(first, last) => {
            let last = last.filter(|l| *l > first);
            if last.is_some_and(|l| (l - first).num_days() > MAX_RANGE_DAYS) {
                return Ok(None);
            }
            if let Some(days) = last.and_then(|l| session_days(info, first, l)) {
                sessions = day_sessions(&days, find_clock_range(info));
            }
            (london_midnight(first), last.map(london_midnight), true)
        }
        When::Timed(day, from, to) => {
            let s = london_to_utc(day.and_time(from));
            let e = to
                .map(|t| london_to_utc(day.and_time(t)))
                .filter(|e| *e > s);
            (s, e, false)
        }
    };
    let price = detail["info"].as_str().map(parse_price).unwrap_or_default();
    let mut ev = NewEvent {
        sessions: Vec::new(),
        dedupe_key: String::new(),
        title,
        description: clean_description(detail["description"].as_str()),
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        price,
        url: p["url"].as_str().map(str::to_string),
        image_url: p["image_url"].as_str().map(str::to_string),
        category,
        tags: Vec::new(),
    };
    ev.set_sessions(sessions);
    ev.dedupe_key = dedupe_key(&ev.title, ev.starts_at, Some(VENUE_NAME));
    Ok(Some(ev))
}

#[async_trait]
impl Source for TheShowroom {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listed_on = london_date(Utc::now());
        let mut out: Vec<RawEvent> = Vec::new();
        let html = ctx.get_text(&self.url(EXHIBITIONS_PATH)?).await?;
        let exhibitions = parse_listing(&html, Section::Exhibitions);
        if exhibitions.total == 0 {
            return Err(SourceError::Parse(format!(
                "no exhibition cards on {EXHIBITIONS_PATH}"
            )));
        }
        out.extend(
            exhibitions
                .cards
                .iter()
                .map(|c| raw_event(c, Section::Exhibitions, &self.base_url, listed_on, None)),
        );
        let events = match ctx.get_text(&self.url(EVENTS_PATH)?).await {
            Ok(html) => parse_listing(&html, Section::Events),
            Err(e) => {
                ctx.report_error(format!("{EVENTS_PATH}: {e}"));
                return Ok(out);
            }
        };
        if events.total == 0 {
            ctx.report_error(format!("no event cards on {}", Section::Events.path()));
        }
        for (i, card) in events.cards.iter().enumerate() {
            let path = card["path"].as_str().unwrap_or_default();
            let detail = if i >= MAX_DETAILS {
                ctx.report_error(format!("{path}: over the {MAX_DETAILS} detail-page cap"));
                None
            } else {
                match ctx.get_text(&self.url(path)?).await {
                    Ok(html) => Some(parse_detail(&html)),
                    Err(e) => {
                        ctx.report_error(format!("{path}: {e}"));
                        None
                    }
                }
            };
            out.push(raw_event(
                card,
                Section::Events,
                &self.base_url,
                listed_on,
                detail,
            ));
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
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

    fn payload(section: &str, title: &str, date: &str, kind: Option<&str>) -> Value {
        json!({
            "url": "https://theshowroom.org/events/x",
            "section": section,
            "card": {"path": "/events/x", "title": title, "date_text": date, "image": null},
            "detail": {"type": kind, "info": null, "description": null},
            "image_url": null,
            "listed_on": "2026-09-27",
        })
    }

    #[test]
    fn date_lines() {
        let on = d(2026, 9, 27);
        let w = |s: &str| parse_when(s, on).unwrap();
        assert_eq!(
            w("22 October 2026 , 6.30 – 9pm"),
            When::Timed(d(2026, 10, 22), t(18, 30), Some(t(21, 0)))
        );
        assert_eq!(
            w("15 August 2026, 12pm"),
            When::Timed(d(2026, 8, 15), t(12, 0), None)
        );
        assert_eq!(
            w("18 July 2026, 12–2pm"),
            When::Timed(d(2026, 7, 18), t(12, 0), Some(t(14, 0)))
        );
        assert_eq!(
            w("15 August 2026, 1.30–5pm"),
            When::Timed(d(2026, 8, 15), t(13, 30), Some(t(17, 0)))
        );
        assert_eq!(
            w("29 July 2026, 6.30–8.30pm"),
            When::Timed(d(2026, 7, 29), t(18, 30), Some(t(20, 30)))
        );
        assert_eq!(
            w("25 September 2026 – 19 December 2026"),
            When::Days(d(2026, 9, 25), Some(d(2026, 12, 19)))
        );
        assert_eq!(
            w("20 October 2026 – 26 January 2027"),
            When::Days(d(2026, 10, 20), Some(d(2027, 1, 26)))
        );
        assert_eq!(w("From 1 October 2026"), When::Days(d(2026, 10, 1), None));
        assert_eq!(w("3 October 2026"), When::Days(d(2026, 10, 3), None));
        assert!(parse_when("Coming soon", on).is_err());
    }

    #[test]
    fn card_paths() {
        assert_eq!(
            card_path(
                "https://theshowroom.org/events/deeper-than-rap",
                Section::Events
            )
            .as_deref(),
            Some("/events/deeper-than-rap")
        );
        assert_eq!(
            card_path("/exhibitions/x/", Section::Exhibitions).as_deref(),
            Some("/exhibitions/x")
        );
        assert_eq!(
            card_path("https://theshowroom.org/events", Section::Events),
            None
        );
        assert_eq!(card_path("/events/a/b", Section::Events), None);
        assert_eq!(card_path("/projects/x", Section::Events), None);
    }

    #[test]
    fn categories() {
        assert_eq!(
            event_category(
                "Workshop Series",
                "Made By Hands: The Fabric of Creative Action"
            ),
            Some(Category::Workshop)
        );
        assert_eq!(
            event_category(
                "Artist talk",
                "Harold Offeh in conversation with Jarelle Francis"
            ),
            Some(Category::Talk)
        );
        assert_eq!(
            event_category("Listening Session", "Deeper than Rap"),
            Some(Category::Community)
        );
        assert_eq!(
            event_category("", "The Showroom Summer Party"),
            Some(Category::Community)
        );
        assert_eq!(event_category("Film Screening", "Night Moves"), None);
        assert_eq!(event_category("Family Day", "Make a Banner"), None);
    }

    #[test]
    fn timed_event_before_and_after_the_clock_change() {
        let before = normalise_payload(&payload(
            "events",
            "Deeper than Rap",
            "22 October 2026, 6.30–9pm",
            Some("Listening Session"),
        ))
        .unwrap()
        .unwrap();
        assert!(!before.all_day);
        assert_eq!(before.starts_at.to_rfc3339(), "2026-10-22T17:30:00+00:00");
        assert_eq!(
            before.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2026-10-22T20:00:00+00:00")
        );
        let after = normalise_payload(&payload(
            "events",
            "Talk: Late",
            "29 October 2026, 6.30–8.30pm",
            None,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(after.starts_at.to_rfc3339(), "2026-10-29T18:30:00+00:00");
        assert_eq!(after.category, Category::Talk);
    }

    #[test]
    fn a_day_without_a_time_is_all_day() {
        let ev = normalise_payload(&payload("events", "Talk: Day", "3 October 2026", None))
            .unwrap()
            .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-10-02T23:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn open_ended_from_date_is_all_day_without_end() {
        let ev = normalise_payload(&payload(
            "exhibitions",
            "A Show",
            "From 1 October 2026",
            None,
        ))
        .unwrap()
        .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.category, Category::Exhibition);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-30T23:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn cross_year_workshop_series_is_all_day_to_the_last_session() {
        let ev = normalise_payload(&payload(
            "events",
            "Made By Hands: The Fabric of Creative Action",
            "20 October 2026 – 26 January 2027",
            Some("Workshop Series"),
        ))
        .unwrap()
        .unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.category, Category::Workshop);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-10-19T23:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2027-01-26T00:00:00+00:00")
        );
    }

    #[test]
    fn workshop_series_with_listed_days_has_sessions_across_the_clock_change() {
        let mut p = payload(
            "events",
            "Made By Hands: The Fabric of Creative Action",
            "20 October 2026 – 26 January 2027",
            Some("Workshop Series"),
        );
        p["detail"]["info"] = json!(
            "Intergenerational workshop series for under 30 year olds and over 60 year olds \
             Fortnightly on Tuesdays at 4.30 – 6.30pm 20 October, 3 November, 17 November, \
             1 December, 12 January, 26 January. Participants must be available for all \
             sessions. £30 for 6 sessions / concessions available."
        );
        let ev = normalise_payload(&p).unwrap().unwrap();
        assert!(!ev.all_day);
        let s: Vec<(String, Option<String>)> = ev
            .sessions
            .iter()
            .map(|s| (s.starts_at.to_rfc3339(), s.ends_at.map(|e| e.to_rfc3339())))
            .collect();
        let at = |a: &str, b: &str| (a.to_string(), Some(b.to_string()));
        assert_eq!(
            s,
            vec![
                // BST (UTC+1) before 25 Oct 2026, GMT after.
                at("2026-10-20T15:30:00+00:00", "2026-10-20T17:30:00+00:00"),
                at("2026-11-03T16:30:00+00:00", "2026-11-03T18:30:00+00:00"),
                at("2026-11-17T16:30:00+00:00", "2026-11-17T18:30:00+00:00"),
                at("2026-12-01T16:30:00+00:00", "2026-12-01T18:30:00+00:00"),
                at("2027-01-12T16:30:00+00:00", "2027-01-12T18:30:00+00:00"),
                at("2027-01-26T16:30:00+00:00", "2027-01-26T18:30:00+00:00"),
            ]
        );
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-10-20T15:30:00+00:00");
        assert_eq!(
            ev.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2027-01-26T18:30:00+00:00")
        );
        assert_eq!(ev.price, parse_price("£30"));
    }

    #[test]
    fn a_day_list_that_does_not_match_the_range_is_ignored() {
        let mut p = payload(
            "events",
            "Made By Hands",
            "20 October 2026 – 26 January 2027",
            Some("Workshop Series"),
        );
        p["detail"]["info"] = json!("Tuesdays at 4.30 – 6.30pm: 20 October, 3 November.");
        let ev = normalise_payload(&p).unwrap().unwrap();
        assert!(ev.all_day);
        assert!(ev.sessions.is_empty());
    }

    #[test]
    fn clock_ranges_in_prose() {
        assert_eq!(
            find_clock_range(
                "under 30 year olds, Fortnightly on Tuesdays at 4.30 – 6.30pm 20 October"
            ),
            Some((t(16, 30), Some(t(18, 30))))
        );
        assert_eq!(
            find_clock_range("Sessions are every Saturday, 2.30-4.30pm."),
            Some((t(14, 30), Some(t(16, 30))))
        );
        assert_eq!(
            find_clock_range("Saturdays: 10 Oct, 24 Oct, 7 Nov 2026"),
            None
        );
    }

    #[test]
    fn skips_and_errors() {
        let film = payload(
            "events",
            "Night Moves",
            "3 October 2026",
            Some("Film Screening"),
        );
        assert_eq!(normalise_payload(&film).unwrap(), None);
        let long = payload(
            "exhibitions",
            "Long",
            "1 January 2026 – 1 January 2028",
            None,
        );
        assert_eq!(normalise_payload(&long).unwrap(), None);
        let mut no_date = payload("events", "Talk: X", "", None);
        no_date["card"]["date_text"] = Value::Null;
        let err = normalise_payload(&no_date).unwrap_err();
        assert!(err.to_string().contains("no date"), "{err}");
    }

    #[test]
    fn price_comes_from_the_detail_note() {
        let mut p = payload(
            "events",
            "Deeper than Rap",
            "22 October 2026, 6.30–9pm",
            None,
        );
        p["detail"]["info"] = json!("Tickets £5 All welcome!");
        let ev = normalise_payload(&p).unwrap().unwrap();
        assert_eq!(ev.price, parse_price("£5"));
    }
}
