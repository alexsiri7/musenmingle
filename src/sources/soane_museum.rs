//! Sir John Soane's Museum — hand-written scraper over the "What's on"
//! listing's HTML.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   disallows only Drupal internals (`/core/`, `/admin/`, `/search/`, …);
//!   `/whats-on`, `/whats-on/…` and `/exhibitions/…` are allowed and there is
//!   no Crawl-delay.
//! * There is no JSON-LD, so CSS selectors are used. `/whats-on` is a Drupal
//!   view paginated with a `.pager a[rel=next]` link (`?page=1`, …). Each
//!   card (`.o-view__listing article[about]`) has the type label, the title,
//!   one `<time>` (a single date) or two (a range), a price line and a
//!   teaser. The pager is offset-based and unstable when cards tie on date:
//!   on 2026-09-26 one exhibition was on two pages, so cards are
//!   de-duplicated by path, and a card can also be missed for a run.
//! * The `<time datetime>` attributes are genuine UTC: "18:00" on the talk of
//!   30 Sep 2026 is `17:00:00Z` (BST) and "6pm" on the Late of 30 Oct 2026 is
//!   `18:00:00Z` (GMT). Exhibitions show dates only (the attributes carry
//!   stray times of day), so both ends are stored as London midnight of
//!   their day, as for Whitechapel Gallery.
//! * Category comes from the type label: Exhibitions → exhibition, Talks →
//!   talk, Workshops and Courses and Classes → workshop, Soane Lates and
//!   Families (holiday workshops and drop-ins) → community. Tours are
//!   skipped, as are non-exhibition cards spanning more than one London day
//!   (the year-long daily Highlights Tour, termly children's clubs and
//!   courses) and online-only exhibitions (price "Online only…" or a teaser
//!   calling it an "online exhibition").
//! * Price is the card's price line ("Tickets: £15 (£5 students)" → £15,
//!   "Tickets are free, but…", "£12 p/p"); cards without one (most
//!   exhibitions) have an unknown price.
//! * Talks and other events can be off-site (the Soane Medal Lecture is at
//!   the Royal Academy), so the detail page of every in-scope
//!   non-exhibition card is fetched for its location: the last paragraph of
//!   the "Event Info" sidebar. A location naming the Soane or Lincoln's Inn
//!   (the museum and its No. 14 annex) is the museum; any other line with a
//!   postcode becomes the venue ("name (entrance), address…", the entrance
//!   dropped so the name matches other listings) with no coordinates; a line
//!   that is neither is an error, since it means the sidebar has changed.
//!   Exhibitions are in the museum and need no detail page.

use async_trait::async_trait;
use chrono::{DateTime, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, parse_datetime,
    parse_price, postcode_outward,
};

pub const KEY: &str = "soane-museum";
/// Upper bound on listing pages fetched per run.
pub const MAX_LISTING_PAGES: usize = 5;
/// Upper bound on detail pages fetched per run (≈ 40 s at 1 req / 2 s);
/// in-scope non-exhibition cards beyond it are left out of the run.
pub const MAX_DETAIL_PAGES: usize = 20;
const LISTING_PATH: &str = "/whats-on";
const VENUE_NAME: &str = "Sir John Soane's Museum";
const VENUE_ADDRESS: &str = "13 Lincoln's Inn Fields, London WC2A 3BP";
/// Approximate location of the building.
const VENUE_LAT: f64 = 51.5170;
const VENUE_LNG: f64 = -0.1174;
const EXHIBITIONS: &str = "Exhibitions";

pub struct SoaneMuseum {
    base_url: Url,
}

impl SoaneMuseum {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// One listing card, as printed.
#[derive(Debug, serde::Serialize)]
pub struct Card {
    /// `/exhibitions/<slug>` or `/whats-on/<slug>`.
    pub path: String,
    pub title: String,
    pub event_type: Option<String>,
    pub date_text: Option<String>,
    pub starts: Option<String>,
    pub ends: Option<String>,
    pub price_text: Option<String>,
    pub teaser: Option<String>,
    /// As found in the card (usually relative to the site).
    pub image_url: Option<String>,
}

/// One page of the listing.
#[derive(Debug, serde::Serialize)]
pub struct ListingPage {
    pub cards: Vec<Card>,
    /// The "Next" link (`?page=N`), relative to the page's own URL.
    pub next_page: Option<String>,
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

fn valid_path(path: &str) -> bool {
    ["/exhibitions/", "/whats-on/"].iter().any(|prefix| {
        path.strip_prefix(prefix).is_some_and(|slug| {
            !slug.is_empty()
                && slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
    })
}

fn parse_card(article: ElementRef<'_>) -> Option<Card> {
    let path = article.value().attr("about")?;
    if !valid_path(path) {
        return None;
    }
    let first = |s: &str| article.select(&selector(s)).next();
    let first_text = |s: &str| first(s).map(element_text).filter(|t| !t.is_empty());
    let title = first_text(".o-teaser__title a")?;
    let times: Vec<&str> = article
        .select(&selector(".o-teaser__date time[datetime]"))
        .filter_map(|e| e.value().attr("datetime"))
        .collect();
    Some(Card {
        path: path.to_string(),
        title,
        event_type: first_text(".o-teaser__event-type"),
        date_text: first_text(".o-teaser__date"),
        starts: times.first().map(|t| t.to_string()),
        ends: times.get(1).map(|t| t.to_string()),
        price_text: first_text(".o-teaser__price"),
        teaser: first(".o-teaser__content > p").map(|e| e.inner_html()),
        image_url: first(".o-teaser__thumb img[src]")
            .and_then(|e| e.value().attr("src"))
            .map(str::to_string),
    })
}

pub fn parse_listing(html: &str) -> ListingPage {
    let doc = Html::parse_document(html);
    let mut cards: Vec<Card> = Vec::new();
    for article in doc.select(&selector(".o-view__listing article[about]")) {
        let Some(card) = parse_card(article) else {
            continue;
        };
        if !cards.iter().any(|c| c.path == card.path) {
            cards.push(card);
        }
    }
    let next_page = doc
        .select(&selector(".pager a[rel=next][href]"))
        .next()
        .and_then(|a| a.value().attr("href"))
        .map(str::to_string);
    ListingPage { cards, next_page }
}

/// Whether a location line names the museum or its No. 14 annex.
fn at_museum(location: &str) -> bool {
    let l = location.to_lowercase().replace('’', "'");
    l.contains("soane") || l.contains("lincoln's inn")
}

/// The location line of a detail page: the last paragraph of the "Event
/// Info" sidebar. It follows the time, price and age lines, so one that
/// neither names the museum nor carries a postcode is not a location.
pub fn parse_detail_location(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    doc.select(&selector("aside.o-sidebar__info-box > p"))
        .next_back()
        .map(element_text)
        .filter(|t| at_museum(t) || postcode_outward(t).is_some())
}

fn is_online_only(price_text: Option<&str>, teaser: Option<&str>) -> bool {
    let has = |text: Option<&str>, phrase: &str| {
        text.is_some_and(|t| clean_text(t).to_lowercase().contains(phrase))
    };
    has(price_text, "online only") || has(teaser, "online exhibition")
}

/// The in-scope category for a card's type label, or `None` to skip it.
pub fn category(event_type: &str, multi_day: bool) -> Option<Category> {
    match event_type {
        EXHIBITIONS => Some(Category::Exhibition),
        _ if multi_day => None,
        "Talks" => Some(Category::Talk),
        "Workshops" | "Courses and Classes" => Some(Category::Workshop),
        "Soane Lates" | "Families" => Some(Category::Community),
        _ => None,
    }
}

struct Classified {
    starts_at: DateTime<Utc>,
    ends_at: Option<DateTime<Utc>>,
    /// `None` to skip the card.
    category: Option<Category>,
}

fn classify(payload: &Value) -> Result<Classified, SourceError> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let title = text("title").unwrap_or_default();
    let time = |key: &str| {
        text(key)
            .map(|s| {
                parse_datetime(s)
                    .ok_or_else(|| SourceError::Parse(format!("{title:?}: bad {key} time {s:?}")))
            })
            .transpose()
    };
    let starts_at =
        time("starts")?.ok_or_else(|| SourceError::Parse(format!("{title:?}: no date")))?;
    let ends_at = time("ends")?;
    if ends_at.is_some_and(|e| e < starts_at) {
        return Err(SourceError::Parse(format!(
            "{title:?}: ends before it starts"
        )));
    }
    let multi_day = ends_at.is_some_and(|e| london_date(e) > london_date(starts_at));
    let category = text("event_type")
        .filter(|_| !is_online_only(text("price_text"), text("teaser")))
        .and_then(|t| category(t, multi_day));
    Ok(Classified {
        starts_at,
        ends_at,
        category,
    })
}

fn london_midnight(t: DateTime<Utc>) -> DateTime<Utc> {
    london_to_utc(london_date(t).and_time(NaiveTime::MIN))
}

/// `(name, address, lat, lng)` for a detail page's location line; `None`
/// (exhibitions) or a line naming the museum is the museum itself.
fn venue(location: Option<&str>) -> (String, Option<String>, Option<f64>, Option<f64>) {
    match location {
        Some(l) if !at_museum(l) => {
            let (name, address) = match l.split_once(", ") {
                Some((name, address)) => (name, Some(address.to_string())),
                None => (l, None),
            };
            let name = name.split(" (").next().unwrap_or(name);
            (name.to_string(), address, None, None)
        }
        _ => (
            VENUE_NAME.to_string(),
            Some(VENUE_ADDRESS.to_string()),
            Some(VENUE_LAT),
            Some(VENUE_LNG),
        ),
    }
}

/// Normalise a Soane [`RawEvent`] payload (a card plus, for non-exhibitions,
/// the detail page's `location`).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("card without title".into()))?;
    let Classified {
        mut starts_at,
        mut ends_at,
        category,
    } = classify(payload)?;
    let Some(category) = category else {
        return Ok(None);
    };
    if category == Category::Exhibition {
        starts_at = london_midnight(starts_at);
        ends_at = ends_at.map(london_midnight);
    }
    let (venue_name, address, lat, lng) = venue(payload.get("location").and_then(Value::as_str));
    let price = payload
        .get("price_text")
        .and_then(Value::as_str)
        .map(|t| parse_price(t.split('(').next().unwrap_or(t)))
        .unwrap_or_default();
    let event_type = payload.get("event_type").and_then(Value::as_str);

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(&venue_name)),
        description: clean_description(payload.get("teaser").and_then(Value::as_str)),
        title,
        venue_name: Some(venue_name),
        address,
        lat,
        lng,
        starts_at,
        ends_at,
        price,
        url: payload
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        image_url: payload
            .get("image_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        category,
        tags: event_type.map(str::to_string).into_iter().collect(),
    }))
}

/// The [`RawEvent`] for a card on the listing page at `page_url`, and the
/// card's absolute URL (its detail page).
pub fn card_event(card: &Card, page_url: &Url) -> Result<(Url, RawEvent), SourceError> {
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
    let raw = RawEvent {
        source_event_id: card.path.trim_matches('/').to_string(),
        source_url: Some(url.to_string()),
        payload,
    };
    Ok((url, raw))
}

#[async_trait]
impl Source for SoaneMuseum {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let mut url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let mut cards: Vec<(Url, RawEvent)> = Vec::new();
        for _ in 0..MAX_LISTING_PAGES {
            let page = parse_listing(&ctx.get_text(&url).await?);
            for card in &page.cards {
                let (card_url, raw) = card_event(card, &url)?;
                if !cards.iter().any(|(u, _)| *u == card_url) {
                    cards.push((card_url, raw));
                }
            }
            let Some(next) = page.next_page else {
                break;
            };
            url = url
                .join(&next)
                .map_err(|e| SourceError::Parse(format!("bad next-page link {next:?}: {e}")))?;
        }
        if cards.is_empty() {
            return Err(SourceError::Parse(
                "no event cards found on the listing".into(),
            ));
        }
        let mut out = Vec::new();
        let mut details = 0;
        for (card_url, mut raw) in cards {
            // A card that fails to classify is kept so that normalise reports it.
            let category = classify(&raw.payload).ok().and_then(|c| c.category);
            if !category.is_some_and(|c| c != Category::Exhibition) {
                out.push(raw);
                continue;
            }
            if details == MAX_DETAIL_PAGES {
                continue;
            }
            details += 1;
            let id = &raw.source_event_id;
            match ctx.get_text(&card_url).await {
                Ok(html) => match parse_detail_location(&html) {
                    Some(location) => {
                        raw.payload["location"] = json!(location);
                        out.push(raw);
                    }
                    None => ctx.report_error(format!("{id}: no location on the detail page")),
                },
                Err(e) => ctx.report_error(format!("{id}: {e}")),
            }
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
    use crate::model::Price;
    use rust_decimal::Decimal;

    fn card(event_type: &str, starts: &str, ends: Option<&str>) -> Value {
        json!({
            "title": "Something",
            "event_type": event_type,
            "starts": starts,
            "ends": ends,
        })
    }

    #[test]
    fn card_times_are_utc_instants() {
        // Soane Late: "Timed entry at 6pm, …" on 30 Oct 2026 (GMT).
        let late = normalise_payload(&card("Soane Lates", "2026-10-30T18:00:00Z", None))
            .unwrap()
            .unwrap();
        assert_eq!(late.starts_at.to_rfc3339(), "2026-10-30T18:00:00+00:00");
        // Study group talk: "18:00 - 19:30" on 30 Sep 2026 (BST).
        let talk = normalise_payload(&card("Talks", "2026-09-30T17:00:00Z", None))
            .unwrap()
            .unwrap();
        assert_eq!(
            talk.starts_at
                .with_timezone(&chrono_tz::Europe::London)
                .format("%H:%M")
                .to_string(),
            "18:00"
        );
        assert_eq!(talk.ends_at, None);
    }

    #[test]
    fn exhibitions_run_from_london_midnight() {
        // "14 Oct 2026 - 17 Jan 2027"
        let event = normalise_payload(&card(
            "Exhibitions",
            "2026-10-14T10:24:58Z",
            Some("2027-01-17T11:24:58Z"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-13T23:00:00+00:00");
        assert_eq!(
            event.ends_at.unwrap().to_rfc3339(),
            "2027-01-17T00:00:00+00:00"
        );
    }

    #[test]
    fn category_rules() {
        assert_eq!(category("Exhibitions", true), Some(Category::Exhibition));
        assert_eq!(category("Talks", false), Some(Category::Talk));
        assert_eq!(category("Workshops", false), Some(Category::Workshop));
        assert_eq!(
            category("Courses and Classes", false),
            Some(Category::Workshop)
        );
        assert_eq!(category("Soane Lates", false), Some(Category::Community));
        assert_eq!(category("Families", false), Some(Category::Community));
        assert_eq!(category("Families", true), None);
        assert_eq!(category("Tours", false), None);
        assert_eq!(category("Talks", true), None);
        assert_eq!(category("Something new", false), None);
    }

    #[test]
    fn online_exhibitions_are_skipped() {
        let mut payload = card(
            "Exhibitions",
            "2026-06-18T23:00:00Z",
            Some("2027-01-01T16:52:39Z"),
        );
        assert!(normalise_payload(&payload).unwrap().is_some());
        payload["teaser"] = json!("This online exhibition showcases work produced by…");
        assert!(normalise_payload(&payload).unwrap().is_none());
        payload["teaser"] = json!(null);
        payload["price_text"] = json!("Online only; free to explore.");
        assert!(normalise_payload(&payload).unwrap().is_none());
    }

    #[test]
    fn off_site_locations_are_the_venue() {
        let mut payload = card("Talks", "2026-11-24T18:30:00Z", None);
        payload["location"] = json!(
            "Royal Academy of Arts (Burlington Gardens entrance), 6 Burlington Gardens, Mayfair, London, W1J 0PE"
        );
        let event = normalise_payload(&payload).unwrap().unwrap();
        assert_eq!(event.venue_name.as_deref(), Some("Royal Academy of Arts"));
        assert_eq!(
            event.address.as_deref(),
            Some("6 Burlington Gardens, Mayfair, London, W1J 0PE")
        );
        assert_eq!((event.lat, event.lng), (None, None));

        for at_museum in [
            "Art Room, No. 14 Lincoln’s Inn Fields",
            "Sir John Soane's Museum",
        ] {
            payload["location"] = json!(at_museum);
            let event = normalise_payload(&payload).unwrap().unwrap();
            assert_eq!(event.venue_name.as_deref(), Some(VENUE_NAME), "{at_museum}");
            assert_eq!(event.lat, Some(VENUE_LAT));
        }
    }

    #[test]
    fn detail_location_must_name_the_museum_or_have_a_postcode() {
        let sidebar = |last: &str| {
            parse_detail_location(&format!(
                r#"<aside class="o-sidebar__info-box"><p>11am - 3pm</p><p>{last}</p></aside>"#
            ))
        };
        for location in [
            "Art Room at No. 14 Lincoln's Inn Fields.",
            "Sir John Soane's Museum",
            "Royal Academy of Arts, 6 Burlington Gardens, Mayfair, London, W1J 0PE",
        ] {
            assert_eq!(sidebar(location).as_deref(), Some(location));
        }
        for not_a_location in ["£12 p/p", "Suitable for ages 8-12", "18:00 - 19:30", ""] {
            assert_eq!(sidebar(not_a_location), None, "{not_a_location}");
        }
    }

    #[test]
    fn price_ignores_concessions() {
        let mut payload = card("Talks", "2026-11-24T18:30:00Z", None);
        payload["price_text"] = json!("Tickets: £15 (£5 students)");
        let price = normalise_payload(&payload).unwrap().unwrap().price;
        assert_eq!(price.min, Some(Decimal::new(15, 0)));
        assert_eq!(price.max, Some(Decimal::new(15, 0)));
        payload["price_text"] = json!(null);
        let price = normalise_payload(&payload).unwrap().unwrap().price;
        assert_eq!(price, Price::default());
    }

    #[test]
    fn bad_or_missing_times_are_errors() {
        for payload in [
            json!({"title": "No date", "event_type": "Talks"}),
            card("Talks", "soon", None),
            card(
                "Exhibitions",
                "2026-10-22T18:00:00Z",
                Some("2026-10-21T18:00:00Z"),
            ),
        ] {
            assert!(normalise_payload(&payload).is_err(), "{payload}");
        }
    }
}
