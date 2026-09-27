//! V&A (South Kensington, V&A East Museum, V&A East Storehouse, Young V&A) —
//! scraper over the schema.org microdata on the `/whatson` listing.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`,
//!   `Crawl-delay: 2`; `/whatson` is allowed, but pagination (`/*page=`,
//!   `/*p=`) and search (`/*q=*`) are disallowed, so the source reads only
//!   the first listing page (~85 items), without a query string. One request
//!   per run; the listing has every field needed, so no detail pages.
//! * Every card is an `article[itemprop=event]` with `name`, `startDate`,
//!   `endDate`, `description`, `image` and an `Offer` (`price` is `5.0`,
//!   `0`, `£6.00` or `£1,600.00`). The microdata `location` is always
//!   "Victoria and Albert Museum, South Kensington", even for online and V&A
//!   East events, so the venue comes from the card's visible pin label
//!   instead; only the four London sites are kept (online, "No venue", V&A
//!   Dundee, Wedgwood → skipped).
//! * Times: exhibitions (`/exhibitions/`, `/season/`, `/festival/`) have
//!   date-only `startDate`/`endDate`: all day, both ends London midnight.
//!   `/event/` items have `2026-10-01 14:00:00 +0100`, but the site renders
//!   London wall-clock time as if it were UTC: in summer the stamp is one
//!   hour late. Verified 2026-09-27 against the event pages: the lunchtime
//!   lecture of 1 October (`14:00:00 +0100`) says "13.00 – 14.00", and Sandy
//!   Powell's talk on 28 September (`20:00:00 +0100` – `21:45:00 +0100`)
//!   says "Talk 19:00 - 20:00 … closing 20:45". Winter stamps are right
//!   (`14:00:00 +0000` for the 14:00 library talks). [`parse_vam_time`] reads
//!   the stamp's UTC instant as London wall-clock time, which fixes both.
//! * Displays on `/event/` pages carry opening hours as times over months;
//!   like other exhibitions they are all day over their London dates.
//! * Categories from the card's type label: display/exhibition/season →
//!   exhibition; festival/special event → community; talks and one-day
//!   courses → talk; workshop → workshop. Skipped (`Ok(None)`): tours, film
//!   screenings, year courses, drop-in play, online/livestream/recording/
//!   on-demand items, members-only, schools and educators' events, recurring
//!   series ("Every Monday …"), open-ended displays ("Now open" without a
//!   closing date) and timed non-exhibition items spanning several days
//!   (session series whose start and end are months apart).
//! * Content policy (seed): facts + link only; see the migration.

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, price_from_amounts,
};

pub const KEY: &str = "vam";
const LISTING_PATH: &str = "/whatson";
const SITE: &str = "https://www.vam.ac.uk";

/// The London sites, by the card's pin label: (label, address, lat, lng).
/// Addresses from the site footer (2026-09-27); coordinates approximate.
const VENUES: &[(&str, &str, f64, f64)] = &[
    (
        "V&A South Kensington",
        "Cromwell Road, London SW7 2RL",
        51.4966,
        -0.1722,
    ),
    (
        "V&A East Museum",
        "East Bank, 107 Carpenters Rd, Queen Elizabeth Olympic Park, Stratford, London E20 2AR",
        51.5396,
        -0.0107,
    ),
    (
        "V&A East Storehouse",
        "2 Parkes Street, London E20 3AX",
        51.5460,
        -0.0225,
    ),
    (
        "Young V&A",
        "Cambridge Heath Rd, Bethnal Green, London E2 9PA",
        51.5291,
        -0.0553,
    ),
];

/// Type labels skipped outright.
const SKIP_TYPES: &[&str] = &["tour", "film", "online", "year course", "drop-in"];
/// Title/type words marking online, members-only or closed-audience items.
const SKIP_WORDS: &[&str] = &[
    "livestream",
    "recording",
    "on-demand",
    "online",
    "members'",
    "schools",
    "educators",
];

pub struct Vam {
    base_url: Url,
}

impl Vam {
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

/// `content` of the first `meta[itemprop=<prop>]` directly describing `card`
/// (not the nested `Place`).
fn card_meta(card: ElementRef<'_>, prop: &str) -> Option<String> {
    card.children()
        .filter_map(ElementRef::wrap)
        .find(|e| e.value().name() == "meta" && e.value().attr("itemprop") == Some(prop))
        .and_then(|e| e.value().attr("content"))
        .map(str::to_string)
}

/// Parse the `/whatson` listing into one [`RawEvent`] per card, in document
/// order. Cards without a link or name are dropped.
pub fn parse_listing(html: &str) -> Vec<RawEvent> {
    let doc = Html::parse_document(html);
    let base = Url::parse(SITE).expect("valid url");
    let link_sel = selector("a.b-event-teaser__link[href]");
    let type_sel = selector(".b-event-teaser__type");
    let date_sel = selector(".b-icon-list__icon--calendar .b-icon-list__item-text");
    let pin_sel = selector(".b-icon-list__icon--pin .b-icon-list__item-text");
    let offer_sel = selector("[itemprop=offers]");
    let price_sel = selector("[itemprop=price]");
    let currency_sel = selector("meta[itemprop=priceCurrency]");
    let image_sel = selector("img[itemprop=image]");
    let mut out = Vec::new();
    for card in doc.select(&selector("article[itemprop=event]")) {
        let Some(url) = card
            .select(&link_sel)
            .next()
            .and_then(|a| a.value().attr("href"))
            .and_then(|h| base.join(h).ok())
        else {
            continue;
        };
        let Some(title) = card_meta(card, "name")
            .map(|t| clean_text(&t))
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        let text = |sel: &Selector| card.select(sel).next().map(element_text);
        let offer = card.select(&offer_sel).next();
        let price = offer
            .and_then(|o| o.select(&price_sel).next())
            .and_then(|p| p.value().attr("content"))
            .map(str::to_string);
        let currency = offer
            .and_then(|o| o.select(&currency_sel).next())
            .and_then(|c| c.value().attr("content"))
            .map(str::to_string);
        let image_url = card
            .select(&image_sel)
            .next()
            .and_then(|i| i.value().attr("src"))
            .and_then(|s| base.join(s).ok())
            .map(String::from);
        let path = url.path().to_string();
        out.push(RawEvent {
            source_event_id: path.trim_matches('/').to_string(),
            source_url: Some(url.to_string()),
            payload: json!({
                "url": url.to_string(),
                "title": title,
                "type": text(&type_sel),
                "date_text": text(&date_sel),
                "venue": text(&pin_sel),
                "start": card_meta(card, "startDate"),
                "end": card_meta(card, "endDate"),
                "description": card_meta(card, "description"),
                "price": price,
                "currency": currency,
                "image_url": image_url,
            }),
        });
    }
    out
}

/// A V&A `/event/` time stamp (`2026-10-01 14:00:00 +0100`). The digits
/// are one hour late in summer: the stamp's UTC instant is the London
/// wall-clock time (see the module docs), so it is read as that.
pub fn parse_vam_time(s: &str) -> Option<DateTime<Utc>> {
    let t = DateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S %z").ok()?;
    Some(london_to_utc(t.naive_utc()))
}

/// Offer price (`5.0`, `0`, `£6.00`, `£1,600.00`) → [`Price`].
pub fn parse_offer_price(amount: Option<&str>, currency: Option<&str>) -> Price {
    let Some(amount) = amount else {
        return Price::default();
    };
    let digits: String = amount
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    match Decimal::from_str(&digits) {
        Ok(d) => {
            let currency = currency.or(amount.contains('£').then_some("GBP"));
            price_from_amounts(Some(d), None, currency)
        }
        Err(_) => Price::default(),
    }
}

fn category_for(kind: &str) -> Option<Category> {
    let k = kind.to_lowercase();
    if ["display", "exhibition", "season"].contains(&k.as_str()) {
        Some(Category::Exhibition)
    } else if k == "festival" || k == "special event" {
        Some(Category::Community)
    } else if k.contains("talk") || k.contains("lecture") || k.contains("one-day course") {
        Some(Category::Talk)
    } else if k.contains("workshop") {
        Some(Category::Workshop)
    } else {
        None
    }
}

fn london_midnight(d: NaiveDate) -> DateTime<Utc> {
    london_to_utc(d.and_time(NaiveTime::MIN))
}

fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

/// Normalise a V&A [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let title = str_field(payload, "title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("card without title".into()))?;
    let kind = str_field(payload, "type").unwrap_or_default();
    let date_text = str_field(payload, "date_text").unwrap_or_default();

    let Some(&(venue, address, lat, lng)) = VENUES
        .iter()
        .find(|v| Some(v.0) == str_field(payload, "venue"))
    else {
        return Ok(None); // online, "No venue" or outside London
    };
    let kind_lower = kind.to_lowercase();
    let title_lower = title.to_lowercase();
    if SKIP_TYPES.iter().any(|t| kind_lower.contains(t))
        || SKIP_WORDS
            .iter()
            .any(|w| title_lower.contains(w) || kind_lower.contains(w))
        || date_text.to_lowercase().starts_with("every ")
    {
        return Ok(None);
    }
    let Some(category) = category_for(kind) else {
        return Ok(None);
    };

    let start = str_field(payload, "start")
        .ok_or_else(|| SourceError::Parse(format!("{title}: no startDate")))?;
    let end = str_field(payload, "end");
    let bad = |s: &str| SourceError::Parse(format!("{title}: unrecognised date {s:?}"));
    let (starts_at, ends_at, all_day) =
        if let Ok(first) = NaiveDate::parse_from_str(start, "%Y-%m-%d") {
            if date_text.eq_ignore_ascii_case("now open") {
                return Ok(None); // open-ended; the end date is a placeholder
            }
            let last = match end {
                Some(e) => NaiveDate::parse_from_str(e, "%Y-%m-%d").map_err(|_| bad(e))?,
                None => first,
            };
            (london_midnight(first), Some(london_midnight(last)), true)
        } else {
            let s = parse_vam_time(start).ok_or_else(|| bad(start))?;
            let e = match end {
                Some(e) => Some(parse_vam_time(e).ok_or_else(|| bad(e))?),
                None => None,
            };
            if category == Category::Exhibition {
                let last = london_date(e.unwrap_or(s));
                (
                    london_midnight(london_date(s)),
                    Some(london_midnight(last)),
                    true,
                )
            } else if e.is_some_and(|e| london_date(e) != london_date(s)) {
                return Ok(None); // a series of sessions over several days
            } else {
                (s, e, false)
            }
        };
    if ends_at.is_some_and(|e| e < starts_at) {
        return Err(SourceError::Parse(format!(
            "{title}: ends before it starts"
        )));
    }

    let tags = if kind.is_empty() {
        Vec::new()
    } else {
        vec![kind_lower]
    };
    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(venue)),
        description: clean_description(str_field(payload, "description")),
        title,
        venue_name: Some(venue.to_string()),
        address: Some(address.to_string()),
        lat: Some(lat),
        lng: Some(lng),
        starts_at,
        ends_at: ends_at.filter(|e| *e > starts_at),
        all_day,
        price: parse_offer_price(str_field(payload, "price"), str_field(payload, "currency")),
        url: str_field(payload, "url").map(str::to_string),
        image_url: str_field(payload, "image_url").map(str::to_string),
        category,
        tags,
    }))
}

#[async_trait]
impl Source for Vam {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let raws = parse_listing(&ctx.get_text(&url).await?);
        if raws.is_empty() {
            return Err(SourceError::Parse(
                "no microdata events found on /whatson".into(),
            ));
        }
        Ok(raws)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> Option<String> {
        parse_vam_time(s).map(|t| t.to_rfc3339())
    }

    #[test]
    fn summer_stamps_are_an_hour_late() {
        // Sandy Powell talk, page: "Talk 19:00 - 20:00 … closing 20:45".
        assert_eq!(
            utc("2026-09-28 20:00:00 +0100").as_deref(),
            Some("2026-09-28T18:00:00+00:00")
        );
        assert_eq!(
            utc("2026-09-28 21:45:00 +0100").as_deref(),
            Some("2026-09-28T19:45:00+00:00")
        );
        // Lunchtime lecture, page: "13.00 – 14.00".
        assert_eq!(
            utc("2026-10-01 14:00:00 +0100").as_deref(),
            Some("2026-10-01T12:00:00+00:00")
        );
    }

    #[test]
    fn winter_stamps_are_right() {
        assert_eq!(
            utc("2026-12-28 14:00:00 +0000").as_deref(),
            Some("2026-12-28T14:00:00+00:00")
        );
        assert_eq!(utc("2026-10-01"), None);
    }

    #[test]
    fn offer_prices() {
        let p = parse_offer_price(Some("£1,600.00"), None);
        assert_eq!(p.min, Some(Decimal::from(1600)));
        assert_eq!(p.currency.as_deref(), Some("GBP"));
        assert!(!p.is_free);
        assert!(parse_offer_price(Some("0"), None).is_free);
        assert!(parse_offer_price(Some("0.0"), Some("GBP")).is_free);
        let p = parse_offer_price(Some("5.0"), Some("GBP"));
        assert_eq!(p.min, Some(Decimal::from(5)));
        assert_eq!(parse_offer_price(None, Some("GBP")), Price::default());
    }
}
