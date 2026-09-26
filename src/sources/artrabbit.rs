//! ArtRabbit — London listing of current contemporary-art shows, many of them
//! at small galleries no venue scraper covers. Facts + link only.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   disallows only technical paths (`/ajax/*.tpl`, `/account/`, `/*.php`,
//!   ...), so the `/all-shows/...` listing is allowed. (It blocks GPTBot and
//!   ClaudeBot by name; MuseNMingleBot is neither and parses deterministically.)
//! * **Terms** (<https://www.artrabbit.com/about/terms>, checked 2026-09-26)
//!   prohibit "reproducing, copying, editing, transmitting, uploading or
//!   incorporating into any other materials, any of the Website, including
//!   without limitation, any information, articles, photographs, images or
//!   submissions". The owner's decision: include ArtRabbit, respectfully,
//!   with facts + a link only. So this scraper takes only title, venue name,
//!   coordinates, dates and category from the listing cards; it never reads
//!   descriptions or image URLs (and the seed row sets `store_description` /
//!   `store_image` false, so the raw payload is redacted too).
//! * **Listing only.** `/all-shows/united-kingdom/london?page=N` is
//!   server-rendered, 20 cards per page, newest openings first (~18 pages,
//!   ~350 shows on 2026-09-26). Event detail pages sit behind a cookie
//!   check (302 to `?__cc=1`, then 403 without the cookie); we don't try to
//!   pass it, so there is no address, price or venue URL.
//! * **Politeness:** at most [`MAX_PAGES`] pages per run; the rate limit for
//!   `www.artrabbit.com` is at least 5 s per request
//!   (`config::BUILTIN_MIN_INTERVALS`); the seed row runs it daily.
//! * The page is London-scoped by ArtRabbit, whose "London" includes places
//!   such as Blackheath, so the card's city label is not used: a card is kept
//!   when its coordinates fall inside a Greater London bounding box.
//! * Card dates are date-only ranges (`26 Sep 2026 – 07 Nov 2026`), stored
//!   as London midnight of each day, as for the other exhibition sources.
//!   "Opening: Today, 17:00" is relative to the fetch time and is ignored.
//! * Shows running longer than [`MAX_RUN_DAYS`] (permanent collection
//!   displays, online shows ending "2030") are open-ended and skipped.
//! * Category: "Exhibition" → exhibition, "Art Fair" → expo; anything else
//!   is skipped.
//! * Many shows are also listed by a venue scraper; the cross-source merge
//!   (`repo::upsert_event` + `matching`) joins them (same dates, venue name
//!   or coordinates within 150 m, similar title).

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_text, dedupe_key, london_to_utc};

pub const KEY: &str = "artrabbit";
/// Upper bound on listing pages per run (≈ 100 s at 1 request / 5 s).
pub const MAX_PAGES: u32 = 20;
const LISTING_PATH: &str = "/all-shows/united-kingdom/london";
const SITE: &str = "https://www.artrabbit.com";
/// Listings running longer than this are permanent displays, public-art
/// programmes or online shows with placeholder end dates (e.g. 2020 → 2030),
/// i.e. open-ended programmes, which are skipped.
pub const MAX_RUN_DAYS: i64 = 730;
/// Greater London bounding box (lat_min, lat_max, lng_min, lng_max).
const LONDON_BBOX: (f64, f64, f64, f64) = (51.28, 51.70, -0.52, 0.34);
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

pub struct ArtRabbit {
    base_url: Url,
    max_pages: u32,
}

impl ArtRabbit {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            max_pages: MAX_PAGES,
        }
    }

    /// Same scraper with a different page cap (tests).
    pub fn with_max_pages(base_url: Url, max_pages: u32) -> Self {
        Self {
            base_url,
            max_pages,
        }
    }
}

/// One parsed listing page.
#[derive(Debug)]
pub struct ListingPage {
    pub events: Vec<RawEvent>,
    /// The page links to a next page.
    pub has_next: bool,
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// Parse one listing page: one [`RawEvent`] per show card (facts only), keyed
/// by ArtRabbit's numeric id (`data-ident`).
pub fn parse_listing(html: &str) -> ListingPage {
    let doc = Html::parse_document(html);
    let site = Url::parse(SITE).expect("valid url");
    let first_text = |card: ElementRef<'_>, s: &str| {
        card.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let mut events = Vec::new();
    for card in doc.select(&selector("article.m_listing-item[data-ident]")) {
        let attr = |name: &str| card.value().attr(name).map(str::trim);
        let Some(id) = attr("data-ident").filter(|i| i.chars().all(|c| c.is_ascii_digit())) else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        let Some(url) = card
            .select(&selector("a.m_listing-link[href]"))
            .next()
            .and_then(|a| a.value().attr("href"))
            .and_then(|h| site.join(h).ok())
            .filter(|u| u.path().starts_with("/events/"))
        else {
            continue;
        };
        let coord = |name: &str| attr(name).and_then(|v| v.parse::<f64>().ok());
        // Venue then place ("London, United Kingdom"); the venue <p> is
        // present but empty when the listing has no venue.
        let lines: Vec<String> = card
            .select(&selector("p.b_instructional-text"))
            .map(element_text)
            .collect();
        events.push(RawEvent {
            source_event_id: id.to_string(),
            source_url: Some(url.to_string()),
            payload: json!({
                "url": url.as_str(),
                "title": first_text(card, "p.b_small-heading.mod--primary"),
                "category": first_text(card, "p.b_categorical-heading"),
                "date_text": first_text(card, "p.b_small-heading.mod--colour"),
                "venue": lines.first().filter(|v| !v.is_empty()),
                "place": lines.get(1).filter(|v| !v.is_empty()),
                "lat": coord("data-lat"),
                "lng": coord("data-lon"),
            }),
        });
    }
    let has_next = doc
        .select(&selector(
            ".m_pagination a.m_pagination-control.mod--next[href]",
        ))
        .next()
        .is_some();
    ListingPage { events, has_next }
}

/// "26 Sep 2026 – 07 Nov 2026" (or a single "26 Sep 2026") → first and last
/// day. `Ok(None)` for open-ended text ("Ongoing", "Until …", "From …").
pub fn parse_date_range(text: &str) -> Result<Option<(NaiveDate, NaiveDate)>, SourceError> {
    let lower = text.to_lowercase();
    if ["until", "from", "ongoing", "open", "permanent"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return Ok(None);
    }
    let err = || SourceError::Parse(format!("unrecognised date range {text:?}"));
    let (start, end) = match ['–', '—', '-']
        .iter()
        .find_map(|sep| lower.split_once(*sep))
    {
        Some((a, b)) => (parse_day(a).ok_or_else(err)?, parse_day(b).ok_or_else(err)?),
        None => {
            let d = parse_day(&lower).ok_or_else(err)?;
            (d, d)
        }
    };
    if start > end {
        return Err(err());
    }
    Ok(Some((start, end)))
}

/// "26 sep 2026" (lower-cased) → date. Months match on their first three
/// letters, so "sept" and "september" are accepted.
fn parse_day(s: &str) -> Option<NaiveDate> {
    let [day, month, year] = s.split_whitespace().collect::<Vec<_>>()[..] else {
        return None;
    };
    let month = MONTHS.iter().position(|p| month.get(..3) == Some(*p))? as u32 + 1;
    NaiveDate::from_ymd_opt(year.parse().ok()?, month, day.parse().ok()?)
}

fn in_london(lat: f64, lng: f64) -> bool {
    let (lat_min, lat_max, lng_min, lng_max) = LONDON_BBOX;
    (lat_min..=lat_max).contains(&lat) && (lng_min..=lng_max).contains(&lng)
}

/// Normalise an ArtRabbit card payload. Out-of-scope categories, cards
/// outside Greater London (or without coordinates), open-ended dates and
/// runs longer than [`MAX_RUN_DAYS`] are skips (`Ok(None)`).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|t| !t.is_empty())
    };
    let title = text("title").ok_or_else(|| SourceError::Parse("card without title".into()))?;
    let category = match text("category").as_deref() {
        Some("Exhibition") => Category::Exhibition,
        Some("Art Fair") => Category::Expo,
        _ => return Ok(None),
    };
    let coord = |k: &str| payload.get(k).and_then(Value::as_f64);
    let (Some(lat), Some(lng)) = (coord("lat"), coord("lng")) else {
        return Ok(None);
    };
    if !in_london(lat, lng) {
        return Ok(None);
    }
    let date_text = text("date_text")
        .ok_or_else(|| SourceError::Parse(format!("card {title:?} without dates")))?;
    let Some((first_day, last_day)) = parse_date_range(&date_text)? else {
        return Ok(None);
    };
    if (last_day - first_day).num_days() > MAX_RUN_DAYS {
        return Ok(None);
    }
    let starts_at = london_to_utc(first_day.and_time(NaiveTime::MIN));
    let ends_at = london_to_utc(last_day.and_time(NaiveTime::MIN));
    let venue = text("venue");

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, venue.as_deref()),
        title,
        description: None,
        venue_name: venue,
        address: None,
        lat: Some(lat),
        lng: Some(lng),
        starts_at,
        ends_at: Some(ends_at).filter(|e| *e > starts_at),
        price: Price::default(),
        // The ArtRabbit page is the listing's `source_url` (the "See it on
        // ArtRabbit" link); `url` is left to the venue's own site.
        url: None,
        image_url: None,
        category,
        tags: vec!["art".to_string()],
    }))
}

#[async_trait]
impl Source for ArtRabbit {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listing = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for page in 1..=self.max_pages {
            let mut url = listing.clone();
            if page > 1 {
                url.set_query(Some(&format!("page={page}")));
            }
            let html = match ctx.get_text(&url).await {
                Ok(h) => h,
                // The first page failing fails the run; a later page is a
                // soft error and ends this run's paging (no retries).
                Err(e) if page == 1 => return Err(e.into()),
                Err(e) => {
                    ctx.report_error(format!("listing page {page}: {e}"));
                    break;
                }
            };
            let parsed = parse_listing(&html);
            if page == 1 && parsed.events.is_empty() {
                return Err(SourceError::Parse(
                    "no show cards found on the first listing page".into(),
                ));
            }
            for raw in parsed.events {
                // Paging over a live list can repeat a card.
                if seen.insert(raw.source_event_id.clone()) {
                    out.push(raw);
                }
            }
            if !parsed.has_next {
                break;
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

    fn range(text: &str) -> Option<(String, String)> {
        parse_date_range(text)
            .unwrap()
            .map(|(a, b)| (a.to_string(), b.to_string()))
    }

    fn pair(a: &str, b: &str) -> Option<(String, String)> {
        Some((a.into(), b.into()))
    }

    fn card(overrides: Value) -> Value {
        let mut base = json!({
            "url": "https://www.artrabbit.com/events/example",
            "title": "Example Show",
            "category": "Exhibition",
            "date_text": "26 Sep 2026 – 07 Nov 2026",
            "venue": "Example Gallery",
            "place": "London, United Kingdom",
            "lat": 51.5254625,
            "lng": -0.0840522,
        });
        for (k, v) in overrides.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    #[test]
    fn parses_card_date_ranges() {
        assert_eq!(
            range("26 Sep 2026 – 07 Nov 2026"),
            pair("2026-09-26", "2026-11-07")
        );
        assert_eq!(
            range("25 Sep 2026 - 14 Aug 2027"),
            pair("2026-09-25", "2027-08-14")
        );
        assert_eq!(range("3 Oct 2026"), pair("2026-10-03", "2026-10-03"));
        assert_eq!(
            range("1 September 2026 — 3 October 2026"),
            pair("2026-09-01", "2026-10-03")
        );
    }

    #[test]
    fn open_ended_and_bad_ranges() {
        for text in ["Ongoing", "Until 7 Nov 2026", "From 3 Oct 2026"] {
            assert_eq!(range(text), None, "{text}");
        }
        for text in [
            "Autumn 2026",
            "7 Nov 2026 – 26 Sep 2026",
            "26 Sep – 7 Nov 2026",
        ] {
            assert!(parse_date_range(text).is_err(), "{text}");
        }
    }

    #[test]
    fn keeps_only_facts() {
        let e = normalise_payload(&card(json!({}))).unwrap().unwrap();
        assert_eq!(e.category, Category::Exhibition);
        assert_eq!(e.description, None);
        assert_eq!(e.image_url, None);
        assert_eq!(e.address, None);
        assert_eq!(e.price, Price::default());
        assert_eq!(e.venue_name.as_deref(), Some("Example Gallery"));
        assert_eq!(e.starts_at.to_rfc3339(), "2026-09-25T23:00:00+00:00");
        assert_eq!(
            e.ends_at.map(|d| d.to_rfc3339()).as_deref(),
            Some("2026-11-07T00:00:00+00:00")
        );
    }

    #[test]
    fn art_fairs_are_expos_and_other_categories_skip() {
        let fair = normalise_payload(&card(json!({"category": "Art Fair"})))
            .unwrap()
            .unwrap();
        assert_eq!(fair.category, Category::Expo);
        for c in ["Event", "Screening", "Performance"] {
            assert!(
                normalise_payload(&card(json!({"category": c})))
                    .unwrap()
                    .is_none(),
                "{c}"
            );
        }
    }

    #[test]
    fn outside_london_or_without_coordinates_is_skipped() {
        // Brighton; and a Blackheath card (still London) is kept.
        let brighton = card(json!({"lat": 50.8225, "lng": -0.1372, "place": "Brighton"}));
        assert!(normalise_payload(&brighton).unwrap().is_none());
        let no_coords = card(json!({"lat": null, "lng": null}));
        assert!(normalise_payload(&no_coords).unwrap().is_none());
        let blackheath = card(json!({"lat": 51.4680034, "lng": 0.0039603,
                                     "place": "Blackheath, United Kingdom"}));
        assert!(normalise_payload(&blackheath).unwrap().is_some());
    }

    #[test]
    fn permanent_displays_are_skipped() {
        for text in ["13 May 2020 – 31 Dec 2030", "31 May 2025 – 29 Jun 2030"] {
            assert!(
                normalise_payload(&card(json!({"date_text": text})))
                    .unwrap()
                    .is_none(),
                "{text}"
            );
        }
        // A long commission (just under a year) and a two-year run are kept.
        for text in ["25 Sep 2026 – 14 Aug 2027", "1 Jan 2026 – 1 Jan 2028"] {
            assert!(
                normalise_payload(&card(json!({"date_text": text})))
                    .unwrap()
                    .is_some(),
                "{text}"
            );
        }
    }

    #[test]
    fn single_day_show_has_no_end() {
        let e = normalise_payload(&card(json!({"date_text": "3 Oct 2026"})))
            .unwrap()
            .unwrap();
        assert_eq!(e.ends_at, None);
    }

    #[test]
    fn missing_venue_is_none() {
        let e = normalise_payload(&card(json!({"venue": null})))
            .unwrap()
            .unwrap();
        assert_eq!(e.venue_name, None);
        assert!(e.dedupe_key.ends_with("|unknown"), "{}", e.dedupe_key);
    }
}
