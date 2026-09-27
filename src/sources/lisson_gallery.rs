//! Lisson Gallery (27 Bell Street and 67 Lisson Street, NW1) — the London
//! exhibitions of an international gallery, from `/exhibitions` plus each
//! London show's JSON-LD `ExhibitionEvent`.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): the
//!   `User-Agent: *` group allows everything but `/private-viewing-room/`,
//!   `/api/` and `/fairs/*/*`. lissongallery.com redirects to lisson.com.
//! * `/exhibitions` lists current and upcoming shows in every city, then
//!   museum shows of Lisson artists elsewhere and past shows. A card is an
//!   `article` linking `/exhibitions/<slug>`; current and upcoming cards
//!   carry their city (`<p>London</p>`) and dates (`time[datetime]`). Only
//!   cards naming London and carrying dates are followed (museum and past
//!   cards have no city or no dates), at most [`MAX_DETAIL_PAGES`] a run.
//! * Each exhibition page has one JSON-LD `ExhibitionEvent`: `name`, `url`,
//!   date-only `startDate`/`endDate` (stored `all_day`, checked against the
//!   printed "19 November – 13 February"), `location.address` "London". The
//!   artist is a `<p>` above the `h1` (`hgroup > p`) when the show's title isn't the
//!   artist's name ("Christopher Le Brun: The Mind's Weather"). The
//!   description is the "About" section (`section#about`).
//! * The page doesn't say which of the two London spaces a show is in
//!   (both are in the footer), so the address names the space only when the
//!   slug, title or description does ("lisson-street-…", "at 27 Bell
//!   Street"); otherwise it gives both, without coordinates.
//! * Everything is an exhibition. The JSON-LD `image` is the full-size
//!   original, so the image is the page's `og:image` (a 1200 px JPEG).

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError, jsonld};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc};

pub const KEY: &str = "lisson-gallery";
/// Per-run cap on exhibition page fetches (about 2–6 London shows are
/// current or upcoming at a time).
pub const MAX_DETAIL_PAGES: usize = 25;
const LISTING_PATH: &str = "/exhibitions";
const PREFIX: &str = "/exhibitions/";
const VENUE_NAME: &str = "Lisson Gallery";
const BELL_STREET: (&str, f64, f64) = ("27 Bell Street, London NW1 5BY", 51.522390, -0.167230);
const LISSON_STREET: (&str, f64, f64) = ("67 Lisson Street, London NW1 5DA", 51.521900, -0.168310);
const BOTH_SPACES: &str = "27 Bell Street / 67 Lisson Street, London NW1";

pub struct LissonGallery {
    base_url: Url,
    max_detail_pages: usize,
}

impl LissonGallery {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            max_detail_pages: MAX_DETAIL_PAGES,
        }
    }

    /// Override the per-run cap on exhibition pages (tests).
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

/// The paths of the London exhibitions on the listing (cards naming London
/// and carrying dates), in page order, de-duplicated.
pub fn parse_listing(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let link = selector(r#"a[href^="/exhibitions/"]"#);
    let city = selector("p");
    let dates = selector("time[datetime]");
    let mut out: Vec<String> = Vec::new();
    for card in doc.select(&selector("article")) {
        let Some(href) = card
            .select(&link)
            .next()
            .and_then(|a| a.value().attr("href"))
        else {
            continue;
        };
        let in_london = card.select(&city).any(|p| element_text(p) == "London");
        let dated = card.select(&dates).next().is_some();
        let slug = href.strip_prefix(PREFIX).unwrap_or_default();
        let valid = !slug.is_empty() && !slug.contains(['/', '?', '#']);
        if in_london && dated && valid && !out.iter().any(|p| p == href) {
            out.push(href.to_string());
        }
    }
    out
}

/// Parse an exhibition page (fetched from `page_url`) into a [`RawEvent`].
/// `None` for a page without an `ExhibitionEvent`.
pub fn parse_detail(html: &str, page_url: &Url) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let node = jsonld::extract_events(&doc).into_iter().next()?;
    let artist = doc
        .select(&selector("main hgroup > p"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty());
    let about = doc
        .select(&selector("main section#about"))
        .next()
        .map(|s| {
            s.select(&selector("p"))
                .map(|p| p.html())
                .collect::<Vec<_>>()
                .join("")
        })
        .filter(|s| !s.is_empty());
    let image = doc
        .select(&selector(r#"meta[property="og:image"]"#))
        .next()
        .and_then(|m| m.value().attr("content"));
    Some(RawEvent {
        source_event_id: page_url.path().trim_start_matches('/').to_string(),
        source_url: Some(page_url.to_string()),
        payload: json!({
            "url": page_url.as_str(),
            "event": node,
            "artist": artist,
            "about": about,
            "image": image,
        }),
    })
}

fn date(node: &Value, key: &str) -> Option<NaiveDate> {
    let s = node.get(key)?.as_str()?.trim();
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

/// Normalise a Lisson Gallery [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let node = payload
        .get("event")
        .ok_or_else(|| SourceError::Parse("payload without event".into()))?;
    let name = jsonld::str_or_name(node, "name")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("exhibition without a name".into()))?;
    let address = node
        .get("location")
        .and_then(|l| {
            jsonld::str_or_name(l, "address").or_else(|| {
                l.pointer("/address/addressLocality")
                    .and_then(Value::as_str)
            })
        })
        .map(clean_text)
        .unwrap_or_default();
    if !address.to_lowercase().contains("london") {
        return Ok(None);
    }
    let first = date(node, "startDate")
        .ok_or_else(|| SourceError::Parse(format!("{name:?}: no startDate")))?;
    let last = date(node, "endDate").unwrap_or(first);
    if last < first {
        return Err(SourceError::Parse(format!(
            "{name:?}: ends before it starts"
        )));
    }
    let artist = payload
        .get("artist")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|a| !a.is_empty() && !name.contains(a.as_str()));
    let title = match artist {
        Some(artist) => format!("{artist}: {name}"),
        None => name,
    };
    let description = clean_description(payload.get("about").and_then(Value::as_str));
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string);

    let hints = [
        url.as_deref().unwrap_or_default().replace('-', " "),
        title.clone(),
        description.clone().unwrap_or_default(),
    ]
    .join(" ")
    .to_lowercase();
    let space = match (
        hints.contains("bell street"),
        hints.contains("lisson street"),
    ) {
        (true, false) => Some(BELL_STREET),
        (false, true) => Some(LISSON_STREET),
        _ => None,
    };
    let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
    let starts_at = midnight(first);

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        title,
        description,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(space.map_or(BOTH_SPACES, |s| s.0).to_string()),
        lat: space.map(|s| s.1),
        lng: space.map(|s| s.2),
        starts_at,
        ends_at: (last > first).then(|| midnight(last)),
        all_day: true,
        price: Price::default(),
        url,
        image_url: payload
            .get("image")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| jsonld::image_url(node)),
        category: Category::Exhibition,
        tags: Vec::new(),
    }))
}

#[async_trait]
impl Source for LissonGallery {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listing_url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let listing = ctx.get_text(&listing_url).await?;
        let doc_has_cards = listing.contains("<article");
        let paths = parse_listing(&listing);
        if !doc_has_cards {
            return Err(SourceError::Parse(
                "no exhibition cards found on the listing page".into(),
            ));
        }
        // Between shows no London card may be current or upcoming: that is
        // an empty programme, not an error.
        let mut out = Vec::new();
        for path in paths.iter().take(self.max_detail_pages) {
            let url = match self.base_url.join(path) {
                Ok(u) => u,
                Err(e) => {
                    ctx.report_error(format!("bad exhibition path {path}: {e}"));
                    continue;
                }
            };
            match ctx.get_text(&url).await {
                Ok(html) => match parse_detail(&html, &url) {
                    Some(raw) => out.push(raw),
                    None => ctx.report_error(format!("{path}: no ExhibitionEvent JSON-LD")),
                },
                Err(e) => ctx.report_error(format!("{path}: {e}")),
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

    fn payload(patch: Value) -> Value {
        let mut p = json!({
            "url": "https://lisson.com/exhibitions/some-show",
            "event": {"@type": "ExhibitionEvent", "name": "Some Show",
                      "startDate": "2026-11-19", "endDate": "2027-02-13",
                      "location": {"@type": "Place", "name": "Lisson Gallery", "address": "London"}},
            "artist": null, "about": "<p>A show.</p>", "image": null,
        });
        for (k, v) in patch.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    }

    #[test]
    fn dates_are_all_day_london_midnights() {
        let ev = normalise_payload(&payload(json!({}))).unwrap().unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-11-19T00:00:00+00:00");
        assert_eq!(
            ev.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2027-02-13T00:00:00+00:00")
        );
        let bst = payload(json!({"event": {"name": "X", "startDate": "2026-09-10",
            "location": {"address": "London"}}}));
        let ev = normalise_payload(&bst).unwrap().unwrap();
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-09-09T23:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    #[test]
    fn other_cities_are_skipped_and_bad_dates_are_errors() {
        let ny = payload(json!({"event": {"name": "X", "startDate": "2026-11-19",
            "location": {"address": "New York"}}}));
        assert_eq!(normalise_payload(&ny).unwrap(), None);
        let backwards = payload(json!({"event": {"name": "X", "startDate": "2026-11-19",
            "endDate": "2026-11-01", "location": {"address": "London"}}}));
        assert!(normalise_payload(&backwards).is_err());
    }

    #[test]
    fn artist_prefix_and_space() {
        let ev = normalise_payload(&payload(json!({"artist": "Christopher Le Brun"})))
            .unwrap()
            .unwrap();
        assert_eq!(ev.title, "Christopher Le Brun: Some Show");
        assert_eq!(ev.address.as_deref(), Some(BOTH_SPACES));
        assert_eq!(ev.lat, None);
        let named = normalise_payload(&payload(json!({"artist": "Some Show"})))
            .unwrap()
            .unwrap();
        assert_eq!(named.title, "Some Show");
        let lisson_street =
            payload(json!({"url": "https://lisson.com/exhibitions/lisson-street-x"}));
        let ev = normalise_payload(&lisson_street).unwrap().unwrap();
        assert_eq!(ev.address.as_deref(), Some(LISSON_STREET.0));
        let bell = payload(json!({"about": "<p>At 27 Bell Street, a show.</p>"}));
        let ev = normalise_payload(&bell).unwrap().unwrap();
        assert_eq!(ev.address.as_deref(), Some(BELL_STREET.0));
        assert_eq!(ev.lat, Some(BELL_STREET.1));
    }
}
