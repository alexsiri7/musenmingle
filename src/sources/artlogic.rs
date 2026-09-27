//! Artlogic galleries — one platform source for the many London commercial
//! galleries whose sites run the Artlogic CMS (issue #52). Each gallery is an
//! `events.sources` row with `platform = 'artlogic'` and its settings in
//! `config` ([`ArtlogicConfig`]), so adding a gallery is a migration row, not
//! code.
//!
//! * robots.txt: every Artlogic site serves the same template (checked
//!   2026-09-27 on all seeded hosts; saved as `artlogic/robots.txt`).
//!   `User-agent: *` disallows only `/admin/ /embed/ /get/ /api/ /media/` and
//!   SQL-injection patterns, so `/exhibitions/…` is allowed. Its
//!   `Crawl-delay: 10` is for SemrushBot only.
//! * Images come from the shared host `static-assets.artlogic.net`, whose
//!   robots.txt has `User-agent: *` / `Crawl-delay: 10`. The thumbnailer
//!   fetches them through the one ingest [`FetchContext`], whose per-host
//!   rate limiter honours that delay across all galleries together.
//! * No JSON-LD or microdata anywhere, so this is an HTML parser. Only the
//!   listing pages (`listing_paths`, default `/exhibitions/`) are fetched: the
//!   cards carry title, dates, location label, image and a short teaser.
//!   Detail pages are not fetched (their `og:` tags are unreliable: GRIMM's
//!   `og:image` is a book cover, Flowers' `og:description` is site
//!   boilerplate). A listing path that redirects to a single exhibition (a
//!   gallery with one show) is read from that page's `.exhibition-header`.
//! * Sections: the newer `records_list` template wraps each list in
//!   `#exhibitions-grid-<name>` (`current`, `forthcoming`,
//!   `forthcoming_featured`, `current-forthcoming`, `past`, `online`, …), the
//!   older "classic" one in `section[data-label]` ("Current", "Upcoming",
//!   "Past", "External", …). Only current and forthcoming ones are read (the
//!   id is trusted over the heading: Portland's `online` grid is headed
//!   "Archive"); links outside any section are ignored.
//! * Ids: the number after `/exhibitions/` in the listing link (stable across
//!   slug changes and redirects: Victoria Miro's `/exhibitions/685/` lands on
//!   a custom slug), as `<host>:<id>`.
//! * Dates: date-only ranges in many shapes (`18 Sep - 7 Nov 2026`,
//!   `17 September – 14 November 2026`, `September 3 - October 10, 2026`,
//!   `11 June–26 September 2026`, `3.9 - 3.10.2026`, `9 - 26 Sep 2026`,
//!   `3 September—1 October 2026`, a single `27 Mar 2026`), stored all-day in
//!   London. A card without a date (a standing display) is skipped; text that
//!   doesn't parse is an error so a template change reaches the health
//!   checker. Open-ended "Until …" is skipped.
//! * London: multi-city galleries list `locations` in their config; a card
//!   must carry a matching location label (Victoria Miro's "London Gallery I",
//!   Flowers' "London, Cork Street"), else it is skipped, and the match picks
//!   the venue's name and address. Single-venue galleries place every show at
//!   `venue`. Art-fair booths, biennale pavilions and museum loans are not
//!   shows at the gallery and are skipped ([`is_offsite`] plus the row's
//!   `skip_match`).
//! * Everything is an exhibition; price is unknown (commercial galleries are
//!   generally free, but the cards don't say). The content policy is decided
//!   per gallery in its seed row.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::{SkipReason, Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc};

/// `events.sources.platform` of Artlogic galleries.
pub const PLATFORM: &str = "artlogic";
const DEFAULT_LISTING_PATH: &str = "/exhibitions/";
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
/// Section names (grid ids / data-labels) that hold shows to list.
const KEEP_SECTIONS: [&str; 4] = ["current", "forthcoming", "upcoming", "future"];

/// A gallery's `events.sources.config`. Unknown fields are rejected, so a
/// typo in a seed row is a recorded skip rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtlogicConfig {
    /// Listing pages to read, in order.
    #[serde(default = "default_listing_paths")]
    pub listing_paths: Vec<String>,
    /// The gallery's (main) London space.
    pub venue: Venue,
    /// For galleries with spaces outside London (or several in London): a
    /// card must carry a location label containing one of these `match`es
    /// (case-insensitive) and takes its venue from the first that does.
    #[serde(default)]
    pub locations: Vec<Location>,
    /// With `locations`: place cards that carry no location label at all at
    /// `venue` instead of skipping them (galleries whose spaces are all in
    /// London but whose cards don't always say which).
    #[serde(default)]
    pub unlabelled_at_venue: bool,
    /// Extra case-insensitive phrases that mark a card (title, subtitle or
    /// location) as not a show at the gallery.
    #[serde(default)]
    pub skip_match: Vec<String>,
}

fn default_listing_paths() -> Vec<String> {
    vec![DEFAULT_LISTING_PATH.to_string()]
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Venue {
    pub name: String,
    #[serde(default)]
    pub address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    #[serde(rename = "match")]
    pub matches: String,
    /// Defaults to the gallery's venue name.
    #[serde(default)]
    pub name: Option<String>,
    /// Defaults to the gallery's venue address.
    #[serde(default)]
    pub address: Option<String>,
}

impl ArtlogicConfig {
    /// Parse a row's `config` (required: it names the venue).
    pub fn from_json(config: Option<&Value>) -> Result<Self, SkipReason> {
        let config = config.ok_or_else(|| SkipReason::InvalidConfig("config is not set".into()))?;
        let config: ArtlogicConfig = serde_json::from_value(config.clone())
            .map_err(|e| SkipReason::InvalidConfig(e.to_string()))?;
        if config.listing_paths.is_empty() {
            return Err(SkipReason::InvalidConfig("listing_paths is empty".into()));
        }
        if config.locations.iter().any(|l| l.matches.trim().is_empty()) {
            return Err(SkipReason::InvalidConfig("empty location match".into()));
        }
        Ok(config)
    }
}

pub struct Artlogic {
    key: String,
    base_url: Url,
    config: ArtlogicConfig,
}

impl Artlogic {
    pub fn from_row(key: &str, base_url: Url, config: Option<&Value>) -> Result<Self, SkipReason> {
        if base_url.host_str().is_none() {
            return Err(SkipReason::InvalidBaseUrl(base_url.to_string()));
        }
        Ok(Self {
            key: key.to_string(),
            base_url,
            config: ArtlogicConfig::from_json(config)?,
        })
    }

    fn host(&self) -> &str {
        self.base_url.host_str().unwrap_or_default()
    }

    async fn fetch_listing(
        &self,
        ctx: &FetchContext,
        path: &str,
    ) -> Result<Vec<Card>, SourceError> {
        let url = self
            .base_url
            .join(path)
            .map_err(|e| SourceError::Config(format!("{path:?}: {e}")))?;
        let resp = ctx.get(&url).await?;
        let landed = resp.url().clone();
        let html = resp
            .text()
            .await
            .map_err(|e| SourceError::Parse(format!("reading {path}: {}", e.without_url())))?;
        if landed.host_str() != Some(self.host()) {
            return Err(SourceError::Parse(format!(
                "{path} redirected off-site to {}",
                landed.host_str().unwrap_or("?")
            )));
        }
        if exhibition_id(landed.path()).is_some() {
            return Ok(parse_single_show(&html, &landed).into_iter().collect());
        }
        Ok(parse_listing(&html, &landed))
    }
}

#[async_trait]
impl Source for Artlogic {
    fn key(&self) -> &str {
        &self.key
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let mut cards: Vec<Card> = Vec::new();
        let mut first_error = None;
        let mut any_ok = false;
        for path in &self.config.listing_paths {
            match self.fetch_listing(ctx, path).await {
                Ok(found) => {
                    any_ok = true;
                    for card in found {
                        if !cards.iter().any(|c| c.id == card.id) {
                            cards.push(card);
                        }
                    }
                }
                Err(e) => {
                    ctx.report_error(format!("{path}: {e}"));
                    first_error.get_or_insert(e);
                }
            }
        }
        if let (false, Some(e)) = (any_ok, first_error) {
            // Every listing failed: the whole run failed. Drop the soft
            // copies so the one error isn't counted twice.
            let _ = ctx.take_errors();
            return Err(e);
        }
        Ok(cards.into_iter().map(|c| c.into_raw(self.host())).collect())
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload, &self.config)
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// The exhibition id in a path: `/exhibitions/<id>[-slug]/…`.
pub fn exhibition_id(path: &str) -> Option<u64> {
    let rest = path.strip_prefix("/exhibitions/")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = &rest[digits.len()..];
    if digits.is_empty() || !(after.is_empty() || after.starts_with(['-', '/'])) {
        return None;
    }
    digits.parse().ok()
}

/// One exhibition as a listing card shows it (or a single-show page's
/// header).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Card {
    pub id: u64,
    pub url: String,
    /// Grid id suffix or `data-label` of the section it was listed in.
    pub section: String,
    pub heading: Option<String>,
    pub subtitle: Option<String>,
    pub date_text: Option<String>,
    /// Location labels (`.location`, a classic card's non-date `.bottom`s).
    pub places: Vec<String>,
    pub teaser: Option<String>,
    pub image_url: Option<String>,
}

impl Card {
    fn into_raw(self, host: &str) -> RawEvent {
        RawEvent {
            source_event_id: format!("{host}:{}", self.id),
            source_url: Some(self.url.clone()),
            payload: json!({ "card": self }),
        }
    }
}

/// The section a listing link sits in, from its nearest marked ancestor.
fn section_of(a: ElementRef<'_>) -> Option<String> {
    a.ancestors()
        .filter_map(ElementRef::wrap)
        .find_map(section_of_element)
}

/// The section an element marks, if any: `#exhibitions-grid-<name>` or
/// `[data-label]`.
fn section_of_element(e: ElementRef<'_>) -> Option<String> {
    let el = e.value();
    if let Some(id) = el
        .id()
        .and_then(|id| id.strip_prefix("exhibitions-grid-"))
        .filter(|id| *id != "container")
    {
        return Some(id.to_string());
    }
    el.attr("data-label")
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
}

fn keep_section(section: &str) -> bool {
    let s = section.to_lowercase();
    KEEP_SECTIONS.iter().any(|k| s.contains(k))
}

/// The card around a listing link: the nearest `li` or `.item`.
fn card_root(a: ElementRef<'_>) -> ElementRef<'_> {
    a.ancestors()
        .filter_map(ElementRef::wrap)
        .take_while(|e| section_of_element(*e).is_none())
        .find(|e| e.value().name() == "li" || e.value().classes().any(|c| c == "item"))
        .unwrap_or(a)
}

fn first_text(root: ElementRef<'_>, selectors: &[&str]) -> Option<String> {
    selectors.iter().find_map(|s| {
        root.select(&selector(s))
            .map(element_text)
            .find(|t| !t.is_empty())
    })
}

/// An image URL on the card: `data-src`, the first `data-responsive-src`
/// entry, or `src`, skipping placeholders.
fn card_image(root: ElementRef<'_>, page_url: &Url) -> Option<String> {
    fn usable(u: &str) -> Option<&str> {
        let u = u.trim();
        (!u.is_empty() && !u.starts_with("data:") && u.contains("/usr/images/")).then_some(u)
    }
    root.select(&selector("img")).find_map(|img| {
        let el = img.value();
        let responsive = el.attr("data-responsive-src").and_then(|s| {
            s.split(['\'', '"'])
                .find(|part| part.starts_with("http") || part.starts_with('/'))
        });
        [el.attr("data-src"), responsive, el.attr("src")]
            .into_iter()
            .flatten()
            .find_map(usable)
            .and_then(|u| page_url.join(u).ok())
            .map(|u| u.to_string())
    })
}

/// The card's date text: `.dates_inner`, `.date`, `.subtitle_date`, else the
/// first classic `.bottom` that parses as a date. `None` when it has none.
fn card_date(root: ElementRef<'_>) -> Option<String> {
    first_text(root, &[".dates_inner", ".date", ".subtitle_date"]).or_else(|| {
        root.select(&selector(".bottom:not(.additional_date)"))
            .map(element_text)
            .find(|t| matches!(parse_date_range(t), Ok(Some(_))))
    })
}

fn card_places(root: ElementRef<'_>) -> Vec<String> {
    let mut places: Vec<String> = root
        .select(&selector(".location, .subtitle_location"))
        .map(element_text)
        .collect();
    places.extend(
        root.select(&selector(".bottom:not(.additional_date)"))
            .map(element_text)
            .filter(|t| parse_date_range(t).is_err()),
    );
    places.retain(|p| !p.is_empty());
    places.dedup();
    places
}

const HEADING: [&str; 5] = [".heading_title", ".h1_heading", "h2", "h3", ".title"];
const SUBTITLE: [&str; 3] = [".heading_subtitle", ".h1_subtitle", ".subtitle"];

/// The current and forthcoming exhibitions on an Artlogic listing page
/// fetched from `page_url`, first card per id.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<Card> {
    let doc = Html::parse_document(html);
    let mut cards: Vec<Card> = Vec::new();
    for a in doc.select(&selector("a[href]")) {
        let Some(url) = a.value().attr("href").and_then(|h| page_url.join(h).ok()) else {
            continue;
        };
        if url.host_str() != page_url.host_str() {
            continue;
        }
        let Some(id) = exhibition_id(url.path()) else {
            continue;
        };
        if cards.iter().any(|c| c.id == id) {
            continue;
        }
        let Some(section) = section_of(a) else {
            continue;
        };
        if !keep_section(&section) {
            continue;
        }
        let root = card_root(a);
        let mut url = url;
        url.set_query(None);
        url.set_fragment(None);
        cards.push(Card {
            id,
            url: url.to_string(),
            section,
            heading: first_text(root, &HEADING),
            subtitle: first_text(root, &SUBTITLE),
            date_text: card_date(root),
            places: card_places(root),
            teaser: first_text(root, &[".description", ".caption"]),
            image_url: card_image(root, page_url),
        });
    }
    cards
}

/// A single exhibition's page (a listing path that redirected to its one
/// current show), read from its `.exhibition-header` and `og:` tags.
pub fn parse_single_show(html: &str, page_url: &Url) -> Option<Card> {
    let id = exhibition_id(page_url.path())?;
    let doc = Html::parse_document(html);
    let header = doc.select(&selector(".exhibition-header")).next()?;
    let status = doc
        .select(&selector("[id^=exhibition-status-]"))
        .next()
        .and_then(|e| e.value().id())
        .and_then(|id| id.strip_prefix("exhibition-status-"))
        .unwrap_or("current")
        .to_string();
    if !keep_section(&status) {
        return None;
    }
    let meta = |p: &str| {
        doc.select(&selector(&format!("meta[property=\"{p}\"]")))
            .next()
            .and_then(|m| m.value().attr("content"))
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string)
    };
    // The show's own page, without the Works/Overview subsection.
    let mut url = page_url.clone();
    url.set_query(None);
    url.set_fragment(None);
    let first_segment = url.path().split('/').nth(2).unwrap_or_default().to_string();
    url.set_path(&format!("/exhibitions/{first_segment}/"));
    Some(Card {
        id,
        url: url.to_string(),
        section: status,
        heading: first_text(header, &HEADING),
        subtitle: first_text(header, &[".h1_subtitle", ".heading_subtitle"]),
        date_text: first_text(header, &[".subtitle_date", ".date"]),
        places: header
            .select(&selector(".subtitle_location, .location"))
            .map(element_text)
            .filter(|t| !t.is_empty())
            .collect(),
        teaser: meta("og:description"),
        image_url: meta("og:image").filter(|u| u.contains("/usr/images/")),
    })
}

/// Phrases that mark a listed item as happening away from the gallery: art
/// fair booths, biennale presentations.
const OFFSITE_PHRASES: [&str; 12] = [
    "art fair",
    "biennale",
    "biennial",
    "frieze",
    "1-54",
    "pad london",
    "armory show",
    "art basel",
    "lapada",
    "tefaf",
    "international art exhibition",
    "institutional exhibition",
];

/// Whether `text` names an off-site presentation: an [`OFFSITE_PHRASES`]
/// phrase, or a fair "Booth"/"Stand(s)" followed by a number (`Stand 3`,
/// `Booth D10`, `Stand G1`).
pub fn is_offsite(text: &str) -> bool {
    let lower = text.to_lowercase();
    if OFFSITE_PHRASES.iter().any(|p| lower.contains(p)) {
        return true;
    }
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || c == ',' || c == '|')
        .filter(|w| !w.is_empty())
        .collect();
    words.windows(2).any(|w| {
        matches!(w[0], "booth" | "stand" | "stands") && w[1].chars().any(|c| c.is_ascii_digit())
    })
}

/// Parse an Artlogic date line into its first and last day. `Ok(None)` for
/// open-ended text ("Until …", "Ongoing").
pub fn parse_date_range(text: &str) -> Result<Option<(NaiveDate, NaiveDate)>, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date {text:?}"));
    let mut s = clean_text(text)
        .to_lowercase()
        .replace(['–', '—', '‑', '−'], "-")
        .replace(',', " ");
    for prefix in ["current:", "forthcoming:", "upcoming:", "dates:"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim().to_string();
        }
    }
    let first_word = s.split_whitespace().next().unwrap_or_default();
    if ["until", "ongoing", "from"].contains(&first_word) {
        return Ok(None);
    }
    // Ropac appends the opening to the dates: "13 October—18 December 2026
    // Opening Tuesday 13 October, 6—8pm".
    for marker in [" opening", " private view", " reception"] {
        if let Some(i) = s.find(marker) {
            s.truncate(i);
        }
    }
    let (start, end) = match s.split_once('-') {
        Some((a, b)) => (Some(a.trim()), b.trim()),
        None => (None, s.trim()),
    };
    let start = start.map(|s| parse_side(s).ok_or_else(err)).transpose()?;
    let end = parse_side(end).ok_or_else(err)?;
    // "October 15 - 31, 2026": the end takes the start's month.
    let end_month = end.month.or(start.as_ref().and_then(|s| s.month));
    let (Some(end_day), Some(end_month), Some(end_year)) = (end.day, end_month, end.year) else {
        return Err(err());
    };
    let last = NaiveDate::from_ymd_opt(end_year, end_month, end_day).ok_or_else(err)?;
    let first = match start {
        None => last,
        Some(start) => {
            let month = start.month.unwrap_or(end_month);
            let day = start.day.ok_or_else(err)?;
            match start.year {
                Some(y) => NaiveDate::from_ymd_opt(y, month, day).ok_or_else(err)?,
                None => {
                    let d = NaiveDate::from_ymd_opt(last.year(), month, day).ok_or_else(err)?;
                    if d > last {
                        NaiveDate::from_ymd_opt(last.year() - 1, month, day).ok_or_else(err)?
                    } else {
                        d
                    }
                }
            }
        }
    };
    if first > last {
        return Err(err());
    }
    Ok(Some((first, last)))
}

#[derive(Debug, Default)]
struct Side {
    day: Option<u32>,
    month: Option<u32>,
    year: Option<i32>,
}

fn month_number(word: &str) -> Option<u32> {
    let word = word.trim_end_matches('.');
    if word.len() < 3 || !word.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let prefix = word.get(..3)?;
    let n = MONTHS.iter().position(|m| *m == prefix)? as u32 + 1;
    // "sept" is the one common abbreviation longer than three letters.
    const FULL: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    (FULL[n as usize - 1].starts_with(word)).then_some(n)
}

const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// One side of a range: "18 sep 2026", "september 3", "3.10.2026", "9",
/// "november 6 2026" (commas already removed).
fn parse_side(s: &str) -> Option<Side> {
    let s = s.trim();
    // Dotted numeric: d.m[.yyyy]
    if s.contains('.') && s.chars().all(|c| c.is_ascii_digit() || c == '.') {
        let parts: Vec<&str> = s.split('.').filter(|p| !p.is_empty()).collect();
        return match parts.as_slice() {
            [d, m] => Some(Side {
                day: Some(d.parse().ok()?),
                month: Some(m.parse().ok()?),
                year: None,
            }),
            [d, m, y] => Some(Side {
                day: Some(d.parse().ok()?),
                month: Some(m.parse().ok()?),
                year: Some(y.parse().ok()?),
            }),
            _ => None,
        };
    }
    let mut side = Side::default();
    for token in s.split_whitespace() {
        let token = token.trim_end_matches(['.', ':']);
        if WEEKDAYS.iter().any(|w| token.starts_with(w)) && month_number(token).is_none() {
            continue;
        }
        let digits = ["st", "nd", "rd", "th"]
            .iter()
            .find_map(|suffix| token.strip_suffix(suffix))
            .unwrap_or(token);
        if let Ok(n) = digits.parse::<u32>() {
            if digits.len() == 4 && side.year.is_none() {
                side.year = Some(n as i32);
            } else if (1..=31).contains(&n) && side.day.is_none() && digits.len() <= 2 {
                side.day = Some(n);
            } else {
                return None;
            }
        } else if side.month.is_none() {
            side.month = Some(month_number(token)?);
        } else {
            return None;
        }
    }
    (side.day.is_some() || side.month.is_some()).then_some(side)
}

fn text_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
}

/// Normalise an Artlogic [`RawEvent`] payload for a gallery.
pub fn normalise_payload(
    payload: &Value,
    config: &ArtlogicConfig,
) -> Result<Option<NewEvent>, SourceError> {
    let card = payload
        .get("card")
        .ok_or_else(|| SourceError::Parse("payload without card".into()))?;
    let heading = text_of(card.get("heading"));
    let subtitle = text_of(card.get("subtitle"));
    let places: Vec<String> = card
        .get("places")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| text_of(Some(p)))
        .collect();
    let title = match (&heading, &subtitle) {
        (Some(h), Some(s)) if !h.to_lowercase().contains(&s.to_lowercase()) => {
            format!("{}: {s}", h.trim_end_matches(':'))
        }
        (Some(h), _) => h.clone(),
        (None, Some(s)) => s.clone(),
        (None, None) => return Err(SourceError::Parse("card without a title".into())),
    };

    let labels: Vec<&str> = [heading.as_deref(), subtitle.as_deref()]
        .into_iter()
        .flatten()
        .chain(places.iter().map(String::as_str))
        .collect();
    let skip = |text: &str| {
        is_offsite(text) || {
            let lower = text.to_lowercase();
            config
                .skip_match
                .iter()
                .any(|m| lower.contains(&m.to_lowercase()))
        }
    };
    if labels.iter().any(|t| skip(t)) {
        return Ok(None);
    }

    let (venue_name, address) =
        if config.locations.is_empty() || (places.is_empty() && config.unlabelled_at_venue) {
            (config.venue.name.clone(), config.venue.address.clone())
        } else {
            let place = places.join(" | ").to_lowercase();
            let Some(loc) = config
                .locations
                .iter()
                .find(|l| place.contains(&l.matches.to_lowercase()))
            else {
                return Ok(None);
            };
            (
                loc.name
                    .clone()
                    .unwrap_or_else(|| config.venue.name.clone()),
                loc.address.clone().or_else(|| config.venue.address.clone()),
            )
        };

    let Some(date_text) = text_of(card.get("date_text")) else {
        return Ok(None);
    };
    let Some((first, last)) =
        parse_date_range(&date_text).map_err(|e| SourceError::Parse(format!("{title:?}: {e}")))?
    else {
        return Ok(None);
    };
    let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
    let starts_at = midnight(first);

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(&venue_name)),
        title,
        description: clean_description(card.get("teaser").and_then(Value::as_str)),
        venue_name: Some(venue_name),
        address,
        lat: None,
        lng: None,
        starts_at,
        ends_at: (last > first).then(|| midnight(last)),
        all_day: true,
        price: Price::default(),
        url: card.get("url").and_then(Value::as_str).map(str::to_string),
        image_url: card
            .get("image_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        category: Category::Exhibition,
        tags: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(text: &str) -> Option<(String, String)> {
        parse_date_range(text)
            .unwrap_or_else(|e| panic!("{text:?}: {e}"))
            .map(|(a, b)| (a.to_string(), b.to_string()))
    }

    fn r(a: &str, b: &str) -> Option<(String, String)> {
        Some((a.into(), b.into()))
    }

    #[test]
    fn parses_every_listing_date_format() {
        assert_eq!(range("18 Sep - 7 Nov 2026"), r("2026-09-18", "2026-11-07"));
        assert_eq!(
            range("17 September – 14 November 2026"),
            r("2026-09-17", "2026-11-14")
        );
        assert_eq!(
            range("September 3 - October 10, 2026"),
            r("2026-09-03", "2026-10-10")
        );
        assert_eq!(
            range("11 June–26 September 2026"),
            r("2026-06-11", "2026-09-26")
        );
        assert_eq!(range("3.9 - 3.10.2026"), r("2026-09-03", "2026-10-03"));
        assert_eq!(range("9 - 26 Sep 2026"), r("2026-09-09", "2026-09-26"));
        assert_eq!(
            range("3 September\u{2014}1 October 2026"),
            r("2026-09-03", "2026-10-01")
        );
        assert_eq!(
            range("November 6, 2026 - January 9, 2027"),
            r("2026-11-06", "2027-01-09")
        );
        assert_eq!(
            range("November 19 - December 23, 2026"),
            r("2026-11-19", "2026-12-23")
        );
        assert_eq!(
            range("13 November 2026 – 23 January 2027"),
            r("2026-11-13", "2027-01-23")
        );
        assert_eq!(
            range("Forthcoming:  8 Nov 2026 - 18 Apr 2027"),
            r("2026-11-08", "2027-04-18")
        );
        assert_eq!(range("27 Mar 2026"), r("2026-03-27", "2026-03-27"));
        assert_eq!(
            range("3 – 12 September 2026"),
            r("2026-09-03", "2026-09-12")
        );
        assert_eq!(
            range("October 15 - 31, 2026"),
            r("2026-10-15", "2026-10-31")
        );
        assert_eq!(
            range("13 October\u{2014}18 December 2026 Opening Tuesday 13 October, 6\u{2014}8pm"),
            r("2026-10-13", "2026-12-18")
        );
    }

    #[test]
    fn a_start_without_a_year_after_the_end_month_is_the_year_before() {
        assert_eq!(range("5 Dec - 23 Jan 2027"), r("2026-12-05", "2027-01-23"));
        assert_eq!(range("20.12 - 9.1.2027"), r("2026-12-20", "2027-01-09"));
    }

    #[test]
    fn open_ended_dates_are_skips() {
        assert_eq!(range("Until 1 October 2026"), None);
        assert_eq!(range("Ongoing"), None);
    }

    #[test]
    fn unrecognised_dates_are_errors() {
        for text in [
            "Soon",
            "Opening Reception: Thursday, 10 September, 6 - 8 pm",
            "Conduit Street",
            "2026",
            "7 Nov 2026 - 3 Oct 2026",
            "31 Feb 2026",
        ] {
            assert!(parse_date_range(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn exhibition_ids() {
        assert_eq!(
            exhibition_id("/exhibitions/249-colin-self-unseen/"),
            Some(249)
        );
        assert_eq!(exhibition_id("/exhibitions/685/"), Some(685));
        assert_eq!(exhibition_id("/exhibitions/582/overview/"), Some(582));
        assert_eq!(exhibition_id("/exhibitions/258-entwined/works/"), Some(258));
        assert_eq!(exhibition_id("/exhibitions/current/"), None);
        assert_eq!(exhibition_id("/exhibitions/location/1/"), None);
        assert_eq!(exhibition_id("/exhibitions/2026x/"), None);
        assert_eq!(exhibition_id("/online-exhibitions/251-x/"), None);
    }

    #[test]
    fn offsite_presentations() {
        for text in [
            "Frieze London",
            "Booth D10",
            "Stand 3, Saatchi Gallery, London",
            "Affordable Art Fair",
            "Stands 3 & 16",
            "THE 61ST INTERNATIONAL ART EXHIBITION OF LA BIENNALE DI VENEZIA",
            "Institutional exhibitions",
            "1-54 London",
            "PAD London",
        ] {
            assert!(is_offsite(text), "{text:?}");
        }
        for text in [
            "Where I Stand",
            "Colin Self",
            "Golden Square",
            "Stand-up paintings",
            "London Gallery I",
        ] {
            assert!(!is_offsite(text), "{text:?}");
        }
    }

    #[test]
    fn config_needs_a_venue_and_rejects_typos() {
        assert!(ArtlogicConfig::from_json(None).is_err());
        assert!(ArtlogicConfig::from_json(Some(&json!({}))).is_err());
        let ok = ArtlogicConfig::from_json(Some(&json!({"venue": {"name": "G"}}))).unwrap();
        assert_eq!(ok.listing_paths, ["/exhibitions/"]);
        assert!(
            ArtlogicConfig::from_json(Some(&json!({"venue": {"name": "G"}, "london": []})))
                .is_err()
        );
        assert!(
            ArtlogicConfig::from_json(Some(&json!({"venue": {"name": "G"}, "listing_paths": []})))
                .is_err()
        );
        assert!(
            ArtlogicConfig::from_json(Some(
                &json!({"venue": {"name": "G"}, "locations": [{"match": " "}]})
            ))
            .is_err()
        );
    }
}
