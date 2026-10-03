//! Foundling Museum (Bloomsbury, WC1) — CSS scraper over the paginated
//! "What's on" cards, plus the event pages of timed items.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): Yoast's default,
//!   `User-agent: *` with an empty `Disallow:`, so everything is allowed.
//! * There is no JSON-LD `Event` (only Yoast's WebPage/BreadcrumbList, on
//!   the listing and on event pages), and The Events Calendar's REST API is
//!   not installed (404). `/whats-on/` shows `div.card.card--event` cards in
//!   a featured strip (repeated on every page) and the listing; each has a
//!   link to `/event/<slug>/`, a category (`span.category`), a date
//!   (`span.date`), the title (`h3.card-title`) and a summary. The listing's
//!   `data-next-page` attribute is followed, at most [`MAX_PAGES`] pages.
//! * Category from the card: Exhibitions & Displays → exhibition; Talks,
//!   Tours (in-museum and walking tours) and Conferences → talk; Workshops →
//!   workshop. Families and Concerts are skipped, and so are online
//!   editions (`…-online` slugs, "online" in the title) (`Ok(None)`);
//!   `qa_scope` tells the scraper check.
//! * Exhibitions are read from their card alone: a date-only range with the
//!   year on the end (`30 Jun – 25 Oct 2026`; a start after the end means it
//!   began the year before) or on both ends (`17 Nov 2026 – 18 Apr 2027`),
//!   stored all day from London midnight of the first to the last day.
//! * Every other in-scope card shows only a day, so its event page is
//!   fetched (at most [`MAX_DETAIL_PAGES`] a run) for the page header's date
//!   and London wall-clock start time (`28 Sep 2026 6:30pm`,
//!   `11 Oct 2026 11am`) and the intro text as description. A page without a
//!   time makes an all-day item. Pages beyond the cap are left for the next
//!   run.
//! * Terms restrict the site's material to personal use and forbid copying
//!   its images, so the seed row is facts + link only.
//! * Prices are not stated in a structured way, so price is unknown. Every
//!   item is placed at the museum (walking tours end there).

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc};

pub const KEY: &str = "foundling-museum";
/// Upper bound on listing pages per run (the site shows three).
pub const MAX_PAGES: usize = 6;
/// Upper bound on event pages fetched per run.
pub const MAX_DETAIL_PAGES: usize = 30;
const LISTING_PATH: &str = "/whats-on/";
const DETAIL_PREFIX: &str = "/event/";
const VENUE_NAME: &str = "Foundling Museum";
const VENUE_ADDRESS: &str = "40 Brunswick Square, London WC1N 1AZ";

pub struct FoundlingMuseum {
    base_url: Url,
    max_pages: usize,
    max_detail_pages: usize,
}

impl FoundlingMuseum {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            max_pages: MAX_PAGES,
            max_detail_pages: MAX_DETAIL_PAGES,
        }
    }

    /// Override the per-run cap on event pages (tests).
    pub fn with_max_detail_pages(mut self, n: usize) -> Self {
        self.max_detail_pages = n;
        self
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// One card on a listing page.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Card {
    pub slug: String,
    pub url: String,
    pub title: String,
    pub category: Option<String>,
    pub date_text: Option<String>,
    pub summary: Option<String>,
    pub image_url: Option<String>,
}

/// Extract a listing page's cards (featured strip and listing), in document
/// order, de-duplicated by slug. `page_url` is the address the page was
/// fetched from; links must stay on its host.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<Card> {
    let doc = Html::parse_document(html);
    let mut out: Vec<Card> = Vec::new();
    for card in doc.select(&selector("div.card.card--event")) {
        let first = |s: &str| {
            card.select(&selector(s))
                .next()
                .map(element_text)
                .filter(|t| !t.is_empty())
        };
        let Some(Ok(url)) = card
            .select(&selector("a[href]"))
            .next()
            .and_then(|a| a.value().attr("href"))
            .map(|h| page_url.join(h))
        else {
            continue;
        };
        let Some(slug) = url
            .path()
            .strip_prefix(DETAIL_PREFIX)
            .map(|s| s.trim_end_matches('/'))
            .filter(|s| !s.is_empty() && !s.contains('/'))
        else {
            continue;
        };
        let Some(title) = first(".card-title") else {
            continue;
        };
        if url.host() != page_url.host()
            || url.query().is_some()
            || out.iter().any(|c| c.slug == slug)
        {
            continue;
        }
        out.push(Card {
            slug: slug.to_string(),
            url: url.to_string(),
            title,
            category: first(".card-image .category"),
            date_text: first(".card-details .date"),
            summary: card
                .select(&selector(".card-summary"))
                .next()
                .map(|s| s.inner_html()),
            image_url: card
                .select(&selector(".card-image[data-back]"))
                .next()
                .and_then(|i| i.value().attr("data-back"))
                .and_then(|src| page_url.join(src).ok())
                .map(String::from),
        });
    }
    out
}

/// The listing's next page (`data-next-page`), kept on `page_url`'s host
/// and under the listing path.
pub fn next_page(html: &str, page_url: &Url) -> Option<Url> {
    let doc = Html::parse_document(html);
    let href = doc
        .select(&selector(".section--listing [data-next-page]"))
        .next()?
        .value()
        .attr("data-next-page")?;
    let url = page_url.join(href).ok()?;
    (url.host() == page_url.host() && url.path().starts_with(LISTING_PATH) && url != *page_url)
        .then_some(url)
}

/// Category from a card's label; `None` means out of scope.
pub fn category(label: &str) -> Option<Category> {
    match clean_text(label).to_lowercase().as_str() {
        "exhibitions & displays" | "exhibitions" | "exhibition" => Some(Category::Exhibition),
        "talks" | "tours" | "conferences" => Some(Category::Talk),
        "workshops" => Some(Category::Workshop),
        _ => None,
    }
}

fn is_online(slug: &str, title: &str) -> bool {
    slug.ends_with("-online")
        || title
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == "online")
}

/// Whether the card is in scope at all (its category maps, not online).
pub fn in_scope(card: &Card) -> bool {
    card.category.as_deref().and_then(category).is_some() && !is_online(&card.slug, &card.title)
}

/// Whether the card's own date is enough (an exhibition's date range);
/// every other in-scope card needs its event page for the time.
pub fn needs_detail(card: &Card) -> bool {
    card.category.as_deref().and_then(category) != Some(Category::Exhibition)
}

/// "28 Sep 2026" / "30 Jun" (year optional, month abbreviated or full).
fn parse_day(s: &str) -> Option<(u32, u32, Option<i32>)> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    let (day, month, year) = match tokens.as_slice() {
        [d, m] => (d, m, None),
        [d, m, y] => (d, m, Some(y.parse().ok()?)),
        _ => return None,
    };
    let month = NaiveDate::parse_from_str(&format!("1 {month} 2000"), "%d %b %Y")
        .or_else(|_| NaiveDate::parse_from_str(&format!("1 {month} 2000"), "%d %B %Y"))
        .ok()?
        .month();
    Some((day.parse().ok()?, month, year))
}

/// Parse a card's exhibition date range (or single day) into its first and
/// last day. A start without a year takes the end's year, or the year
/// before if that would put it after the end.
pub fn parse_range(text: &str) -> Result<(NaiveDate, NaiveDate), SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date range {text:?}"));
    let text = clean_text(text);
    let (start, end) = match text.split_once(['–', '—', '-']) {
        Some((s, e)) => (Some(s.trim()), e.trim()),
        None => (None, text.trim()),
    };
    let (d, m, Some(y)) = parse_day(end).ok_or_else(err)? else {
        return Err(err());
    };
    let last = NaiveDate::from_ymd_opt(y, m, d).ok_or_else(err)?;
    let first = match start.map(parse_day) {
        None => last,
        Some(None) => return Err(err()),
        Some(Some((d, m, Some(y)))) => NaiveDate::from_ymd_opt(y, m, d).ok_or_else(err)?,
        Some(Some((d, m, None))) => {
            let same = NaiveDate::from_ymd_opt(last.year(), m, d).ok_or_else(err)?;
            if same > last {
                NaiveDate::from_ymd_opt(last.year() - 1, m, d).ok_or_else(err)?
            } else {
                same
            }
        }
    };
    if first > last {
        return Err(err());
    }
    Ok((first, last))
}

/// "6:30pm" / "11am" / "12.30pm" → time.
fn parse_clock(s: &str) -> Option<NaiveTime> {
    let s = s.trim().to_lowercase().replace(' ', "");
    let (digits, pm) = if let Some(d) = s.strip_suffix("pm") {
        (d, true)
    } else {
        (s.strip_suffix("am")?, false)
    };
    let (h, m) = match digits.split_once([':', '.']) {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (digits.parse::<u32>().ok()?, 0),
    };
    if !(1..=12).contains(&h) {
        return None;
    }
    let h = match (h, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, true) => h + 12,
        (h, false) => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// A London wall-clock start and optional end time.
pub type StartEnd = (NaiveTime, Option<NaiveTime>);

/// Parse an event page's header date: a day with an optional London
/// wall-clock start (`28 Sep 2026 6:30pm`, `11 Oct 2026`), optionally with
/// an end time (`… 6:30pm – 8pm`).
pub fn parse_day_and_time(text: &str) -> Result<(NaiveDate, Option<StartEnd>), SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date {text:?}"));
    let text = clean_text(text);
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.len() < 3 {
        return Err(err());
    }
    let (d, m, Some(y)) = parse_day(&tokens[..3].join(" ")).ok_or_else(err)? else {
        return Err(err());
    };
    let day = NaiveDate::from_ymd_opt(y, m, d).ok_or_else(err)?;
    let rest = tokens[3..].join(" ");
    if rest.is_empty() {
        return Ok((day, None));
    }
    let (start, end) = match rest.split_once(['–', '—', '-']) {
        Some((a, b)) => (a, Some(parse_clock(b).ok_or_else(err)?)),
        None => (rest.as_str(), None),
    };
    let start = parse_clock(start).ok_or_else(err)?;
    Ok((day, Some((start, end))))
}

/// Parse an event page into the facts the card lacks.
pub fn parse_detail(html: &str) -> Option<Value> {
    let doc = Html::parse_document(html);
    let first = |s: &str| {
        doc.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let date = first(".article-title .date")?;
    Some(json!({
        "date_text": date,
        "category": first(".article-title .category"),
        "title": first(".article-title h1.title"),
        "description": doc
            .select(&selector(".article-body .content--intro"))
            .next()
            .map(|s| s.inner_html()),
    }))
}

/// Build the [`RawEvent`] for a card, with its event page's facts if read.
pub fn raw_event(card: &Card, detail: Option<Value>) -> RawEvent {
    RawEvent {
        source_event_id: card.slug.clone(),
        source_url: Some(card.url.clone()),
        payload: json!({
            "card": card,
            "detail": detail,
        }),
    }
}

/// Normalise a Foundling Museum [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let card = payload
        .get("card")
        .ok_or_else(|| SourceError::Parse("payload without card".into()))?;
    let detail = payload.get("detail").filter(|d| !d.is_null());
    fn text<'a>(v: Option<&'a Value>, k: &str) -> Option<&'a str> {
        v.and_then(|v| v.get(k)).and_then(Value::as_str)
    }
    let title = text(Some(card), "title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("card without title".into()))?;
    let slug = text(Some(card), "slug").unwrap_or_default();
    let Some(category) = text(Some(card), "category").and_then(category) else {
        return Ok(None);
    };
    if is_online(slug, &title) {
        return Ok(None);
    }
    let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
    let (starts_at, ends_at, all_day): (DateTime<Utc>, _, _) = if category == Category::Exhibition {
        let date = text(Some(card), "date_text")
            .ok_or_else(|| SourceError::Parse(format!("{title:?} has no date")))?;
        let (first, last) = parse_range(date)?;
        (
            midnight(first),
            (last > first).then(|| midnight(last)),
            true,
        )
    } else {
        let date = text(detail, "date_text").ok_or_else(|| {
            SourceError::Parse(format!("{title:?}: event page not read or without a date"))
        })?;
        match parse_day_and_time(date)? {
            (day, None) => (midnight(day), None, true),
            (day, Some((start, end))) => {
                let starts_at = london_to_utc(day.and_time(start));
                let ends_at = end
                    .map(|e| london_to_utc(day.and_time(e)))
                    .filter(|e| *e > starts_at);
                (starts_at, ends_at, false)
            }
        }
    };
    let description = clean_description(text(detail, "description"))
        .or_else(|| clean_description(text(Some(card), "summary")));

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description,
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        price: Default::default(),
        url: text(Some(card), "url").map(str::to_string),
        image_url: text(Some(card), "image_url").map(str::to_string),
        category,
        tags: Vec::new(),
    }))
}

#[async_trait]
impl Source for FoundlingMuseum {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let mut url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let mut cards: Vec<Card> = Vec::new();
        for page in 0..self.max_pages {
            let html = match ctx.get_text(&url).await {
                Ok(html) => html,
                Err(e) if page > 0 => {
                    ctx.report_error(format!("listing page {}: {e}", page + 1));
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            for card in parse_listing(&html, &url) {
                if !cards.iter().any(|c| c.slug == card.slug) {
                    cards.push(card);
                }
            }
            match next_page(&html, &url) {
                Some(next) => url = next,
                None => break,
            }
        }
        if cards.is_empty() {
            return Err(SourceError::Parse(
                "no event cards found on the What's on page".into(),
            ));
        }
        let mut out = Vec::new();
        let mut details = 0;
        for card in cards.iter().filter(|c| in_scope(c)) {
            if !needs_detail(card) {
                out.push(raw_event(card, None));
                continue;
            }
            if details >= self.max_detail_pages {
                continue;
            }
            details += 1;
            let url = match self
                .base_url
                .join(&format!("{DETAIL_PREFIX}{}/", card.slug))
            {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad event slug {}: {e}", card.slug));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html) {
                    Some(detail) => out.push(raw_event(card, Some(detail))),
                    None => ctx.report_error(format!("{}: no date on the event page", card.slug)),
                },
                Err(e) => ctx.report_error(format!("{}: {e}", card.slug)),
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only cards whose category is Exhibitions & Displays, Talks, Tours \
             (in-museum and walking tours), Conferences or Workshops. Families \
             (including the offsite Foundling Libraries sessions) and Concerts are \
             left out on purpose, as are online editions (\"online\" in the title \
             or an event slug ending in -online).",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn parses_exhibition_ranges() {
        assert_eq!(
            parse_range("30 Jun – 25 Oct 2026").unwrap(),
            (d("2026-06-30"), d("2026-10-25"))
        );
        assert_eq!(
            parse_range("17 Nov 2026 – 18 Apr 2027").unwrap(),
            (d("2026-11-17"), d("2027-04-18"))
        );
        assert_eq!(
            parse_range("17 Nov – 18 Apr 2027").unwrap(),
            (d("2026-11-17"), d("2027-04-18"))
        );
        assert_eq!(
            parse_range("16 June – 1 November 2026").unwrap(),
            (d("2026-06-16"), d("2026-11-01"))
        );
        assert_eq!(
            parse_range("25 Oct 2026").unwrap(),
            (d("2026-10-25"), d("2026-10-25"))
        );
        for bad in [
            "Ongoing",
            "30 Jun – 25 Oct",
            "25 Oct 2026 – 30 Jun 2026",
            "",
        ] {
            assert!(parse_range(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_event_page_dates() {
        assert_eq!(
            parse_day_and_time("28 Sep 2026 6:30pm").unwrap(),
            (d("2026-09-28"), Some((t(18, 30), None)))
        );
        assert_eq!(
            parse_day_and_time("11 Oct 2026                        11am").unwrap(),
            (d("2026-10-11"), Some((t(11, 0), None)))
        );
        assert_eq!(
            parse_day_and_time("16 Oct 2026 7pm – 9pm").unwrap(),
            (d("2026-10-16"), Some((t(19, 0), Some(t(21, 0)))))
        );
        assert_eq!(
            parse_day_and_time("12 Dec 2026").unwrap(),
            (d("2026-12-12"), None)
        );
        for bad in [
            "28 Sep 6:30pm",
            "28 Sep 2026 18:30",
            "28 Sep 2026 13pm",
            "Soon",
        ] {
            assert!(parse_day_and_time(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn categories_and_skips() {
        assert_eq!(
            category("Exhibitions & Displays"),
            Some(Category::Exhibition)
        );
        assert_eq!(category("Talks"), Some(Category::Talk));
        assert_eq!(category("Tours"), Some(Category::Talk));
        assert_eq!(category("Conferences"), Some(Category::Talk));
        assert_eq!(category("Workshops"), Some(Category::Workshop));
        assert_eq!(category("Families"), None);
        assert_eq!(category("Concerts"), None);
        assert!(is_online(
            "wet-nursing-foundling-babies-online",
            "Meet the Experts"
        ));
        assert!(is_online("x", "Online talk: Handel"));
        assert!(!is_online(
            "wet-nursing-foundling-babies",
            "Meet the Experts"
        ));
    }

    fn payload(category: &str, date: &str, detail_date: Option<&str>) -> Value {
        json!({
            "card": {
                "slug": "x",
                "url": "https://foundlingmuseum.org.uk/event/x/",
                "title": "Something",
                "category": category,
                "date_text": date,
            },
            "detail": detail_date.map(|d| json!({"date_text": d})),
        })
    }

    #[test]
    fn a_timed_talk_uses_the_event_page_time() {
        let e = normalise_payload(&payload("Talks", "28 Sep 2026", Some("28 Sep 2026 6:30pm")))
            .unwrap()
            .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-09-28T17:30:00+00:00");
        assert!(!e.all_day);
        assert_eq!(e.ends_at, None);
        let e = normalise_payload(&payload("Tours", "22 Nov 2026", Some("22 Nov 2026 2pm")))
            .unwrap()
            .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-11-22T14:00:00+00:00");
    }

    #[test]
    fn an_untimed_event_page_is_all_day() {
        let e = normalise_payload(&payload("Workshops", "22 Nov 2026", Some("22 Nov 2026")))
            .unwrap()
            .unwrap();
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-11-22T00:00:00+00:00");
        assert_eq!(e.ends_at, None);
    }

    #[test]
    fn a_talk_without_its_event_page_is_an_error() {
        assert!(normalise_payload(&payload("Talks", "28 Sep 2026", None)).is_err());
    }

    #[test]
    fn an_exhibition_is_an_all_day_range_from_its_card() {
        let e = normalise_payload(&payload(
            "Exhibitions & Displays",
            "30 Jun – 25 Oct 2026",
            None,
        ))
        .unwrap()
        .unwrap();
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-06-29T23:00:00+00:00");
        assert_eq!(
            e.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-24T23:00:00+00:00")
        );
    }

    #[test]
    fn families_concerts_and_online_are_skipped() {
        for p in [
            payload("Families", "2 Oct 2026", Some("2 Oct 2026 10am")),
            payload("Concerts", "16 Oct 2026", Some("16 Oct 2026 7pm")),
        ] {
            assert!(normalise_payload(&p).unwrap().is_none());
        }
        let mut online = payload("Talks", "9 Oct 2026", Some("9 Oct 2026 6:30pm"));
        online["card"]["slug"] = json!("wet-nursing-foundling-babies-online");
        assert!(normalise_payload(&online).unwrap().is_none());
    }

    #[test]
    fn the_qa_scope_names_what_is_left_out() {
        let scope = FoundlingMuseum::new(Url::parse("https://x.test/").unwrap())
            .qa_scope()
            .unwrap();
        for word in [
            "Exhibitions",
            "Talks",
            "Workshops",
            "Families",
            "Foundling Libraries",
            "Concerts",
            "online",
        ] {
            assert!(scope.contains(word), "{word}");
        }
    }
}
