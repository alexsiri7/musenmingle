//! Blackheath Halls (Lee Road, Blackheath, SE3) — concerts and talks from
//! the site's server-rendered "What's On" listing and event pages (issue
//! #218).
//!
//! * robots.txt (checked 2026-11-08, saved as a fixture): `Crawl-delay: 10`
//!   (before any `User-agent` line, honoured anyway by FetchContext), and the
//!   `User-agent: *` group disallows nothing. A run makes robots.txt, up to
//!   [`MAX_LISTING_PAGES`] listing pages and up to [`MAX_DETAILS`] event
//!   pages, 10 s apart, so it asks the runner for a longer `fetch_timeout`.
//! * No JSON-LD `Event`s (only Yoast's), so CSS. `/whats-on/` shows about
//!   20 cards (`.events-grid .event-card`) and links the next page from
//!   `.has-another-page a` (`?pagetoshow=2`, …; empty on the last page). A
//!   card has the venue's genre labels (`.event-card-types span`: Music,
//!   Jazz, Opera, Talks, Exhibition, Family, Dance, Take Part, Comedy), a
//!   title (`h3`), a date line (`.event-card-date`) and the event page link.
//! * Date lines are London wall clock and usually have no year: "Mon 9
//!   Nov, 10am", "Sun 6 Dec 2026, 2.30pm", "Wed 18 Nov 7.30pm (first
//!   session)", "Sat 30 Jan, 6.30pm SOLD OUT". The year is the one, from
//!   the fetch's on, whose date falls on the written weekday. A time
//!   without am/pm ("Sun 28 Feb, 6.30 *GENERAL ON SALE…*") is pm: the site
//!   writes morning times with "am". The event page repeats the same line.
//! * Each in-scope card's event page is read for what the card lacks: the
//!   price, the duration ("120 minutes", the end), the location and the
//!   description. A card's `h3` and the page's `h1` are sometimes the date
//!   line instead of the title; then the page's post title (`og:title`) is
//!   used. Without the page
//!   (failed or over the cap, reported) the card alone is used, at the
//!   halls. The location is a room of the halls ("Great Hall", "The Hearn
//!   Recital Room") or, for some festival concerts, another place with its
//!   address ("St Michael & All Angels Church, London, SE3 9JL").
//! * Category from the labels: Talks → talk (talks about music too: "Music
//!   Appreciation"), Music, Jazz or Opera → music, Exhibition alone (fairs
//!   and markets) or an exhibition titled so ("LIFEM26: Exhibition Only",
//!   the early-music festival's makers' exhibition) → expo. The rest
//!   (Family or Dance shows, Comedy, Take Part courses, whose listing gives
//!   only the first session) and festival passes are skipped, as are
//!   cancelled events; `qa_scope` says so. The labels are the tags, plus
//!   `classical` for music unless they or the title say jazz, folk, gospel
//!   or swing ([`NOT_CLASSICAL`]).

use std::time::Duration;

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime, Utc, Weekday};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, month_number,
    padded_words, parse_price, words,
};

pub const KEY: &str = "blackheath-halls";
pub const LISTING_PATH: &str = "/whats-on/";
/// At most this many listing pages a run (4 of about 20 cards on
/// 2026-11-08).
pub const MAX_LISTING_PAGES: usize = 8;
/// At most this many event pages a run (about 45 in-scope cards on
/// 2026-11-08).
pub const MAX_DETAILS: usize = 60;
/// robots.txt's `Crawl-delay`, for the fetch timeout.
const CRAWL_DELAY_SECS: u64 = 10;
const VENUE_NAME: &str = "Blackheath Halls";
const VENUE_ADDRESS: &str = "23 Lee Road, London SE3 9RQ";
/// How far back a year-less date may fall from the fetch's day (a card
/// still listed on the day after).
const PAST_DAYS: i64 = 7;
/// Words (as [`padded_words`]) in the labels or title of a concert that is
/// not classical.
const NOT_CLASSICAL: &[&str] = &["jazz", "folk", "gospel", "swing"];

pub struct BlackheathHalls {
    base_url: Url,
}

impl BlackheathHalls {
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

fn labels(scope: ElementRef<'_>, sel: &Selector) -> Vec<String> {
    scope
        .select(sel)
        .map(element_text)
        .filter(|t| !t.is_empty())
        .collect()
}

/// One listing page: its event cards and the next page's URL.
#[derive(Debug, Default)]
pub struct ListingPage {
    pub cards: Vec<Value>,
    pub next: Option<Url>,
}

/// The URL in a card image's `background-image: url(…)` style.
fn background_image(style: &str, page_url: &Url) -> Option<Url> {
    let start = style.find("url(")? + 4;
    let rest = &style[start..];
    let url = rest[..rest.find(')')?].trim_matches(['"', '\'', ' ']);
    page_url.join(url).ok()
}

/// The event cards of a listing page fetched from `page_url`, in page
/// order, and the link to the next page (same host, `/whats-on/?pagetoshow=N`).
pub fn parse_listing(html: &str, page_url: &Url) -> ListingPage {
    let doc = Html::parse_document(html);
    let link = selector("a.button-more-details[href]");
    let types = selector(".event-card-types span");
    let title = selector("h3");
    let date = selector(".event-card-date");
    let image = selector("a.event-card-image[style]");
    let mut cards = Vec::new();
    for card in doc.select(&selector("div.events-grid div.event-card")) {
        let Some(url) = card
            .select(&link)
            .next()
            .and_then(|a| a.value().attr("href"))
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
            .map(str::to_string)
        else {
            continue;
        };
        cards.push(json!({
            "slug": slug,
            "url": url.as_str(),
            "types": labels(card, &types),
            "title": card.select(&title).next().map(element_text),
            "date_text": card.select(&date).next().map(element_text),
            "image_url": card
                .select(&image)
                .next()
                .and_then(|a| background_image(a.value().attr("style")?, page_url))
                .map(|u| u.to_string()),
        }));
    }
    let next = doc
        .select(&selector(".has-another-page a[href]"))
        .next()
        .and_then(|a| a.value().attr("href"))
        .and_then(|h| page_url.join(h).ok())
        .filter(|u| u.host_str() == page_url.host_str() && u.path() == LISTING_PATH);
    ListingPage { cards, next }
}

/// What an event page adds: its labels, heading and post title, price,
/// duration and location lines, and the description; `None` when the page
/// has no `#event-banner`.
pub fn parse_detail(html: &str) -> Option<Value> {
    let doc = Html::parse_document(html);
    let banner = selector("#event-banner");
    let banner = doc.select(&banner).next()?;
    let line = |class: &str| {
        banner
            .select(&selector(&format!(".event-details .{class}")))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let post_title = doc
        .select(&selector(r#"meta[property="og:title"]"#))
        .next()
        .and_then(|m| m.value().attr("content"))
        .map(|t| clean_text(t.trim_end_matches(" - Blackheath Halls")));
    let description: Vec<String> = doc
        .select(&selector(".page-block-content article"))
        .filter(|e| !element_text(*e).is_empty())
        .map(|e| e.inner_html())
        .collect();
    Some(json!({
        "types": labels(banner, &selector(".event-card-types span")),
        "title": banner.select(&selector("h1")).next().map(element_text),
        "post_title": post_title,
        "price_text": line("event-detail-price"),
        "duration_text": line("event-detail-duration"),
        "location": line("event-detail-location"),
        "date_text": line("event-detail-time"),
        "description": (!description.is_empty()).then(|| description.join("\n")),
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

/// One clock token ("10am", "1.10pm", "7.00pm", "6.30"); a time without
/// am/pm before noon is pm.
fn parse_clock(token: &str) -> Option<NaiveTime> {
    let t = token.to_lowercase();
    let (num, pm) = match (t.strip_suffix("pm"), t.strip_suffix("am")) {
        (Some(n), _) => (n, Some(true)),
        (_, Some(n)) => (n, Some(false)),
        _ => (t.as_str(), None),
    };
    let (h, m): (u32, u32) = match num.split_once(['.', ':']) {
        Some((h, m)) if m.len() == 2 => (h.parse().ok()?, m.parse().ok()?),
        Some(_) => return None,
        // A bare number is a time only with am/pm ("10am"), not "2026".
        None if pm.is_some() => (num.parse().ok()?, 0),
        None => return None,
    };
    let h = match pm {
        Some(_) if !(1..=12).contains(&h) => return None,
        Some(true) | None if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        _ => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Parse a date line ("Mon 9 Nov, 10am", "Sun 6 Dec 2026, 2.30pm", "Wed 18
/// Nov 7.30pm (first session)") into its day and London wall-clock time, if
/// any; words after the time are ignored. A year-less date is the first on
/// the written weekday from [`PAST_DAYS`] before `listed_on`.
pub fn parse_date_line(
    text: &str,
    listed_on: NaiveDate,
) -> Result<(NaiveDate, Option<NaiveTime>), SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {text:?}"));
    let tokens: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    let weekday = tokens.first().and_then(|t| t.parse::<Weekday>().ok());
    let rest = &tokens[usize::from(weekday.is_some())..];
    let [day, month, rest @ ..] = rest else {
        return Err(err());
    };
    let day: u32 = day.parse().map_err(|_| err())?;
    let month = month_number(month).ok_or_else(err)?;
    let year = rest
        .first()
        .and_then(|t| t.parse::<i32>().ok())
        .filter(|y| (2000..2100).contains(y));
    let time = rest
        .get(usize::from(year.is_some()))
        .and_then(|t| parse_clock(t));
    let date = match year {
        Some(y) => NaiveDate::from_ymd_opt(y, month, day),
        None => {
            let from = listed_on - chrono::Duration::days(PAST_DAYS);
            (from.year()..=from.year() + 2)
                .filter_map(|y| NaiveDate::from_ymd_opt(y, month, day))
                .filter(|d| weekday.is_none_or(|w| d.weekday() == w))
                .find(|d| *d >= from)
        }
    }
    .ok_or_else(err)?;
    Ok((date, time))
}

/// The category of an event with these labels and title; `None` to skip.
pub fn category(types: &[String], title: &str) -> Option<Category> {
    let has = |label: &str| types.iter().any(|t| t.eq_ignore_ascii_case(label));
    let title_words = words(title);
    let titled = |w: &str| title_words.iter().any(|x| x == w);
    if titled("pass") || titled("cancelled") || titled("postponed") {
        None
    } else if has("Talks") {
        Some(Category::Talk)
    } else if has("Exhibition") && (titled("exhibition") || types.len() == 1) {
        Some(Category::Expo)
    } else if has("Music") || has("Jazz") || has("Opera") {
        Some(Category::Music)
    } else {
        None
    }
}

/// The card's labels and title, the event page's when it was read; a title
/// that is a date line gives way to the next (the page's post title last:
/// it is sometimes shorter than the heading).
fn types_and_title(card: &Value, detail: &Value) -> (Vec<String>, String) {
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(clean_text)
            .filter(|t| !t.is_empty())
            .collect()
    };
    let detail_types = strings(&detail["types"]);
    let types = if detail_types.is_empty() {
        strings(&card["types"])
    } else {
        detail_types
    };
    let title = [&detail["title"], &card["title"], &detail["post_title"]]
        .into_iter()
        .filter_map(|v| v.as_str().map(clean_text))
        .find(|t| !t.is_empty() && !is_date_line(t))
        .unwrap_or_default();
    (types, title)
}

/// Whether a title is really a date line ("Mon 23 Nov, 6.30pm SOLD OUT"),
/// as some cards' and pages' are.
fn is_date_line(title: &str) -> bool {
    let tokens: Vec<&str> = title
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    matches!(tokens.as_slice(), [weekday, day, month, ..]
        if weekday.parse::<Weekday>().is_ok()
            && day.parse::<u32>().is_ok()
            && month_number(month).is_some())
}

/// Whether a card is in scope, so its event page is worth reading.
pub fn needs_detail(card: &Value) -> bool {
    let (types, title) = types_and_title(card, &Value::Null);
    category(&types, &title).is_some()
}

/// The labels as tags, plus `classical` for a concert unless they or the
/// title name a genre in [`NOT_CLASSICAL`].
fn tags(types: &[String], title: &str, category: Category) -> Vec<String> {
    let mut tags: Vec<String> = types.iter().map(|t| t.to_lowercase()).collect();
    let hay = format!("{}{}", padded_words(&tags.join(" ")), padded_words(title));
    if category == Category::Music
        && !NOT_CLASSICAL
            .iter()
            .any(|w| hay.contains(&format!(" {w} ")))
    {
        tags.push("classical".to_string());
    }
    tags
}

/// The venue and address of an event page's location line: a room ("Great
/// Hall") is the halls; a line with an address names another place.
fn venue(location: Option<&str>) -> (String, String) {
    match location.map(clean_text) {
        Some(loc) if loc.contains(',') && !loc.contains(VENUE_NAME) => {
            let name = loc.split(',').next().unwrap_or_default().trim().to_string();
            (name, loc)
        }
        _ => (VENUE_NAME.to_string(), VENUE_ADDRESS.to_string()),
    }
}

/// The minutes of a duration line ("120 minutes").
fn duration_minutes(text: &str) -> Option<i64> {
    let w = words(text);
    match w.as_slice() {
        [n, unit] if unit.starts_with("min") => n.parse().ok().filter(|m| *m > 0),
        _ => None,
    }
}

/// Normalise a payload built by [`raw_event`].
pub fn normalise_payload(p: &Value) -> Result<Option<NewEvent>, SourceError> {
    let card = &p["card"];
    let detail = &p["detail"];
    let (types, title) = types_and_title(card, detail);
    let Some(category) = category(&types, &title) else {
        return Ok(None);
    };
    if title.is_empty() {
        return Err(SourceError::Parse("card without a title".into()));
    }
    let listed_on = p["listed_on"]
        .as_str()
        .and_then(|s| s.parse::<NaiveDate>().ok())
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no listed_on")))?;
    let date_text = card["date_text"]
        .as_str()
        .or(detail["date_text"].as_str())
        .map(clean_text)
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no date")))?;
    let (day, time) = parse_date_line(&date_text, listed_on)?;
    let starts_at = london_to_utc(day.and_time(time.unwrap_or(NaiveTime::MIN)));
    let ends_at = time
        .and(detail["duration_text"].as_str().and_then(duration_minutes))
        .map(|m| starts_at + chrono::Duration::minutes(m));
    let (venue_name, address) = venue(detail["location"].as_str());

    let mut ev = NewEvent {
        dedupe_key: String::new(),
        description: clean_description(detail["description"].as_str()),
        venue_name: Some(venue_name),
        address: Some(address),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day: time.is_none(),
        sessions: Vec::new(),
        price: parse_price(detail["price_text"].as_str().unwrap_or_default()),
        url: card["url"].as_str().map(str::to_string),
        image_url: card["image_url"].as_str().map(str::to_string),
        tags: tags(&types, &title, category),
        category,
        title,
    };
    ev.dedupe_key = dedupe_key(&ev.title, ev.starts_at, ev.venue_name.as_deref());
    Ok(Some(ev))
}

#[async_trait]
impl Source for BlackheathHalls {
    fn key(&self) -> &str {
        KEY
    }

    fn fetch_timeout(&self) -> Option<Duration> {
        // The Crawl-delay per request (robots.txt, listing pages, event
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
            let detail = match card["url"].as_str().map(Url::parse) {
                Some(Ok(url)) if needs_detail(card) => {
                    let slug = card["slug"].as_str().unwrap_or_default();
                    details += 1;
                    if details > MAX_DETAILS {
                        ctx.report_error(format!("{slug}: over the {MAX_DETAILS} event-page cap"));
                        None
                    } else {
                        match ctx.get_text(&url).await {
                            Ok(html) => parse_detail(&html).or_else(|| {
                                ctx.report_error(format!("{slug}: no #event-banner on its page"));
                                None
                            }),
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
            "Only music (Music, Jazz, Opera), talks (Talks) and fairs and markets \
             (Exhibition) are listed: Family and Dance shows, Comedy and Take Part \
             courses, festival passes and cancelled events are left out on purpose.",
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

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn date_lines_take_the_year_of_their_weekday() {
        let on = d(2026, 11, 8);
        let p = |s: &str| parse_date_line(s, on).unwrap();
        assert_eq!(p("Mon 9 Nov, 10am"), (d(2026, 11, 9), Some(t(10, 0))));
        assert_eq!(
            p("Sat 30 Jan, 6.30pm SOLD OUT"),
            (d(2027, 1, 30), Some(t(18, 30)))
        );
        assert_eq!(p("Sat 9 Oct, 7.30pm"), (d(2027, 10, 9), Some(t(19, 30))));
        assert_eq!(
            p("Sun 6 Dec 2026, 2.30pm"),
            (d(2026, 12, 6), Some(t(14, 30)))
        );
        assert_eq!(
            p("Wed 18 Nov 7.30pm (first session)"),
            (d(2026, 11, 18), Some(t(19, 30)))
        );
        assert_eq!(
            p("Sun 28 Feb, 6.30 *GENERAL ON SALE FROM 10AM MON 9 NOV*"),
            (d(2027, 2, 28), Some(t(18, 30)))
        );
        assert_eq!(
            p("Sat 21 Nov 2026, 12.00pm"),
            (d(2026, 11, 21), Some(t(12, 0)))
        );
        assert_eq!(p("Sat 21 Nov 2026"), (d(2026, 11, 21), None));
        assert!(parse_date_line("Coming soon", on).is_err());
        // 31 February never comes.
        assert!(parse_date_line("Wed 31 Feb, 7pm", on).is_err());
    }

    #[test]
    fn categories_from_the_labels() {
        let c = |types: &[&str], title: &str| category(&strings(types), title);
        assert_eq!(c(&["Music"], "Lunchtime Recital"), Some(Category::Music));
        assert_eq!(
            c(&["Jazz", "Music"], "A Jazzy Christmas"),
            Some(Category::Music)
        );
        assert_eq!(c(&["Opera"], "Opera Gala Night"), Some(Category::Music));
        assert_eq!(
            c(&["Music", "Talks"], "Music Appreciation"),
            Some(Category::Talk)
        );
        assert_eq!(c(&["Exhibition"], "Christmas Fair"), Some(Category::Expo));
        assert_eq!(
            c(&["Exhibition", "Music"], "LIFEM26: Exhibition Only"),
            Some(Category::Expo)
        );
        assert_eq!(
            c(&["Exhibition", "Music"], "LIFEM26: Phantasm"),
            Some(Category::Music)
        );
        assert_eq!(
            c(&["Exhibition", "Music"], "LIFEM26: Festival Day Pass"),
            None
        );
        assert_eq!(c(&["Family"], "Room on the Broom"), None);
        assert_eq!(c(&["Dance", "Family"], "The Nutcracker"), None);
        assert_eq!(c(&["Take Part"], "Choir Course"), None);
        assert_eq!(c(&["Comedy"], "Stand-up"), None);
        assert_eq!(c(&["Music"], "CANCELLED: A Concert"), None);
    }

    #[test]
    fn classical_unless_another_genre_is_named() {
        let tags = |types: &[&str], title: &str| tags(&strings(types), title, Category::Music);
        assert_eq!(
            tags(&["Music"], "Lunchtime Recital"),
            ["music", "classical"]
        );
        assert_eq!(tags(&["Jazz", "Music"], "Shez Raja"), ["jazz", "music"]);
        assert_eq!(tags(&["Music"], "Blackheath Goes Gospel 2026"), ["music"]);
        assert_eq!(
            super::tags(&strings(&["Exhibition"]), "Fair", Category::Expo),
            ["exhibition"]
        );
    }

    #[test]
    fn a_date_line_title_gives_way_to_the_post_title() {
        let card = json!({"types": ["Music"], "title": "Mon 23 Nov, 6.30pm SOLD OUT"});
        let detail =
            json!({"title": "Mon 23 Nov, 6.30pm SOLD OUT", "post_title": "Turning of The Year"});
        assert_eq!(types_and_title(&card, &detail).1, "Turning of The Year");
        let detail =
            json!({"title": "Emma Smith: An Evening of Swing", "post_title": "Emma Smith"});
        assert_eq!(
            types_and_title(&card, &detail).1,
            "Emma Smith: An Evening of Swing"
        );
        assert!(!is_date_line("Sun Ra Arkestra"));
    }

    #[test]
    fn a_skipped_card_is_skipped_whatever_its_title() {
        let payload = |types: &[&str]| {
            let card = json!({"types": types, "title": "Mon 23 Nov, 6.30pm SOLD OUT",
                "date_text": "Mon 23 Nov, 6.30pm SOLD OUT"});
            raw_event(&card, d(2026, 11, 8), None).payload
        };
        assert!(matches!(normalise_payload(&payload(&["Family"])), Ok(None)));
        assert!(matches!(
            normalise_payload(&payload(&["Music"])),
            Err(SourceError::Parse(_))
        ));
    }

    #[test]
    fn a_date_without_a_time_is_all_day() {
        let all_day = |date_text: &str| {
            let card = json!({"types": ["Exhibition"], "title": "Christmas Fair",
                "date_text": date_text});
            normalise_payload(&raw_event(&card, d(2026, 11, 8), None).payload)
                .unwrap()
                .unwrap()
                .all_day
        };
        assert!(all_day("Sat 21 Nov 2026"));
        assert!(!all_day("Sat 21 Nov 2026, 12.00pm"));
    }

    #[test]
    fn a_location_with_an_address_is_another_place() {
        assert_eq!(
            venue(Some("St Michael & All Angels Church, London, SE3 9JL")),
            (
                "St Michael & All Angels Church".to_string(),
                "St Michael & All Angels Church, London, SE3 9JL".to_string()
            )
        );
        for room in [Some("Great Hall"), Some("The Hearn Recital Room"), None] {
            assert_eq!(venue(room).0, VENUE_NAME);
        }
        assert_eq!(venue(Some("Blackheath Halls, Great Hall")).0, VENUE_NAME);
    }

    #[test]
    fn durations_are_minutes() {
        assert_eq!(duration_minutes("120 minutes"), Some(120));
        assert_eq!(duration_minutes("about two hours"), None);
    }

    #[test]
    fn the_qa_scope_names_the_skips() {
        let scope = BlackheathHalls::new(Url::parse("https://x.test/").unwrap())
            .qa_scope()
            .unwrap();
        for word in [
            "Family",
            "Dance",
            "Comedy",
            "Take Part",
            "passes",
            "cancelled",
        ] {
            assert!(scope.contains(word), "{word}");
        }
    }
}
