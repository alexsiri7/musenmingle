//! Headstone Manor & Museum (Harrow) — hand-written scraper over the "What's
//! on" listing and the detail pages of in-scope items.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   disallows only `/cpresources/`, `/vendor/`, `/.env` and `/cache/`, with
//!   no Crawl-delay; `/whats-on/…`, `/events/…` and `/exhibitions/…` are
//!   allowed.
//! * Detail pages carry one JSON-LD `Event` block per occurrence, but it is
//!   invalid JSON (raw newlines in `description`, a trailing comma in
//!   `location`), has no `endDate`, and exhibitions have none at all, so CSS
//!   selectors are used instead.
//! * `/whats-on/` is paginated (`/whats-on/page-2`, …) with an
//!   `a.c-pagination__next` link that has no `href` on the last page (an
//!   unreadable `href` is reported, not taken for the last page). Each
//!   card (`a.c-media--event`) has the title, one `<time datetime>` (start)
//!   or two (start, end) or free text with no `<time>`, and the genre
//!   (`Family Events`, `Events for Adults`, `Special Events`,
//!   `Exhibitions`). Every page repeats the same exhibitions block, so cards
//!   are de-duplicated by path. The site's links omit the trailing slash and
//!   redirect (301) to the slash form, so fetches add it. The end `<time>` is
//!   written `datetime="…"itemprop="endDate"`, so times are taken by order.
//! * The `datetime` offsets are genuine: "Tue 6 Oct, 2:00pm" is
//!   `2026-10-06T14:00:00+01:00`, and after the clocks go back the 25 Oct
//!   2026 cards switch to `+00:00`. Exhibitions show dates only (their times
//!   are London midnight), so both ends are stored as London midnight of
//!   their day (`all_day`).
//! * Category: cards with no `<time>` (programmes with free-text dates,
//!   undated exhibitions) are skipped; then exhibitions → exhibition. Other
//!   cards are skipped when they span more than one London day (weekly
//!   clubs, trails and workshops held on several days: a range of sessions,
//!   not one continuous event), and so are tours. Then `map_category` on the
//!   title and slug (`hmm-tuesday-talk-…` → talk, "… Workshop" → workshop),
//!   except that only the `/exhibitions/` path makes an exhibition (its
//!   times are dates); otherwise `Family Events` and `Events for Adults` →
//!   community, and anything else (`Special Events`: concerts and
//!   performances) is skipped.
//! * The detail page of every in-scope card is fetched for the description
//!   (the `#page-content` text blocks: all of them on untabbed pages such as
//!   exhibitions, only the "Details" tab's on tabbed ones) and the sidebar's
//!   Price, Venue (a room, kept in the payload only: every item is at the
//!   museum) and Duration. The "Booking info" tab (a £1 booking fee) never
//!   feeds the description or the price.

use async_trait::async_trait;
use chrono::{DateTime, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, is_london_midnight, london_date, london_to_utc,
    map_category, parse_datetime, parse_price,
};

pub const KEY: &str = "headstone-manor";
/// Upper bound on listing pages fetched per run.
pub const MAX_LISTING_PAGES: usize = 5;
/// Upper bound on detail pages fetched per run (≈ 60 s at 1 req / 2 s);
/// in-scope cards beyond it are left out of the run.
pub const MAX_DETAIL_PAGES: usize = 30;
const LISTING_PATH: &str = "/whats-on/";
const SITE: &str = "https://headstonemanor.org";
const VENUE_NAME: &str = "Headstone Manor & Museum";
const VENUE_ADDRESS: &str = "Headstone Recreation Ground, Pinner View, Harrow HA2 6PX";
/// Headstone Manor (OSM way 559262688).
const VENUE_LAT: f64 = 51.5947;
const VENUE_LNG: f64 = -0.3542;
const FAMILY_EVENTS: &str = "Family Events";

pub struct HeadstoneManor {
    base_url: Url,
}

impl HeadstoneManor {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// One listing card, as printed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Card {
    /// `/events/<slug>` or `/exhibitions/<slug>`, without a trailing slash.
    pub path: String,
    pub title: String,
    pub genre: Option<String>,
    /// The raw `datetime` of the card's first `<time>`.
    pub start: Option<String>,
    /// The raw `datetime` of the card's second `<time>`.
    pub end: Option<String>,
    pub image_url: Option<String>,
}

/// One page of the listing.
#[derive(Debug, serde::Serialize)]
pub struct Listing {
    pub cards: Vec<Card>,
    /// The next page's path (`/whats-on/page-N/`).
    pub next_path: Option<String>,
    /// Cards that could not be read, for the fetch to report.
    pub problems: Vec<String>,
}

/// What a detail page adds to its card.
#[derive(Debug, serde::Serialize)]
pub struct Detail {
    pub pre_title: Option<String>,
    pub description: Option<String>,
    pub price_text: Option<String>,
    pub room: Option<String>,
    pub duration: Option<String>,
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

fn site() -> Url {
    Url::parse(SITE).expect("valid url")
}

fn valid_path(path: &str) -> bool {
    ["/events/", "/exhibitions/"].iter().any(|prefix| {
        path.strip_prefix(prefix).is_some_and(|slug| {
            !slug.is_empty()
                && slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
    })
}

/// The site-relative path of an on-site link, without a trailing slash.
fn on_site_path(href: &str) -> Option<String> {
    let u = site().join(href).ok()?;
    (u.host_str() == Some("headstonemanor.org") && u.query().is_none())
        .then(|| u.path().trim_end_matches('/').to_string())
}

fn parse_card(a: ElementRef<'_>) -> Result<Card, String> {
    let href = a.value().attr("href").unwrap_or_default();
    let first = |s: &str| a.select(&selector(s)).next();
    let first_text = |s: &str| first(s).map(element_text).filter(|t| !t.is_empty());
    let title = first_text(".c-media__title");
    let path = on_site_path(href).filter(|p| valid_path(p));
    let (Some(path), Some(title)) = (path, title.clone()) else {
        return Err(format!(
            "unreadable card {:?} ({href:?})",
            title.unwrap_or_default()
        ));
    };
    let times: Vec<&str> = a
        .select(&selector(".c-media__posttitle time[datetime]"))
        .filter_map(|e| e.value().attr("datetime"))
        .collect();
    Ok(Card {
        path,
        title,
        genre: first_text(".c-media__category"),
        start: times.first().map(|t| t.to_string()),
        end: times.get(1).map(|t| t.to_string()),
        image_url: first(".c-media__image img[src]")
            .and_then(|e| e.value().attr("src"))
            .map(str::to_string),
    })
}

pub fn parse_listing(html: &str) -> Listing {
    let doc = Html::parse_document(html);
    let mut cards = Vec::new();
    let mut problems = Vec::new();
    for a in doc.select(&selector("a.c-media--event[href]")) {
        match parse_card(a) {
            Ok(card) => cards.push(card),
            Err(problem) => problems.push(problem),
        }
    }
    let next_href = doc
        .select(&selector("a.c-pagination__next[href]"))
        .next()
        .and_then(|a| a.value().attr("href"));
    let next_path = next_href
        .and_then(on_site_path)
        .filter(|p| p.starts_with("/whats-on/page-"))
        .map(|p| format!("{p}/"));
    if let (Some(href), None) = (next_href, &next_path) {
        problems.push(format!("unreadable next-page link {href:?}"));
    }
    Listing {
        cards,
        next_path,
        problems,
    }
}

pub fn parse_detail(html: &str) -> Detail {
    let doc = Html::parse_document(html);
    let text = |s: &str| {
        doc.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let meta = |key: &str| {
        doc.select(&selector("dl.c-meta__list .c-meta__item"))
            .find(|item| {
                item.select(&selector("dt.c-meta__key"))
                    .next()
                    .is_some_and(|dt| element_text(dt) == key)
            })
            .and_then(|item| item.select(&selector("dd.c-meta__value")).next())
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let blocks: Vec<String> = doc
        .select(&selector("#page-content .o-text-block"))
        .filter(|e| !in_tab_other_than_details(*e))
        .map(|e| e.inner_html())
        .collect();
    Detail {
        pre_title: text("h1 small.c-page-header__pre-title"),
        description: (!blocks.is_empty()).then(|| blocks.join("\n")),
        price_text: meta("Price"),
        room: meta("Venue"),
        duration: meta("Duration"),
    }
}

/// Pages with bookable dates split their content into tabs ("Details",
/// "Booking info"); the others have no tabs.
fn in_tab_other_than_details(e: ElementRef<'_>) -> bool {
    e.ancestors().filter_map(ElementRef::wrap).any(|a| {
        a.value().classes().any(|c| c == "o-tab") && a.value().id() != Some("content-details")
    })
}

fn parse_time(
    title: &str,
    key: &str,
    s: Option<&str>,
) -> Result<Option<DateTime<Utc>>, SourceError> {
    s.filter(|s| !s.trim().is_empty())
        .map(|s| {
            parse_datetime(s)
                .ok_or_else(|| SourceError::Parse(format!("{title:?}: bad {key} time {s:?}")))
        })
        .transpose()
}

fn is_tour(title: &str) -> bool {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "tour" || w == "tours")
}

fn classify(
    title: &str,
    genre: Option<&str>,
    path: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<Option<Category>, SourceError> {
    let start = parse_time(title, "start", start)?;
    let end = parse_time(title, "end", end)?;
    let Some(start) = start else {
        return Ok(None);
    };
    if path.starts_with("/exhibitions/") {
        return Ok(Some(Category::Exhibition));
    }
    if end.is_some_and(|e| london_date(e) != london_date(start)) || is_tour(title) {
        return Ok(None);
    }
    let slug_words = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .replace('-', " ");
    let mapped = [title, slug_words.as_str()]
        .into_iter()
        .find_map(|hint| map_category(&[hint]).filter(|c| *c != Category::Exhibition));
    Ok(mapped.or(match genre {
        Some(FAMILY_EVENTS | "Events for Adults") => Some(Category::Community),
        _ => None,
    }))
}

/// The in-scope category of a card, or `None` to skip it.
pub fn category(card: &Card) -> Result<Option<Category>, SourceError> {
    classify(
        &card.title,
        card.genre.as_deref(),
        &card.path,
        card.start.as_deref(),
        card.end.as_deref(),
    )
}

/// The [`RawEvent`] for a card, with its detail page if one was fetched.
pub fn card_event(card: &Card, base: &Url, detail: Option<&Detail>) -> RawEvent {
    let url = base
        .join(&card.path)
        .map(String::from)
        .unwrap_or_else(|_| format!("{SITE}{}", card.path));
    let mut payload = serde_json::to_value(card).expect("card serialises");
    payload["url"] = json!(url);
    if let Some(Value::Object(detail)) = detail.map(|d| serde_json::to_value(d).expect("detail")) {
        payload.as_object_mut().expect("object").extend(detail);
    }
    RawEvent {
        source_event_id: card.path.trim_matches('/').to_string(),
        source_url: Some(url),
        payload,
    }
}

fn london_midnight(t: DateTime<Utc>) -> DateTime<Utc> {
    london_to_utc(london_date(t).and_time(NaiveTime::MIN))
}

/// Normalise a Headstone Manor [`RawEvent`] payload (a card plus its detail
/// page).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("card without title".into()))?;
    let Some(category) = classify(
        &title,
        text("genre"),
        text("path").unwrap_or_default(),
        text("start"),
        text("end"),
    )?
    else {
        return Ok(None);
    };
    let starts_at = parse_time(&title, "start", text("start"))?
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no date")))?;
    let ends_at = parse_time(&title, "end", text("end"))?;
    let (starts_at, ends_at, all_day) = if category == Category::Exhibition {
        let starts_at = london_midnight(starts_at);
        let ends_at = ends_at.map(london_midnight).filter(|e| *e > starts_at);
        (starts_at, ends_at, true)
    } else {
        let ends_at = ends_at.filter(|e| *e > starts_at);
        let all_day = is_london_midnight(starts_at) && ends_at.is_none_or(is_london_midnight);
        (starts_at, ends_at, all_day)
    };

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(text("description")),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day,
        price: text("price_text").map(parse_price).unwrap_or_default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: if text("genre") == Some(FAMILY_EVENTS) {
            vec!["family".to_string()]
        } else {
            Vec::new()
        },
    }))
}

#[async_trait]
impl Source for HeadstoneManor {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let join = |path: &str| {
            self.base_url
                .join(path)
                .map_err(|e| SourceError::Config(e.to_string()))
        };
        let mut cards: Vec<Card> = Vec::new();
        let mut fetched: Vec<String> = Vec::new();
        let mut next = Some(LISTING_PATH.to_string());
        while let Some(path) = next.take() {
            if fetched.len() == MAX_LISTING_PAGES || fetched.contains(&path) {
                break;
            }
            let listing = parse_listing(&ctx.get_text(&join(&path)?).await?);
            if fetched.is_empty() && listing.cards.is_empty() {
                return Err(SourceError::Parse(format!(
                    "no event cards on {LISTING_PATH}"
                )));
            }
            fetched.push(path);
            for problem in listing.problems {
                ctx.report_error(problem);
            }
            for card in listing.cards {
                if !cards.iter().any(|c| c.path == card.path) {
                    cards.push(card);
                }
            }
            next = listing.next_path;
        }

        let mut out = Vec::new();
        let mut details = 0;
        for card in &cards {
            // A card that fails to classify is kept so that normalise reports it.
            if !matches!(category(card), Ok(Some(_))) {
                out.push(card_event(card, &self.base_url, None));
                continue;
            }
            if details == MAX_DETAIL_PAGES {
                continue;
            }
            details += 1;
            let result = match join(&format!("{}/", card.path)) {
                Ok(url) => ctx.get_text(&url).await.map_err(SourceError::from),
                Err(e) => Err(e),
            };
            match result {
                Ok(html) => out.push(card_event(card, &self.base_url, Some(&parse_detail(&html)))),
                Err(e) => ctx.report_error(format!("{}: {e}", card.path)),
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
    use rust_decimal::Decimal;

    fn card(path: &str, title: &str, genre: &str, start: Option<&str>, end: Option<&str>) -> Card {
        Card {
            path: path.into(),
            title: title.into(),
            genre: Some(genre.into()),
            start: start.map(str::to_string),
            end: end.map(str::to_string),
            image_url: None,
        }
    }

    fn event(card: &Card) -> NewEvent {
        let raw = card_event(card, &site(), None);
        normalise_payload(&raw.payload).unwrap().unwrap()
    }

    #[test]
    fn offsets_are_honoured_across_the_clock_change() {
        for (start, utc) in [
            ("2026-10-24T10:00:00+01:00", "2026-10-24T09:00:00+00:00"),
            ("2026-10-25T11:00:00+00:00", "2026-10-25T11:00:00+00:00"),
        ] {
            let e = event(&card(
                "/events/x",
                "Spooky Craft",
                FAMILY_EVENTS,
                Some(start),
                None,
            ));
            assert_eq!(e.starts_at.to_rfc3339(), utc);
            assert!(!e.all_day);
            assert_eq!(e.category, Category::Community);
            assert_eq!(e.tags, ["family"]);
        }
    }

    #[test]
    fn exhibitions_are_all_day_at_london_midnights() {
        let e = event(&card(
            "/exhibitions/x",
            "Roman Harrow",
            "Exhibitions",
            Some("2026-09-15T00:00:00+01:00"),
            Some("2026-12-20T00:00:00+00:00"),
        ));
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-09-14T23:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-12-20T00:00:00+00:00");

        let e = event(&card(
            "/exhibitions/x",
            "Roman Harrow",
            "Exhibitions",
            Some("2026-09-15T12:00:00+01:00"),
            Some("2026-09-15T17:00:00+01:00"),
        ));
        assert_eq!(e.starts_at.to_rfc3339(), "2026-09-14T23:00:00+00:00");
        assert_eq!(e.ends_at, None);
    }

    #[test]
    fn classification_rules() {
        let at = Some("2026-10-06T14:00:00+01:00");
        let later = Some("2026-11-26T12:00:00+00:00");
        let cases = [
            (
                card("/events/mums-club", "Mum's Club", FAMILY_EVENTS, at, later),
                None,
            ),
            (
                card("/events/x", "Festive Printing", FAMILY_EVENTS, None, None),
                None,
            ),
            (
                card(
                    "/events/x",
                    "Tuesday Tour: Halloween",
                    "Events for Adults",
                    at,
                    None,
                ),
                None,
            ),
            (
                card(
                    "/events/hmm-tuesday-talk-x",
                    "Music Hall",
                    "Events for Adults",
                    at,
                    None,
                ),
                Some(Category::Talk),
            ),
            (
                card(
                    "/events/x",
                    "Stained Glass Workshop",
                    "Events for Adults",
                    at,
                    None,
                ),
                Some(Category::Workshop),
            ),
            (
                card("/events/x", "Pond Dipping", FAMILY_EVENTS, at, None),
                Some(Category::Community),
            ),
            (
                card(
                    "/events/x",
                    "Memorabilia Session",
                    "Events for Adults",
                    at,
                    None,
                ),
                Some(Category::Community),
            ),
            (
                card(
                    "/events/x",
                    "Candlelight Concert",
                    "Special Events",
                    at,
                    None,
                ),
                None,
            ),
            (card("/events/x", "Something", "New Genre", at, None), None),
            (
                card(
                    "/exhibitions/x",
                    "Turning the Page",
                    "Exhibitions",
                    at,
                    later,
                ),
                Some(Category::Exhibition),
            ),
            (
                card(
                    "/events/x",
                    "Fine Art Printing",
                    FAMILY_EVENTS,
                    Some("2026-10-24T10:00:00+01:00"),
                    Some("2026-10-24T12:00:00+01:00"),
                ),
                Some(Category::Community),
            ),
            (
                card(
                    "/exhibitions/x",
                    "Permanent Display",
                    "Exhibitions",
                    None,
                    None,
                ),
                None,
            ),
            (
                card(
                    "/events/hmm-tuesday-talk-x",
                    "Exhibition Preview",
                    "Events for Adults",
                    at,
                    None,
                ),
                Some(Category::Talk),
            ),
        ];
        for (card, expected) in cases {
            assert_eq!(category(&card).unwrap(), expected, "{card:?}");
        }
    }

    #[test]
    fn exhibition_words_on_an_event_keep_its_times() {
        let e = event(&card(
            "/events/x",
            "Exhibition Preview",
            "Events for Adults",
            Some("2026-10-06T18:00:00+01:00"),
            Some("2026-10-06T20:00:00+01:00"),
        ));
        assert_eq!(e.category, Category::Community);
        assert!(!e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-06T17:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-06T19:00:00+00:00");
    }

    #[test]
    fn an_undated_exhibition_is_a_skip() {
        let raw = card_event(
            &card(
                "/exhibitions/x",
                "Permanent Display",
                "Exhibitions",
                None,
                None,
            ),
            &site(),
            None,
        );
        assert!(normalise_payload(&raw.payload).unwrap().is_none());
    }

    #[test]
    fn an_unreadable_next_link_is_a_problem() {
        let listing = parse_listing(
            r#"<html><body><a class="c-pagination__next" href="/whats-on/?page=2">Next</a></body></html>"#,
        );
        assert_eq!(listing.next_path, None);
        assert_eq!(listing.problems.len(), 1, "{:?}", listing.problems);
        let last =
            parse_listing(r#"<html><body><a class="c-pagination__next">Next</a></body></html>"#);
        assert!(last.problems.is_empty());
    }

    #[test]
    fn a_bad_start_is_an_error() {
        let bad = card("/events/x", "Talk", FAMILY_EVENTS, Some("6 Oct"), None);
        assert!(category(&bad).is_err());
        let raw = card_event(&bad, &site(), None);
        assert!(normalise_payload(&raw.payload).is_err());
    }

    #[test]
    fn price_and_description_skip_the_booking_info_tab() {
        let detail = parse_detail(
            r#"<html><body>
            <h1><small class="c-page-header__pre-title">Tuesday Talk</small>Music Hall</h1>
            <div id="page-content">
            <section class="o-tab" id="content-details"><div class="o-text-block"><p>A talk.</p></div></section>
            <section class="o-tab" id="content-booking-info"><div class="o-text-block"><p>A £1 non-refundable booking fee applies.</p></div></section>
            </div>
            <dl class="c-meta__list">
              <div class="c-meta__item"><dt class="c-meta__key">Venue</dt><dd class="c-meta__value">The Granary</dd></div>
              <div class="c-meta__item"><dt class="c-meta__key">Price</dt><dd class="c-meta__value">£4.50 per person</dd></div>
            </dl></body></html>"#,
        );
        assert_eq!(detail.price_text.as_deref(), Some("£4.50 per person"));
        assert_eq!(detail.room.as_deref(), Some("The Granary"));
        assert_eq!(detail.pre_title.as_deref(), Some("Tuesday Talk"));
        assert_eq!(detail.description.as_deref(), Some("<p>A talk.</p>"));

        let card = card(
            "/events/hmm-tuesday-talk-x",
            "Music Hall",
            "Events for Adults",
            Some("2026-10-06T14:00:00+01:00"),
            None,
        );
        let raw = card_event(&card, &site(), Some(&detail));
        let e = normalise_payload(&raw.payload).unwrap().unwrap();
        assert_eq!(e.price.min, Some(Decimal::new(450, 2)));
        assert_eq!(e.tags, Vec::<String>::new());

        let untabbed = parse_detail(
            r#"<html><body><div id="page-content"><section class="o-grid">
            <div class="o-text-block"><p>An exhibition.</p></div>
            <div class="o-text-block"><p>Sponsored by the Friends.</p></div>
            </section></div></body></html>"#,
        );
        assert_eq!(
            untabbed.description.as_deref(),
            Some("<p>An exhibition.</p>\n<p>Sponsored by the Friends.</p>")
        );
    }
}
