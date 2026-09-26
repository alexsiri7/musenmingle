//! William Morris Society — hand-written scraper over the "What's on"
//! listing's HTML.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   disallows only `/wp-admin/`; `/whats-on/` is allowed and there is no
//!   Crawl-delay.
//! * There is no JSON-LD `Event` (only WebPage, ImageObject, BreadcrumbList
//!   and WebSite), so CSS selectors are used. `/whats-on/` is one page (its
//!   `div.pagination` is empty) of `li.post-card` cards, each with the title
//!   link (`h3.fusion-title-heading a`, not the card's first link, which is
//!   the "Book Tickets" button), `.event-date` ("October 26, 2026"),
//!   `.event-time-start` / `.event-time-end` ("10:30 am"), `.event-location`,
//!   an excerpt (`.fusion-content-tb`, ending in a "Read More" link, which is
//!   dropped) and an image. The detail pages repeat the same facts as
//!   free-form prose, so they are never fetched.
//! * Cards with an empty date are open-ended or recurring programmes (the
//!   museum exhibition, weekly guided tours, multi-date textile tours) and
//!   are skipped. A non-empty date or time that doesn't parse is an error,
//!   and so is a card without an `/events/<slug>/` title link.
//! * Times are London wall-clock. An end time not after the start (MorrisFest:
//!   "6:00 pm" to "3:00 pm", ending on a later day the card doesn't give) is
//!   dropped; a card with no start time starts at London midnight.
//! * Only events at Kelmscott House (a location naming the Society or
//!   Kelmscott House, including hybrids "… and Online") are kept. Online-only
//!   events and every other location are skipped: the off-site ones today
//!   are at the Birmingham & Midland Institute, and cards give no address to
//!   place a venue. An off-site London event would be skipped too.
//! * Cards have no type label. A title mentioning a "Late(s)" is community;
//!   otherwise the category comes from the title's keywords, then the
//!   excerpt's ("A workshop for 6-12 year olds"); cards matching neither are
//!   skipped.
//! * Price is the amount after a "Price:" or "Cost:" label in the excerpt;
//!   without one it is unknown, since excerpts mention unrelated amounts.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_to_utc, map_category, parse_price, words,
};

pub const KEY: &str = "william-morris-society";
const LISTING_PATH: &str = "/whats-on/";
const VENUE_NAME: &str = "William Morris Society";
const VENUE_ADDRESS: &str = "Kelmscott House, 26 Upper Mall, Hammersmith, London W6 9TA";
/// Kelmscott House (OSM way 176677084).
const VENUE_LAT: f64 = 51.4906;
const VENUE_LNG: f64 = -0.2355;
const PRICE_LABELS: [&str; 2] = ["Price:", "Cost:"];

pub struct WilliamMorrisSociety {
    base_url: Url,
}

impl WilliamMorrisSociety {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// One listing card, as printed.
#[derive(Debug, serde::Serialize)]
pub struct Card {
    /// `/events/<slug>/`.
    pub path: String,
    pub title: String,
    pub date_text: Option<String>,
    pub start_text: Option<String>,
    pub end_text: Option<String>,
    pub location: Option<String>,
    /// Plain text, without the trailing "Read More" link.
    pub excerpt: Option<String>,
    pub image_url: Option<String>,
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

fn event_path(href: &str) -> Option<String> {
    let url = Url::parse(href).ok()?;
    let slug = url.path().strip_prefix("/events/")?.strip_suffix('/')?;
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    valid.then(|| url.path().to_string())
}

fn parse_card(card: ElementRef<'_>) -> Option<Card> {
    let link = card
        .select(&selector("h3.fusion-title-heading a[href]"))
        .next()?;
    let path = event_path(link.value().attr("href")?)?;
    let title = Some(element_text(link)).filter(|t| !t.is_empty())?;
    let first = |s: &str| card.select(&selector(s)).next();
    let first_text = |s: &str| first(s).map(element_text).filter(|t| !t.is_empty());
    let excerpt = first_text(".fusion-content-tb").map(|t| {
        t.strip_suffix("Read More")
            .map(str::trim_end)
            .unwrap_or(&t)
            .to_string()
    });
    Some(Card {
        path,
        title,
        date_text: first_text(".event-date"),
        start_text: first_text(".event-time-start"),
        end_text: first_text(".event-time-end"),
        location: first_text(".event-location"),
        excerpt: excerpt.filter(|t| !t.is_empty()),
        image_url: first(".fusion-imageframe img[src]")
            .and_then(|e| e.value().attr("src"))
            .map(str::to_string),
    })
}

#[derive(Debug)]
pub struct Listing {
    pub cards: Vec<Card>,
    /// Descriptions of cards without a usable title link.
    pub rejected: Vec<String>,
}

pub fn parse_listing(html: &str) -> Listing {
    let doc = Html::parse_document(html);
    let mut cards: Vec<Card> = Vec::new();
    let mut rejected = Vec::new();
    for li in doc.select(&selector("li.post-card")) {
        let Some(card) = parse_card(li) else {
            let heading = li
                .select(&selector("h3"))
                .next()
                .map(element_text)
                .unwrap_or_default();
            rejected.push(format!("unusable listing card {heading:?}"));
            continue;
        };
        if !cards.iter().any(|c| c.path == card.path) {
            cards.push(card);
        }
    }
    Listing { cards, rejected }
}

/// "October 26, 2026".
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%B %d, %Y").ok()
}

/// "10:30 am", "5:30 pm", "10 am".
pub fn parse_clock(s: &str) -> Option<NaiveTime> {
    let s = s.trim().to_lowercase();
    let s = match s.split_once(' ') {
        Some((hour, meridiem)) if !hour.contains(':') => format!("{hour}:00 {meridiem}"),
        _ => s,
    };
    NaiveTime::parse_from_str(&s, "%I:%M %p").ok()
}

/// Whether a card's location is Kelmscott House, the Society's home.
fn at_museum(location: &str) -> bool {
    let l = location.to_lowercase().replace('’', "'");
    l.contains("william morris society") || l.contains("kelmscott house")
}

/// The in-scope category for a card, or `None` to skip it.
pub fn category(title: &str, excerpt: Option<&str>) -> Option<Category> {
    if words(title).iter().any(|w| w == "late" || w == "lates") {
        return Some(Category::Community);
    }
    map_category(&[title]).or_else(|| excerpt.and_then(|e| map_category(&[e])))
}

/// The amount after the first price label in an excerpt.
pub fn price(excerpt: &str) -> Price {
    PRICE_LABELS
        .iter()
        .filter_map(|label| excerpt.find(label).map(|i| i + label.len()))
        .min()
        .and_then(|i| excerpt[i..].split_whitespace().next())
        .map(parse_price)
        .unwrap_or_default()
}

/// Normalise a William Morris Society [`RawEvent`] payload (a card plus its
/// absolute `url` and `image_url`).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("card without title".into()))?;
    let Some(date_text) = text("date_text") else {
        return Ok(None);
    };
    let date = parse_date(date_text)
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: bad date {date_text:?}")))?;
    let clock = |key: &str| {
        text(key)
            .map(|s| {
                parse_clock(s)
                    .ok_or_else(|| SourceError::Parse(format!("{title:?}: bad {key} {s:?}")))
            })
            .transpose()
    };
    let start = clock("start_text")?;
    let end = clock("end_text")?;
    if !text("location").is_some_and(at_museum) {
        return Ok(None);
    }
    let excerpt = text("excerpt");
    let Some(category) = category(&title, excerpt) else {
        return Ok(None);
    };
    let starts_at = london_to_utc(date.and_time(start.unwrap_or(NaiveTime::MIN)));
    let ends_at = start
        .zip(end)
        .filter(|(s, e)| e > s)
        .map(|(_, e)| london_to_utc(date.and_time(e)));

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(excerpt),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        price: excerpt.map(price).unwrap_or_default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: vec![],
    }))
}

/// The [`RawEvent`] for a card on the listing page at `page_url`.
pub fn card_event(card: &Card, page_url: &Url) -> Result<RawEvent, SourceError> {
    let url = page_url
        .join(&card.path)
        .map_err(|e| SourceError::Parse(format!("bad card path {:?}: {e}", card.path)))?;
    let mut payload = serde_json::to_value(card).expect("card serialises");
    payload["url"] = json!(url.as_str());
    payload["image_url"] = json!(
        card.image_url
            .as_deref()
            .and_then(|i| page_url.join(i).ok())
            .map(String::from)
    );
    Ok(RawEvent {
        source_event_id: card.path.trim_matches('/').to_string(),
        source_url: Some(url.to_string()),
        payload,
    })
}

#[async_trait]
impl Source for WilliamMorrisSociety {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let Listing { cards, rejected } = parse_listing(&ctx.get_text(&url).await?);
        for card in rejected {
            ctx.report_error(card);
        }
        if cards.is_empty() {
            return Err(SourceError::Parse(
                "no event cards found on the listing".into(),
            ));
        }
        cards.iter().map(|card| card_event(card, &url)).collect()
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn card(title: &str, date: Option<&str>, start: Option<&str>, end: Option<&str>) -> Value {
        json!({
            "title": title,
            "date_text": date,
            "start_text": start,
            "end_text": end,
            "location": "The William Morris Society",
        })
    }

    fn talk(date: Option<&str>, start: Option<&str>, end: Option<&str>) -> Value {
        card("An Evening Talk", date, start, end)
    }

    fn normalise(payload: &Value) -> Option<NewEvent> {
        normalise_payload(payload).unwrap()
    }

    #[test]
    fn card_times_are_london_wall_clock() {
        let bst = normalise(&talk(
            Some("October 21, 2026"),
            Some("5:30 pm"),
            Some("7:30 pm"),
        ))
        .unwrap();
        assert_eq!(bst.starts_at.to_rfc3339(), "2026-10-21T16:30:00+00:00");
        assert_eq!(
            bst.ends_at.unwrap().to_rfc3339(),
            "2026-10-21T18:30:00+00:00"
        );
        // Clocks go back on 25 October.
        let gmt = normalise(&talk(
            Some("October 26, 2026"),
            Some("10:30 am"),
            Some("12:30 pm"),
        ))
        .unwrap();
        assert_eq!(gmt.starts_at.to_rfc3339(), "2026-10-26T10:30:00+00:00");
        assert_eq!(
            gmt.ends_at.unwrap().to_rfc3339(),
            "2026-10-26T12:30:00+00:00"
        );
    }

    #[test]
    fn dates_and_clocks_parse() {
        assert_eq!(
            parse_date("December 2, 2026"),
            NaiveDate::from_ymd_opt(2026, 12, 2)
        );
        assert_eq!(
            parse_date("November 5, 2026"),
            NaiveDate::from_ymd_opt(2026, 11, 5)
        );
        assert_eq!(parse_clock("10 am"), NaiveTime::from_hms_opt(10, 0, 0));
        assert_eq!(parse_clock("12:30 pm"), NaiveTime::from_hms_opt(12, 30, 0));
        assert_eq!(parse_clock("5:30 PM"), NaiveTime::from_hms_opt(17, 30, 0));
    }

    #[test]
    fn end_not_after_start_is_dropped_and_untimed_cards_start_at_midnight() {
        let event = normalise(&talk(
            Some("November 20, 2026"),
            Some("6:00 pm"),
            Some("3:00 pm"),
        ))
        .unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-11-20T18:00:00+00:00");
        assert_eq!(event.ends_at, None);

        let event = normalise(&talk(Some("October 21, 2026"), None, Some("7:30 pm"))).unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-20T23:00:00+00:00");
        assert_eq!(event.ends_at, None);
    }

    #[test]
    fn only_kelmscott_house_locations_are_kept() {
        let mut payload = talk(Some("October 21, 2026"), Some("5:30 pm"), None);
        for at_museum in [
            "The William Morris Society",
            "The William Morris Society and Online",
            "The William Morris Society and Emery Walker’s House",
            "26 Upper Mall, The William Morris Society, W6 9TA",
            "Kelmscott House",
        ] {
            payload["location"] = json!(at_museum);
            let event = normalise(&payload).expect(at_museum);
            assert_eq!(event.venue_name.as_deref(), Some(VENUE_NAME));
            assert_eq!(event.lat, Some(VENUE_LAT));
        }
        for elsewhere in [
            json!("Online"),
            json!("The Birmingham & Midland Institute"),
            json!("Birmingham & Midland Institute and Online"),
            json!("Kelmscott Manor"),
            json!(null),
        ] {
            payload["location"] = elsewhere.clone();
            assert!(normalise(&payload).is_none(), "{elsewhere}");
        }
    }

    #[test]
    fn category_rules() {
        assert_eq!(
            category("Morris by Moonlight – Museum Lates", None),
            Some(Category::Community)
        );
        assert_eq!(
            category("Water Marbling Workshop with Daunton Marbling", None),
            Some(Category::Workshop)
        );
        assert_eq!(
            category("An Evening Talk with Dr Gavin Stoneystreet", None),
            Some(Category::Talk)
        );
        assert_eq!(
            category(
                "October Half Term Halloween fun at The William Morris Society",
                Some("A workshop for 6-12 year olds")
            ),
            Some(Category::Workshop)
        );
        assert_eq!(
            category(
                "Guided Tours",
                Some("Step into the world of William Morris")
            ),
            None
        );
    }

    #[test]
    fn uncategorised_cards_are_skipped() {
        let payload = card(
            "Guided Tours",
            Some("October 21, 2026"),
            Some("10 am"),
            None,
        );
        assert!(normalise(&payload).is_none());
    }

    #[test]
    fn price_needs_a_label() {
        let pounds = |n| Some(Decimal::new(n, 2));
        assert_eq!(price("Ages 6-12. Price: £7.50 per child").min, pounds(750));
        assert_eq!(price("Cost: £120 Join us for a day").min, pounds(12000));
        assert_eq!(price("A day of marbling."), Price::default());
        assert_eq!(price("Our £5 guidebook is on sale."), Price::default());
    }

    #[test]
    fn undated_cards_are_skipped_and_bad_dates_or_times_are_errors() {
        assert!(normalise(&talk(None, Some("1:30 pm"), Some("4:00 pm"))).is_none());
        for payload in [
            talk(Some("Autumn 2026"), None, None),
            talk(Some("October 21, 2026"), Some("teatime"), None),
            talk(Some("October 21, 2026"), Some("5:30 pm"), Some("late")),
            json!({"date_text": "October 21, 2026"}),
        ] {
            assert!(normalise_payload(&payload).is_err(), "{payload}");
        }
    }
}
