//! Barbican Centre — hand-written scraper over the "What's on" pages' HTML.
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *`
//!   disallows only Drupal internals (`/core/`, `/admin/`, `/search`,
//!   `/taxonomy/`, …); `/whats-on/…` is allowed and there is no Crawl-delay.
//! * There is no JSON-LD at all, so CSS selectors are used. `/whats-on` itself
//!   is a day-by-day listing dominated by cinema, so the two art-form
//!   listings that hold in-scope events are crawled instead: art & design and
//!   talks & events. ("Take part" adds only club nights and gigs beyond what
//!   talks & events lists.) Each is a Drupal view paginated with a
//!   `a[rel=next]` "Load More" link (`?page=1`, `?page=2`, …). Cards are
//!   `article.listing--event`; only `/whats-on/<year>/event/<slug>` links are
//!   kept (series pages are hubs). The same slug can exist under two years
//!   (a tour repeated in 2026 and 2027), so the source id is `<year>/<slug>`.
//!   Each card's image and title are kept for its detail page.
//! * The title is the listing card's (`h2.listing-title`): the detail page
//!   splits it into an h1 and an h2 whose role varies (the rest of the title,
//!   a series name that goes first, a promoter, a tagline), so they can't be
//!   rejoined by a rule. Card titles cut short with `...` fall back to the h1.
//! * Every detail page is fetched. Times come from the header's
//!   `.event-byline .date-range time[datetime]` attributes, which are genuine
//!   UTC: "Thu 22 Oct 2026, 19:00" is `2026-10-22T18:00:00Z` (BST) and
//!   "Fri 6 Nov 2026, 18:30" is `2026-11-06T18:30:00Z` (GMT). A range has two
//!   `<time>`s (first opening, last closing); a single slot has one.
//! * Category comes from the page's art-form tag buttons (`/whats-on/<form>`
//!   links), checked in order: cinema → skipped (screenings, even when also
//!   filed under talks); talks & events plus take part → community (Young
//!   Barbican, festivals); talks & events → talk (including talks also filed
//!   under music); art & design spanning more than one London day →
//!   exhibition. Anything else is skipped: single-slot art & design items are
//!   concerts, performances and gallery tours, and take part on its own or
//!   the other art forms are club nights, gigs and shows. `qa_scope` tells
//!   the scraper check the same.
//! * Price is the first ticket-price row only ("Standard £20.50 (£19 + £1.50
//!   transaction fee)" → £20.50, or a bare "Free"); the other rows are
//!   member and concession prices ("Free entry", "Free"). Pages without a
//!   ticket-price block are free only when their description says so
//!   explicitly ("This free installation…", see
//!   `normalise::describes_free_entry`); otherwise the price is unknown.
//! * The image is the listing card's JPEG (`img[src]`, the 728×509
//!   `event_listing_small_jpg` style). The detail page's `og:image` is an AVIF
//!   derivative, which the thumbnailer cannot decode, so it is only a fallback
//!   when it is not AVIF.
//! * Every item is placed at the Barbican Centre; the room ("Art Gallery",
//!   "The Pit") is kept in the payload only. An off-site item would get the
//!   wrong venue.

use std::collections::BTreeMap;

use async_trait::async_trait;
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, describes_free_entry, is_date_only, london_date,
    parse_datetime, parse_price,
};

pub const KEY: &str = "barbican";
/// Upper bound on detail pages fetched per run (≈ 115 s with the listing
/// pages at 1 req / 2 s).
/// Listings are crawled in the order below, so exhibitions come first and
/// only the furthest-out talks can be cut.
pub const MAX_DETAIL_PAGES: usize = 50;
/// Upper bound on pages fetched per art-form listing.
pub const MAX_LISTING_PAGES: usize = 5;
const LISTING_PATHS: &[&str] = &["/whats-on/art-design", "/whats-on/talks-events"];
const SITE: &str = "https://www.barbican.org.uk";
const VENUE_NAME: &str = "Barbican Centre";
const VENUE_ADDRESS: &str = "Silk Street, London EC2Y 8DS";
/// Approximate location of the building.
const VENUE_LAT: f64 = 51.5201;
const VENUE_LNG: f64 = -0.0955;

pub struct Barbican {
    base_url: Url,
}

impl Barbican {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// One page of an art-form listing.
#[derive(Debug, serde::Serialize)]
pub struct ListingPage {
    /// Event paths (`/whats-on/<year>/event/<slug>`), in document order,
    /// de-duplicated.
    pub event_paths: Vec<String>,
    /// Absolute URL of each event's card image, by event path.
    pub image_urls: BTreeMap<String, String>,
    /// Each event's card title (`h2.listing-title`), by event path; titles
    /// cut short with `...`/`…` are left out.
    pub titles: BTreeMap<String, String>,
    /// The "Load More" link (`?page=N`), relative to the page's own URL.
    pub next_page: Option<String>,
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// `(year, slug)` of a `/whats-on/<year>/event/<slug>` path.
fn event_path_parts(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/whats-on/")?;
    let (year, slug) = rest.split_once("/event/")?;
    let valid = year.len() == 4
        && year.chars().all(|c| c.is_ascii_digit())
        && !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    valid.then_some((year, slug))
}

pub fn parse_listing(html: &str) -> ListingPage {
    let doc = Html::parse_document(html);
    let base = Url::parse(SITE).expect("valid url");
    let link = selector("a.search-listing__link[href]");
    let image = selector(".search-listing__image img[src]");
    let card_title = selector("h2.listing-title");
    let mut event_paths: Vec<String> = Vec::new();
    let mut image_urls = BTreeMap::new();
    let mut titles = BTreeMap::new();
    for card in doc.select(&selector("article.listing--event")) {
        let Some(Ok(u)) = card
            .select(&link)
            .next()
            .and_then(|a| a.value().attr("href"))
            .map(|h| base.join(h))
        else {
            continue;
        };
        if u.host_str() != Some("www.barbican.org.uk") || u.query().is_some() {
            continue;
        }
        let path = u.path().to_string();
        if event_path_parts(&path).is_none() || event_paths.contains(&path) {
            continue;
        }
        if let Some(Ok(img)) = card
            .select(&image)
            .next()
            .and_then(|i| i.value().attr("src"))
            .map(|src| base.join(src))
        {
            image_urls.insert(path.clone(), img.to_string());
        }
        if let Some(title) = card
            .select(&card_title)
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty() && !t.ends_with("...") && !t.ends_with('…'))
        {
            titles.insert(path.clone(), title);
        }
        event_paths.push(path);
    }
    let next_page = doc
        .select(&selector(".pager a[rel=next][href]"))
        .next()
        .and_then(|a| a.value().attr("href"))
        .map(str::to_string);
    ListingPage {
        event_paths,
        image_urls,
        titles,
        next_page,
    }
}

/// The lead paragraph followed by the main body copy (the first block of
/// `.trimmed-content`; its siblings are tag buttons and sponsor credits).
fn description_html(doc: &Html) -> Option<String> {
    let parts: Vec<String> = [".lead-text", ".trimmed-content > div:first-child"]
        .iter()
        .filter_map(|s| doc.select(&selector(s)).next())
        .map(|e| e.inner_html())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Art-form slugs (`art-design`, `talks-events`, …) of the page's tag
/// buttons. Other tag buttons link to series, taxonomy or visitor pages.
fn art_forms(doc: &Html) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in doc.select(&selector(".tag-buttons a.tag-button[href]")) {
        let Some(form) = a
            .value()
            .attr("href")
            .and_then(|h| h.strip_prefix("/whats-on/"))
        else {
            continue;
        };
        let valid = !form.is_empty() && form.chars().all(|c| c.is_ascii_lowercase() || c == '-');
        if valid && !out.iter().any(|f| f == form) {
            out.push(form.to_string());
        }
    }
    out
}

/// The first ticket-price row: its value, or its title when the value is
/// empty (a bare "Free" row).
fn price_text(doc: &Html) -> Option<String> {
    let row = doc
        .select(&selector(".ticket-prices .accordion-item--unexpandable"))
        .next()?;
    let text_of = |s: &str| {
        row.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    text_of(".accordion-item__value").or_else(|| text_of(".accordion-item__title"))
}

fn is_avif(image_url: &str) -> bool {
    Url::parse(image_url).is_ok_and(|u| u.path().to_ascii_lowercase().ends_with(".avif"))
}

/// Parse one detail page into a [`RawEvent`] (None if it has no title).
/// `url` is the address the page was fetched from; `<year>/<slug>` of its
/// path is the stable source id. `listing_image` and `listing_title` are the
/// event's card image and title from [`ListingPage::image_urls`] and
/// [`ListingPage::titles`].
pub fn parse_detail(
    html: &str,
    url: &Url,
    listing_image: Option<&str>,
    listing_title: Option<&str>,
) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let first_text = |s: &str| {
        doc.select(&selector(s))
            .next()
            .map(element_text)
            .filter(|t| !t.is_empty())
    };
    let title = first_text("h1.heading-group__primary")?;
    let times: Vec<&str> = doc
        .select(&selector(".event-byline .date-range time[datetime]"))
        .filter_map(|e| e.value().attr("datetime"))
        .collect();
    let image_url = listing_image.map(str::to_string).or_else(|| {
        doc.select(&selector(r#"meta[property="og:image"]"#))
            .next()
            .and_then(|e| e.value().attr("content"))
            .filter(|u| !is_avif(u))
            .map(str::to_string)
    });
    let path = url.path();
    let source_event_id = match event_path_parts(path) {
        Some((year, slug)) => format!("{year}/{slug}"),
        None => path.trim_matches('/').to_string(),
    };
    Some(RawEvent {
        source_event_id,
        source_url: Some(url.to_string()),
        payload: json!({
            "url": url.as_str(),
            "listing_title": listing_title,
            "title": title,
            "subtitle": first_text("h2.heading-group__secondary"),
            "date_text": first_text(".event-byline .date-range"),
            "starts": times.first(),
            "ends": times.get(1..).and_then(<[_]>::last),
            "room": first_text(".event-byline__venue"),
            "art_forms": art_forms(&doc),
            "price_text": price_text(&doc),
            "description": description_html(&doc),
            "image_url": image_url,
        }),
    })
}

/// The in-scope category for a page's art forms, or `None` to skip it.
pub fn category(art_forms: &[&str], multi_day: bool) -> Option<Category> {
    let talks = art_forms.contains(&"talks-events");
    if art_forms.contains(&"cinema") {
        None
    } else if talks && art_forms.contains(&"take-part") {
        Some(Category::Community)
    } else if talks {
        Some(Category::Talk)
    } else if art_forms.contains(&"art-design") && multi_day {
        Some(Category::Exhibition)
    } else {
        None
    }
}

/// Normalise a Barbican [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|t| !t.is_empty())
    };
    let title = text("listing_title")
        .or_else(|| text("title"))
        .ok_or_else(|| SourceError::Parse("detail page without title".into()))?;
    let time = |key: &str| -> Result<Option<_>, SourceError> {
        payload
            .get(key)
            .and_then(Value::as_str)
            .map(|s| {
                parse_datetime(s).ok_or_else(|| SourceError::Parse(format!("bad {key} time {s:?}")))
            })
            .transpose()
    };
    let starts_at =
        time("starts")?.ok_or_else(|| SourceError::Parse(format!("{title:?}: no header date")))?;
    let ends_at = time("ends")?;
    if ends_at.is_some_and(|e| e < starts_at) {
        return Err(SourceError::Parse(format!(
            "{title:?}: ends before it starts"
        )));
    }
    let multi_day = ends_at.is_some_and(|e| london_date(e) > london_date(starts_at));
    let forms: Vec<&str> = payload
        .get("art_forms")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(category) = category(&forms, multi_day) else {
        return Ok(None);
    };
    let description = clean_description(payload.get("description").and_then(Value::as_str));
    let price = match payload.get("price_text").and_then(Value::as_str) {
        Some(t) => parse_price(t.split('(').next().unwrap_or(t)),
        None if description.as_deref().is_some_and(describes_free_entry) => parse_price("Free"),
        None => Price::default(),
    };

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description,
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at,
        all_day: payload
            .get("starts")
            .and_then(Value::as_str)
            .is_some_and(is_date_only),
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
        tags: forms.iter().map(|f| f.to_string()).collect(),
    }))
}

#[async_trait]
impl Source for Barbican {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let mut paths: Vec<String> = Vec::new();
        let mut images: BTreeMap<String, String> = BTreeMap::new();
        let mut titles: BTreeMap<String, String> = BTreeMap::new();
        for listing in LISTING_PATHS {
            let mut url = self
                .base_url
                .join(listing)
                .map_err(|e| SourceError::Config(e.to_string()))?;
            for _ in 0..MAX_LISTING_PAGES {
                let page = parse_listing(&ctx.get_text(&url).await?);
                for path in page.event_paths {
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                }
                for (path, image) in page.image_urls {
                    images.entry(path).or_insert(image);
                }
                for (path, title) in page.titles {
                    titles.entry(path).or_insert(title);
                }
                let Some(next) = page.next_page else {
                    break;
                };
                url = url
                    .join(&next)
                    .map_err(|e| SourceError::Parse(format!("bad next-page link {next:?}: {e}")))?;
            }
        }
        if paths.is_empty() {
            return Err(SourceError::Parse(
                "no event links found on the listing pages".into(),
            ));
        }
        let mut out = Vec::new();
        for path in paths.iter().take(MAX_DETAIL_PAGES) {
            let url = match self.base_url.join(path) {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad detail path {path}: {e}"));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(
                    &html,
                    &url,
                    images.get(path).map(String::as_str),
                    titles.get(path).map(String::as_str),
                ) {
                    Some(raw) => out.push(raw),
                    None => ctx.report_error(format!("{path}: no page title")),
                },
                Err(e) => ctx.report_error(format!("{path}: {e}")),
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only Talks & events (except anything also filed under Cinema) and \
             Art & design exhibitions running over more than one day. \
             Single-date Art & design items (concerts, gigs and performances, \
             including ones also tagged Contemporary music, gallery tours and \
             late openings) are left out on purpose.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn concrete_and_clay(description: &str) -> Value {
        json!({
            "title": "Concrete and Clay: Archiving the Barbican",
            "starts": "2025-02-10T10:00:00Z",
            "ends": "2027-01-01T10:00:00Z",
            "art_forms": ["art-design"],
            "description": description,
        })
    }

    #[test]
    fn description_free_wording_is_a_fallback() {
        let mut payload = concrete_and_clay(
            "<p>This free installation invites visitors to orient themselves.</p>",
        );
        let price = normalise_payload(&payload).unwrap().unwrap().price;
        assert!(price.is_free);
        assert_eq!(price.min, Some(Decimal::ZERO));

        payload["price_text"] = json!("£20.50 (£19 + £1.50 transaction fee)");
        let price = normalise_payload(&payload).unwrap().unwrap().price;
        assert!(!price.is_free);
        assert_eq!(price.min, Some(Decimal::new(2050, 2)));
    }

    #[test]
    fn description_without_free_wording_has_unknown_price() {
        let payload = concrete_and_clay("<p>An installation drawn from the Barbican archive.</p>");
        let price = normalise_payload(&payload).unwrap().unwrap().price;
        assert_eq!(price, Price::default());
    }

    #[test]
    fn header_times_are_utc_instants() {
        // Walters and Cohen: "Thu 22 Oct 2026, 19:00" on the page (BST).
        let event = normalise_payload(&json!({
            "title": "Walters and Cohen",
            "starts": "2026-10-22T18:00:00Z",
            "art_forms": ["art-design", "talks-events"],
        }))
        .unwrap()
        .unwrap();
        assert_eq!(event.starts_at.to_rfc3339(), "2026-10-22T18:00:00+00:00");
        assert_eq!(
            event
                .starts_at
                .with_timezone(&chrono_tz::Europe::London)
                .format("%H:%M")
                .to_string(),
            "19:00"
        );
        assert_eq!(event.ends_at, None);
    }

    fn detail_with_og_image(og_image: &str) -> String {
        format!(
            r#"<html><head><meta property="og:image" content="{og_image}" /></head>
            <body><h1 class="heading-group__primary">Talk</h1></body></html>"#
        )
    }

    fn image_url(html: &str, listing_image: Option<&str>) -> Option<String> {
        let url = Url::parse("https://www.barbican.org.uk/whats-on/2026/event/talk").unwrap();
        let raw = parse_detail(html, &url, listing_image, None).unwrap();
        raw.payload["image_url"].as_str().map(str::to_string)
    }

    #[test]
    fn listing_image_wins_and_avif_og_image_is_dropped() {
        let avif = detail_with_og_image(
            "https://www.barbican.org.uk/sites/default/files/styles/large/public/images/x.jpg.avif?itok=a",
        );
        let jpeg = "https://www.barbican.org.uk/sites/default/files/styles/event_listing_small_jpg/public/images/x.jpg?itok=b";
        assert_eq!(image_url(&avif, Some(jpeg)).as_deref(), Some(jpeg));
        assert_eq!(image_url(&avif, None), None);

        let og_jpeg = "https://www.barbican.org.uk/sites/default/files/x.JPG?itok=c";
        let html = detail_with_og_image(og_jpeg);
        assert_eq!(image_url(&html, None).as_deref(), Some(og_jpeg));
        assert_eq!(image_url(&html, Some(jpeg)).as_deref(), Some(jpeg));
    }

    #[test]
    fn listing_title_wins_over_the_page_heading() {
        let mut payload = concrete_and_clay("");
        payload["title"] = json!("Concrete and Clay");
        let title = |p: &Value| normalise_payload(p).unwrap().unwrap().title;
        assert_eq!(title(&payload), "Concrete and Clay");
        payload["listing_title"] = json!("");
        assert_eq!(title(&payload), "Concrete and Clay");
        payload["listing_title"] = json!("Concrete and Clay: Archiving the Barbican");
        assert_eq!(title(&payload), "Concrete and Clay: Archiving the Barbican");
    }

    #[test]
    fn cut_short_card_titles_are_dropped() {
        let card = |slug: &str, title: &str| {
            format!(
                r#"<article class="listing--event">
                <a class="search-listing__link" href="/whats-on/2026/event/{slug}"></a>
                <h2 class="listing-title listing-title--event"> {title} </h2>
                </article>"#
            )
        };
        let html = format!(
            "<html><body>{}{}{}</body></html>",
            card("whole", "Robert Ryman: The Real Thing"),
            card(
                "cut",
                "Robert Ryman: Exhibition Tour with Curatorial Assistant..."
            ),
            card("ellipsis", "Darbar: Exploring the Beauty of Raga…"),
        );
        let page = parse_listing(&html);
        assert_eq!(page.event_paths.len(), 3);
        assert_eq!(
            page.titles,
            BTreeMap::from([(
                "/whats-on/2026/event/whole".to_string(),
                "Robert Ryman: The Real Thing".to_string()
            )])
        );
    }

    #[test]
    fn the_qa_scope_names_the_kept_and_skipped_art_forms() {
        let scope = Barbican::new(Url::parse("https://x.test/").unwrap())
            .qa_scope()
            .unwrap();
        for words in [
            "Talks & events",
            "Art & design",
            "more than one day",
            "Cinema",
            "Single-date",
            "Contemporary music",
            "left out on purpose",
        ] {
            assert!(scope.contains(words), "{words}");
        }
    }

    #[test]
    fn category_rules() {
        assert_eq!(
            category(&["talks-events", "take-part"], false),
            Some(Category::Community)
        );
        assert_eq!(
            category(&["talks-events", "take-part", "cinema"], false),
            None
        );
        assert_eq!(category(&["contemporary-music", "take-part"], false), None);
        assert_eq!(
            category(&["classical-music", "talks-events"], false),
            Some(Category::Talk)
        );
        assert_eq!(category(&["art-design"], true), Some(Category::Exhibition));
        assert_eq!(category(&["art-design"], false), None);
        assert_eq!(category(&["art-design", "contemporary-music"], false), None);
        assert_eq!(category(&["cinema"], true), None);
    }

    #[test]
    fn same_day_range_is_not_an_exhibition() {
        let skipped = normalise_payload(&json!({
            "title": "Late opening",
            "starts": "2026-10-22T17:00:00Z",
            "ends": "2026-10-22T21:00:00Z",
            "art_forms": ["art-design"],
        }))
        .unwrap();
        assert!(skipped.is_none());
    }

    #[test]
    fn bad_or_missing_times_are_errors() {
        for payload in [
            json!({"title": "No date", "art_forms": ["talks-events"]}),
            json!({"title": "Bad", "starts": "soon", "art_forms": ["talks-events"]}),
            json!({
                "title": "Backwards",
                "starts": "2026-10-22T18:00:00Z",
                "ends": "2026-10-21T18:00:00Z",
                "art_forms": ["art-design"],
            }),
        ] {
            assert!(normalise_payload(&payload).is_err(), "{payload}");
        }
    }
}
