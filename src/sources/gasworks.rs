//! Gasworks (Vauxhall Street, SE11): exhibitions and events from the
//! site's two server-rendered listings, `/exhibitions/` and `/events/`.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): the `User-agent: *`
//!   group allows both listings but asks for `Crawl-delay: 20` and
//!   `Request-rate: 1/60`. FetchContext honours the Crawl-delay; the
//!   one-request-a-minute rate is a built-in floor in
//!   `config::BUILTIN_MIN_INTERVALS`. A run makes robots.txt, the two
//!   listings and up to [`MAX_DETAILS`] event detail pages, 60 s apart, so
//!   it asks the runner for a longer `fetch_timeout`.
//! * No JSON-LD, so CSS. Each listing has optional `section#current` and
//!   `section#forthcoming` blocks, then `section#archive` (past shows).
//!   Only cards outside the archive are read. A page with no current cards is
//!   normal between shows; a page with no cards at all (archive included)
//!   means the layout changed: an error for exhibitions, a reported error for
//!   events.
//! * A card is `article.list-item` with a `header`: `h3` type label
//!   ("Exhibition", "Event"), `h2.date` date line and an `h1 a` title/link.
//!   Event cards sometimes link under `/exhibitions/`; the listing page, not
//!   the link, decides the section.
//! * Date lines have two-digit years on the last day only: "1 Oct – 13 Dec
//!   26", "9 – 10 Oct 26", "1 Oct 26". The year is expanded to 20xx and the
//!   line is parsed with The Showroom's `parse_when` (a start without a year
//!   takes the end's year, or the year before). There are never times, so
//!   everything is all-day (London midnights): `ends_at` is the last day of
//!   a range, none for a single day or a "From <day>" run. Runs longer than
//!   `the_showroom::MAX_RANGE_DAYS` are skipped.
//! * Categories: exhibitions → exhibition. Events by title (the type label is
//!   always just "Event") via The Showroom's `event_category`, with tours
//!   and conversations as talks: screenings
//!   and children's/family sessions are skipped, tours/talks → talk,
//!   workshops → workshop, the rest (club nights, performances) → community.
//! * Price: the site footer says "FREE ADMISSION" (gallery admission); it is
//!   applied to exhibitions only. Event prices are unknown.
//! * Descriptions: the listing's `precis` is cut short by the site ("…"),
//!   and detail pages are read for the location only, so no description is
//!   stored.
//! * Venue: exhibitions are at the gallery. An event's detail page has a
//!   second header `h2` ("12:30–1pm", or "Various times at Wellcome
//!   Collection, 183 Euston Road, London NW1 2BE"). When it names another
//!   place with a UK postcode, that place is the venue and address (#200).
//!   A failed or over-cap detail page, or one without that line, is
//!   reported and keeps Gasworks. Exhibition detail pages are not fetched.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::the_showroom::{MAX_RANGE_DAYS, When, event_category, parse_when};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_text, dedupe_key, london_date, london_to_utc, parse_price, words};
use crate::venues::postcode;

pub const KEY: &str = "gasworks";
pub const EXHIBITIONS_PATH: &str = "/exhibitions/";
pub const EVENTS_PATH: &str = "/events/";
const VENUE_NAME: &str = "Gasworks";
const VENUE_ADDRESS: &str = "155 Vauxhall Street, London SE11 5RH";
/// At most this many event detail pages a run.
pub const MAX_DETAILS: usize = 6;
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Which listing a card came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Exhibitions,
    Events,
}

impl Section {
    fn name(self) -> &'static str {
        match self {
            Section::Exhibitions => "exhibitions",
            Section::Events => "events",
        }
    }
}

/// The cards of one listing page.
#[derive(Debug, Default)]
pub struct Listing {
    /// Current and forthcoming cards (not the archive), as raw events.
    pub items: Vec<RawEvent>,
    /// Every card on the page, archive included (0 means the layout changed).
    pub total: usize,
}

pub struct Gasworks {
    base_url: Url,
}

impl Gasworks {
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

fn in_archive(e: ElementRef<'_>) -> bool {
    e.ancestors()
        .filter_map(ElementRef::wrap)
        .any(|a| a.value().name() == "section" && a.value().attr("id") == Some("archive"))
}

/// Parse one listing page. `page_url` is where it was fetched from (links
/// are resolved against it and must stay on its host); `listed_on` is the
/// London date of the fetch, kept in each payload.
pub fn parse_listing(
    html: &str,
    page_url: &Url,
    section: Section,
    listed_on: NaiveDate,
) -> Listing {
    let doc = Html::parse_document(html);
    let admission = doc
        .select(&selector("p"))
        .map(element_text)
        .find(|t| t.to_lowercase().contains("admission"));
    let type_sel = selector("header h3");
    let date_sel = selector("header h2.date");
    let link_sel = selector("header h1 a[href]");
    let img_sel = selector("figure img[src]");
    let mut listing = Listing::default();
    for card in doc.select(&selector("main article.list-item")) {
        let Some(link) = card.select(&link_sel).next() else {
            continue;
        };
        let Some(Ok(url)) = link.value().attr("href").map(|h| page_url.join(h)) else {
            continue;
        };
        if url.host_str() != page_url.host_str() || url.query().is_some() {
            continue;
        }
        let id = url.path().trim_matches('/').to_string();
        if id.is_empty() {
            continue;
        }
        listing.total += 1;
        if in_archive(card) || listing.items.iter().any(|r| r.source_event_id == id) {
            continue;
        }
        let title = element_text(link);
        if title.is_empty() {
            continue;
        }
        let text_of = |sel: &Selector| {
            card.select(sel)
                .next()
                .map(element_text)
                .filter(|t| !t.is_empty())
        };
        let image_url = card
            .select(&img_sel)
            .next()
            .and_then(|e| e.value().attr("src"))
            .filter(|s| !s.starts_with("data:"))
            .and_then(|s| page_url.join(s).ok())
            .map(|u| u.to_string());
        listing.items.push(RawEvent {
            source_event_id: id,
            source_url: Some(url.to_string()),
            payload: json!({
                "section": section.name(),
                "url": url.as_str(),
                "type": text_of(&type_sel),
                "date_text": text_of(&date_sel),
                "title": title,
                "image_url": image_url,
                "admission_text": admission,
                "listed_on": listed_on.to_string(),
            }),
        });
    }
    listing
}

/// The location line of an event's detail page: the header's `h2` that is
/// not the date line.
pub fn parse_detail(html: &str) -> Option<String> {
    Html::parse_document(html)
        .select(&selector("header.page-header h2:not(.date)"))
        .next()
        .map(element_text)
        .filter(|t| !t.is_empty())
}

/// The venue and address named by a location line ("Various times at
/// Wellcome Collection, 183 Euston Road, London NW1 2BE"), when it is a
/// place other than Gasworks with a UK postcode.
pub fn off_site_venue(location: &str) -> Option<(String, String)> {
    let location = clean_text(location);
    let at = location.to_lowercase().find(" at ")?;
    let (venue, address) = location[at + " at ".len()..].split_once(',')?;
    let (venue, address) = (venue.trim(), clean_text(address));
    let on_site = Some(postcode(&address)?) == postcode(VENUE_ADDRESS)
        || venue.to_lowercase().contains("gasworks");
    (!venue.is_empty() && !on_site).then(|| (venue.to_string(), address))
}

/// Expand a two-digit year after a month ("13 Dec 26" → "13 Dec 2026").
pub fn expand_years(text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let after_month = i > 0
                && tokens[i - 1]
                    .to_lowercase()
                    .get(..3)
                    .is_some_and(|m| MONTHS.contains(&m));
            if after_month && t.len() == 2 && t.chars().all(|c| c.is_ascii_digit()) {
                format!("20{t}")
            } else {
                t.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Category for an event from its title: The Showroom's rules (screenings
/// and children's/family sessions skipped), except that tours, talks and
/// conversations are talks even when `map_category` sees another word first
/// ("Exhibition Tour"), and an event is never an exhibition.
pub fn category_for_event(title: &str) -> Option<Category> {
    let category = event_category("", title)?;
    let w = words(title);
    let talk = [
        "tour",
        "tours",
        "talk",
        "talks",
        "conversation",
        "symposium",
    ];
    if w.iter().any(|x| talk.contains(&x.as_str())) {
        Some(Category::Talk)
    } else if category == Category::Exhibition {
        Some(Category::Community)
    } else {
        Some(category)
    }
}

fn london_midnight(d: NaiveDate) -> DateTime<Utc> {
    london_to_utc(d.and_time(NaiveTime::MIN))
}

/// Normalise a payload built by [`parse_listing`].
pub fn normalise_payload(p: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |k: &str| {
        p.get(k)
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|t| !t.is_empty())
    };
    let title = text("title").ok_or_else(|| SourceError::Parse("card without a title".into()))?;
    let exhibition = p["section"] == "exhibitions";
    let category = if exhibition {
        Category::Exhibition
    } else {
        match category_for_event(&title) {
            Some(c) => c,
            None => return Ok(None),
        }
    };
    let listed_on = text("listed_on")
        .and_then(|s| s.parse::<NaiveDate>().ok())
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no listed_on")))?;
    let date_text = text("date_text")
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no date line")))?;
    let (first, last) = match parse_when(&expand_years(&date_text), listed_on)? {
        When::Days(first, last) => (first, last.filter(|l| *l > first)),
        When::Timed(..) => {
            return Err(SourceError::Parse(format!(
                "{title:?}: unexpected time in {date_text:?}"
            )));
        }
    };
    if last.is_some_and(|l| (l - first).num_days() > MAX_RANGE_DAYS) {
        return Ok(None);
    }
    let starts_at = london_midnight(first);
    let (venue, address) = text("location")
        .and_then(|l| off_site_venue(&l))
        .unwrap_or_else(|| (VENUE_NAME.to_string(), VENUE_ADDRESS.to_string()));
    let price = match text("admission_text") {
        Some(a) if exhibition => parse_price(&a),
        _ => Price::default(),
    };
    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(&venue)),
        title,
        description: None,
        venue_name: Some(venue),
        address: Some(address),
        lat: None,
        lng: None,
        starts_at,
        ends_at: last.map(london_midnight),
        all_day: true,
        price,
        url: text("url"),
        image_url: text("image_url"),
        category,
        tags: Vec::new(),
    }))
}

#[async_trait]
impl Source for Gasworks {
    fn key(&self) -> &str {
        KEY
    }

    fn fetch_timeout(&self) -> Option<Duration> {
        // The 60 s floor per request (robots.txt, two listings, detail
        // pages), plus a margin.
        Some(Duration::from_secs(60 * (3 + MAX_DETAILS as u64) + 60))
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listed_on = london_date(Utc::now());
        let url = self.url(EXHIBITIONS_PATH)?;
        let html = ctx.get_text(&url).await?;
        let exhibitions = parse_listing(&html, &url, Section::Exhibitions, listed_on);
        if exhibitions.total == 0 {
            return Err(SourceError::Parse(format!(
                "no exhibition cards on {EXHIBITIONS_PATH}"
            )));
        }
        let mut out = exhibitions.items;
        let url = self.url(EVENTS_PATH)?;
        let events = match ctx.get_text(&url).await {
            Ok(html) => parse_listing(&html, &url, Section::Events, listed_on),
            Err(e) => {
                ctx.report_error(format!("{EVENTS_PATH}: {e}"));
                return Ok(out);
            }
        };
        if events.total == 0 {
            ctx.report_error(format!("no event cards on {EVENTS_PATH}"));
        }
        let mut details = 0;
        for mut item in events.items {
            if out
                .iter()
                .any(|r| r.source_event_id == item.source_event_id)
            {
                continue;
            }
            let wanted = item.payload["title"]
                .as_str()
                .is_some_and(|t| category_for_event(t).is_some());
            let location = match item.source_url.as_deref().map(Url::parse) {
                Some(Ok(url)) if wanted => {
                    let id = &item.source_event_id;
                    details += 1;
                    if details > MAX_DETAILS {
                        ctx.report_error(format!("{id}: over the {MAX_DETAILS} detail-page cap"));
                        None
                    } else {
                        match ctx.get_text(&url).await {
                            Ok(html) => {
                                let location = parse_detail(&html);
                                if location.is_none() {
                                    ctx.report_error(format!(
                                        "{id}: no location line on the detail page"
                                    ));
                                }
                                location
                            }
                            Err(e) => {
                                ctx.report_error(format!("{id}: {e}"));
                                None
                            }
                        }
                    }
                }
                _ => None,
            };
            item.payload["location"] = json!(location);
            out.push(item);
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

    fn payload(section: &str, title: &str, date: &str) -> Value {
        json!({
            "section": section,
            "url": "https://www.gasworks.org.uk/events/x/",
            "type": if section == "exhibitions" { "Exhibition" } else { "Event" },
            "date_text": date,
            "title": title,
            "image_url": null,
            "admission_text": "FREE ADMISSION",
            "listed_on": "2026-09-27",
        })
    }

    fn norm(section: &str, title: &str, date: &str) -> Option<NewEvent> {
        normalise_payload(&payload(section, title, date)).unwrap()
    }

    fn london(y: i32, m: u32, day: u32) -> DateTime<Utc> {
        london_midnight(d(y, m, day))
    }

    #[test]
    fn expands_two_digit_years_after_a_month_only() {
        assert_eq!(expand_years("1 Oct – 13 Dec 26"), "1 Oct – 13 Dec 2026");
        assert_eq!(expand_years("9 – 10 Oct 26"), "9 – 10 Oct 2026");
        assert_eq!(expand_years("1 Oct 26"), "1 Oct 2026");
        assert_eq!(expand_years("From 7 Nov 26"), "From 7 Nov 2026");
        // Days and four-digit years are left alone.
        assert_eq!(expand_years("26 Oct 2026"), "26 Oct 2026");
        assert_eq!(expand_years("26 – 28 Oct"), "26 – 28 Oct");
    }

    #[test]
    fn exhibition_range_is_all_day_to_the_last_day_and_free() {
        let e = norm(
            "exhibitions",
            "Paloma Contreras Lomas: Disco Inferno",
            "1 Oct – 13 Dec 26",
        )
        .unwrap();
        assert_eq!(e.category, Category::Exhibition);
        assert!(e.all_day);
        // BST start, GMT end: London midnights.
        assert_eq!(e.starts_at.to_rfc3339(), "2026-09-30T23:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-12-13T00:00:00+00:00");
        assert_eq!(e.price, parse_price("FREE ADMISSION"));
        assert!(e.description.is_none());
    }

    #[test]
    fn cross_year_range_takes_the_start_year_from_the_end() {
        let e = norm("exhibitions", "Late show", "10 Dec – 21 Mar 27").unwrap();
        assert_eq!(e.starts_at, london(2026, 12, 10));
        assert_eq!(e.ends_at, Some(london(2027, 3, 21)));
        let e = norm("exhibitions", "Next year", "14 Jan – 21 Mar 27").unwrap();
        assert_eq!(e.starts_at, london(2027, 1, 14));
    }

    #[test]
    fn event_days_are_all_day() {
        // Single day: no end.
        let e = norm(
            "events",
            "COTCH x Gasworks presents Disco Inferno",
            "1 Oct 26",
        )
        .unwrap();
        assert!(e.all_day);
        assert_eq!(e.starts_at, london(2026, 10, 1));
        assert_eq!(e.ends_at, None);
        assert_eq!(e.category, Category::Community);
        assert_eq!(e.price, Price::default());
        // Two days sharing a month.
        let e = norm("events", "Elders, 2046", "9 – 10 Oct 26").unwrap();
        assert_eq!(e.starts_at, london(2026, 10, 9));
        assert_eq!(e.ends_at, Some(london(2026, 10, 10)));
    }

    #[test]
    fn off_site_venue_needs_a_named_place_with_a_postcode() {
        assert_eq!(
            off_site_venue(
                "Various times at Wellcome Collection, 183 Euston Road,  London NW1 2BE"
            ),
            Some((
                "Wellcome Collection".to_string(),
                "183 Euston Road, London NW1 2BE".to_string()
            ))
        );
        assert_eq!(off_site_venue("12:30–1pm"), None);
        assert_eq!(
            off_site_venue("6–9pm at Gasworks, 155 Vauxhall Street, London SE11 5RH"),
            None
        );
        assert_eq!(
            off_site_venue("6–9pm at The yard, 155 Vauxhall Street, London SE11 5RH"),
            None
        );
        assert_eq!(off_site_venue("Meet at the front desk"), None);
        assert_eq!(off_site_venue("7pm at Somewhere, 1 High Street"), None);
    }

    #[test]
    fn an_off_site_location_is_the_venue() {
        let mut p = payload("events", "Elders, 2046", "9 – 10 Oct 26");
        p["location"] =
            json!("Various times at Wellcome Collection, 183 Euston Road,  London NW1 2BE");
        let e = normalise_payload(&p).unwrap().unwrap();
        assert_eq!(e.venue_name.as_deref(), Some("Wellcome Collection"));
        assert_eq!(
            e.address.as_deref(),
            Some("183 Euston Road, London NW1 2BE")
        );
        assert_eq!(
            e.dedupe_key,
            dedupe_key("Elders, 2046", e.starts_at, Some("Wellcome Collection"))
        );
        p["location"] = json!("12:30–1pm");
        let e = normalise_payload(&p).unwrap().unwrap();
        assert_eq!(e.venue_name.as_deref(), Some(VENUE_NAME));
        assert_eq!(e.address.as_deref(), Some(VENUE_ADDRESS));
    }

    #[test]
    fn open_ended_from_date_is_all_day_without_end() {
        let e = norm("exhibitions", "Ongoing show", "From 7 Nov 26").unwrap();
        assert_eq!(e.starts_at, london(2026, 11, 7));
        assert_eq!(e.ends_at, None);
        assert!(e.all_day);
    }

    #[test]
    fn categories_from_event_titles() {
        let cat = |t: &str| norm("events", t, "7 Nov 26").map(|e| e.category);
        assert_eq!(cat("Curator's Tour: Disco Inferno"), Some(Category::Talk));
        assert_eq!(
            cat("Neighbourhood Breakfast & Exhibition Tour: Disco Inferno"),
            Some(Category::Talk)
        );
        assert_eq!(cat("Exhibition launch party"), Some(Category::Community));
        assert_eq!(cat("Family Day"), None);
        assert_eq!(cat("Film screening: Diamantino"), None);
    }

    #[test]
    fn skips_and_errors() {
        // Over a year long: skipped.
        assert!(norm("exhibitions", "Forever", "1 Oct 26 – 13 Dec 27").is_none());
        // Garbage date: an error.
        assert!(normalise_payload(&payload("events", "X", "sometime soon")).is_err());
        // No title: an error.
        assert!(normalise_payload(&payload("events", "", "1 Oct 26")).is_err());
    }
}
