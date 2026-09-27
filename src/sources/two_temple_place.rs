//! Two Temple Place (Temple, WC2) — listing-only CSS scraper over the
//! "What's on" page.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): Yoast's default,
//!   `User-agent: *` with an empty `Disallow:`, so everything is allowed.
//! * There is no JSON-LD `Event` (only Yoast's WebPage/BreadcrumbList, on
//!   the listing and on event pages), and The Events Calendar's REST API is
//!   not installed (404). `/whats-on/` renders every upcoming event as an
//!   `article.filter-item` in `section.related-events-block.upcoming`,
//!   followed by some 540 past ones in `section…past`, which are ignored.
//!   Each card has the title link (`.copy-side a h1`, to `/events/<slug>/`,
//!   with a `<br>` between title and subtitle, joined as "Title: Subtitle"
//!   unless the subtitle starts in lower case), a date block (`.copy-side
//!   h2`, lines split by `<br>`), a price overlay (`.overlay`: "£12",
//!   "Free") and a summary. Event pages add nothing structured, so a run is
//!   one request after robots.txt.
//! * Date blocks are one or two date lines (`Sunday 27 September 2026 -`,
//!   `Mon 28 September 2026`) and an optional London wall-clock time range
//!   (`10:00am – 11:00am`):
//!   - one day with times → timed, `ends_at` the end time;
//!   - one day without times → all day;
//!   - several days → an exhibition's run (its times are daily opening
//!     hours), stored all day from London midnight of the first day to
//!     London midnight of the last; other multi-day items (performance runs,
//!     festivals) are skipped.
//! * The cards carry no type, so the category comes from the title, then the
//!   summary: guided and building tours → talk, else
//!   [`crate::normalise::map_category`] (workshops, talks, exhibitions).
//!   Music (opera, concerts, recitals, live music, choirs) and anything
//!   unmatched are skipped (`Ok(None)`).
//! * Every item is placed at the house.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_to_utc, map_category, parse_price,
};

pub const KEY: &str = "two-temple-place";
const LISTING_PATH: &str = "/whats-on/";
const DETAIL_PREFIX: &str = "/events/";
const VENUE_NAME: &str = "Two Temple Place";
const VENUE_ADDRESS: &str = "2 Temple Place, London WC2R 3BD";
/// Words that mark a music event (skipped).
const MUSIC_WORDS: &[&str] = &[
    "opera",
    "concert",
    "concerts",
    "recital",
    "choir",
    "choristers",
    "gig",
    "music",
];

pub struct TwoTemplePlace {
    base_url: Url,
}

impl TwoTemplePlace {
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

/// Extract the upcoming cards, in document order, de-duplicated by slug.
/// `page_url` is the address the listing was fetched from; links must stay
/// on its host.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<RawEvent> {
    let doc = Html::parse_document(html);
    let mut out: Vec<RawEvent> = Vec::new();
    let cards = selector("section.related-events-block.upcoming article.filter-item");
    for card in doc.select(&cards) {
        let first = |s: &str| {
            card.select(&selector(s))
                .next()
                .map(element_text)
                .filter(|t| !t.is_empty())
        };
        let Some(a) = card.select(&selector(".copy-side a[href]")).next() else {
            continue;
        };
        let Some(Ok(url)) = a.value().attr("href").map(|h| page_url.join(h)) else {
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
        let title = join_title_lines(a.text().map(clean_text).filter(|t| !t.is_empty()));
        if title.is_empty()
            || url.host() != page_url.host()
            || url.query().is_some()
            || out.iter().any(|r| r.source_event_id == slug)
        {
            continue;
        }
        let date_lines: Vec<String> = card
            .select(&selector(".copy-side h2"))
            .next()
            .map(|h2| {
                h2.text()
                    .map(clean_text)
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        out.push(RawEvent {
            source_event_id: slug.to_string(),
            source_url: Some(url.to_string()),
            payload: json!({
                "url": url.as_str(),
                "title": title,
                "date_lines": date_lines,
                "price": first(".image-container .overlay"),
                "summary": card.select(&selector(".copy-side p")).next().map(|p| p.inner_html()),
                "image_url": card
                    .select(&selector(".image-container img[data-original]"))
                    .next()
                    .and_then(|i| i.value().attr("data-original"))
                    .and_then(|src| page_url.join(src).ok())
                    .map(String::from),
            }),
        });
    }
    out
}

/// The title link's `<br>`-separated lines: a subtitle that starts in lower
/// case ("with Totally Thames Trust") continues the title, any other gets a
/// colon ("Gathering Rest: An Exhibition on Rest …").
fn join_title_lines(lines: impl Iterator<Item = String>) -> String {
    let mut title = String::new();
    for line in lines {
        if !title.is_empty() {
            let continues = line.chars().next().is_some_and(char::is_lowercase);
            title.push_str(if continues || title.ends_with(':') {
                " "
            } else {
                ": "
            });
        }
        title.push_str(&line);
    }
    title
}

/// When an item happens, from its date block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct When {
    pub first: NaiveDate,
    pub last: NaiveDate,
    /// London wall-clock start and optional end, if printed.
    pub times: Option<(NaiveTime, Option<NaiveTime>)>,
}

/// "Sunday 27 September 2026" / "Mon 28 September 2026" (weekday optional)
/// → date.
fn parse_day(s: &str) -> Option<NaiveDate> {
    let mut tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens
        .first()
        .is_some_and(|t| t.chars().all(|c| c.is_ascii_alphabetic()))
    {
        tokens.remove(0);
    }
    let joined = tokens.join(" ");
    NaiveDate::parse_from_str(&joined, "%d %B %Y")
        .or_else(|_| NaiveDate::parse_from_str(&joined, "%d %b %Y"))
        .ok()
}

/// "4:30pm" / "10:00am" / "11am" → time.
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

fn is_time_line(line: &str) -> bool {
    line.chars().next().is_some_and(|c| c.is_ascii_digit())
        && ["am", "pm"].iter().any(|s| line.to_lowercase().contains(s))
        && !line
            .chars()
            .any(|c| c.is_ascii_alphabetic() && !"apm".contains(c))
}

/// Parse a card's date block (its `<br>`-separated lines).
pub fn parse_when(lines: &[String]) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date block {lines:?}"));
    let mut days: Vec<NaiveDate> = Vec::new();
    let mut times = None;
    for line in lines {
        if is_time_line(line) {
            if times.is_some() {
                return Err(err());
            }
            let (start, end) = match line.split_once(['-', '–', '—']) {
                Some((a, b)) => (parse_clock(a), Some(parse_clock(b).ok_or_else(err)?)),
                None => (parse_clock(line), None),
            };
            times = Some((start.ok_or_else(err)?, end));
            continue;
        }
        for part in line.split(['-', '–', '—']) {
            let part = part.trim();
            if !part.is_empty() {
                days.push(parse_day(part).ok_or_else(err)?);
            }
        }
    }
    let (first, last) = match days.as_slice() {
        [d] => (*d, *d),
        [a, b] if a <= b => (*a, *b),
        _ => return Err(err()),
    };
    Ok(When { first, last, times })
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Category from the title, then the summary; `None` means out of scope.
pub fn category(title: &str, summary: &str) -> Option<Category> {
    for text in [title, summary] {
        let w = words(text);
        if w.iter().any(|w| MUSIC_WORDS.contains(&w.as_str())) {
            return None;
        }
        if w.iter().any(|w| w == "tour" || w == "tours") {
            return Some(Category::Talk);
        }
        if let Some(c) = map_category(&[text]) {
            return Some(c);
        }
    }
    None
}

/// Normalise a Two Temple Place [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("item without title".into()))?;
    let description = clean_description(text("summary"));
    let Some(category) = category(&title, description.as_deref().unwrap_or_default()) else {
        return Ok(None);
    };
    let lines: Vec<String> = payload
        .get("date_lines")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let when = parse_when(&lines).map_err(|e| SourceError::Parse(format!("{title:?}: {e}")))?;
    let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
    let (starts_at, ends_at, all_day): (DateTime<Utc>, _, _) = match when {
        When { first, last, .. } if last > first => {
            if category != Category::Exhibition {
                return Ok(None);
            }
            (midnight(first), Some(midnight(last)), true)
        }
        When {
            first, times: None, ..
        } => (midnight(first), None, true),
        When {
            first,
            times: Some((start, end)),
            ..
        } => {
            let starts_at = london_to_utc(first.and_time(start));
            let ends_at = end
                .map(|e| london_to_utc(first.and_time(e)))
                .filter(|e| *e > starts_at);
            (starts_at, ends_at, false)
        }
    };

    Ok(Some(NewEvent {
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
        price: text("price").map(parse_price).unwrap_or_default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: Vec::new(),
    }))
}

#[async_trait]
impl Source for TwoTemplePlace {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let html = ctx.get_text(&url).await?;
        let items = parse_listing(&html, &url);
        // An empty upcoming section is a real state between seasons, but a
        // page without the section at all means the markup changed.
        if items.is_empty() && !html.contains("related-events-block filters upcoming") {
            return Err(SourceError::Parse(
                "no upcoming events section on the What's on page".into(),
            ));
        }
        Ok(items)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
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

    fn lines(ls: &[&str]) -> Vec<String> {
        ls.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn parses_date_blocks() {
        assert_eq!(
            parse_when(&lines(&["Monday 5 October 2026", "10:00am – 11:00am"])).unwrap(),
            When {
                first: d("2026-10-05"),
                last: d("2026-10-05"),
                times: Some((t(10, 0), Some(t(11, 0))))
            }
        );
        assert_eq!(
            parse_when(&lines(&[
                "Sunday 27 September 2026 -",
                "Mon 28 September 2026",
                "4:30pm – 8:00pm"
            ]))
            .unwrap(),
            When {
                first: d("2026-09-27"),
                last: d("2026-09-28"),
                times: Some((t(16, 30), Some(t(20, 0))))
            }
        );
        assert_eq!(
            parse_when(&lines(&["Thursday 12 November 2026", "12:00pm"])).unwrap(),
            When {
                first: d("2026-11-12"),
                last: d("2026-11-12"),
                times: Some((t(12, 0), None))
            }
        );
        assert_eq!(
            parse_when(&lines(&["Saturday 23 January 2027 - Sun 18 April 2027"])).unwrap(),
            When {
                first: d("2027-01-23"),
                last: d("2027-04-18"),
                times: None
            }
        );
    }

    #[test]
    fn unrecognised_date_blocks_are_errors() {
        for ls in [
            &["Opening January 23"][..],
            &["Mon 28 September 2026 -", "Sunday 27 September 2026"],
            &["Monday 5 October 2026", "13:00pm – 14:00pm"],
            &["Monday 5 October 2026", "10:00 – 11:00"],
            &["10:00am"],
            &[],
        ] {
            assert!(parse_when(&lines(ls)).is_err(), "{ls:?}");
        }
    }

    #[test]
    fn joins_title_lines() {
        let j = |ls: &[&str]| join_title_lines(ls.iter().map(|l| l.to_string()));
        assert_eq!(
            j(&["Gathering Rest", "An Exhibition on Rest"]),
            "Gathering Rest: An Exhibition on Rest"
        );
        assert_eq!(
            j(&["Where the River Holds Us", "with Totally Thames Trust"]),
            "Where the River Holds Us with Totally Thames Trust"
        );
        assert_eq!(
            j(&["Wednesday Lates:", "Live Music"]),
            "Wednesday Lates: Live Music"
        );
        assert_eq!(j(&["Feel like zine-ing(?)"]), "Feel like zine-ing(?)");
    }

    #[test]
    fn categories_from_title_then_summary() {
        assert_eq!(
            category("Two Temple Place Building Tour", ""),
            Some(Category::Talk)
        );
        assert_eq!(
            category("Curators Tour The Weight of Being", ""),
            Some(Category::Talk)
        );
        assert_eq!(
            category("Family Workshop Upside Down Drawing", ""),
            Some(Category::Workshop)
        );
        assert_eq!(
            category("Gathering Rest An Exhibition on Rest", ""),
            Some(Category::Exhibition)
        );
        assert_eq!(
            category(
                "The Weight of Being",
                "A free exhibition of art and mental health"
            ),
            Some(Category::Exhibition)
        );
        assert_eq!(
            category(
                "Where the River Holds Us with Totally Thames Trust",
                "a site-responsive multisensory opera exploring water"
            ),
            None
        );
        assert_eq!(
            category("Wednesday Lates Live Music with Steve Skaith", ""),
            None
        );
        assert_eq!(category("Family Day at Two Temple Place", ""), None);
    }

    fn payload(title: &str, date_lines: &[&str], price: &str) -> Value {
        json!({
            "url": "https://twotempleplace.org/events/x/",
            "title": title,
            "date_lines": date_lines,
            "price": price,
        })
    }

    #[test]
    fn a_tour_is_timed_with_its_end() {
        let e = normalise_payload(&payload(
            "Two Temple Place Building Tour",
            &["Monday 5 October 2026", "1:00pm – 2:00pm"],
            "£12",
        ))
        .unwrap()
        .unwrap();
        // BST.
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-05T12:00:00+00:00");
        assert_eq!(
            e.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-05T13:00:00+00:00")
        );
        assert!(!e.all_day);
        assert_eq!(e.price.min.map(|p| p.to_string()).as_deref(), Some("12"));
        // GMT.
        let e = normalise_payload(&payload(
            "Two Temple Place Building Tour",
            &["Friday 11 December 2026", "10:30am – 11:30am"],
            "£12",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-12-11T10:30:00+00:00");
    }

    #[test]
    fn an_exhibition_run_is_all_day_despite_opening_hours() {
        let e = normalise_payload(&payload(
            "Gathering Rest An Exhibition on Rest",
            &[
                "Saturday 23 January 2027 -",
                "Sun 18 April 2027",
                "11:00am – 4:30pm",
            ],
            "Free",
        ))
        .unwrap()
        .unwrap();
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2027-01-23T00:00:00+00:00");
        assert_eq!(
            e.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2027-04-17T23:00:00+00:00")
        );
        assert!(e.price.is_free);
    }

    #[test]
    fn multi_day_non_exhibitions_are_skipped() {
        let p = payload(
            "Flowing through a workshop festival",
            &[
                "Saturday 3 October 2026 -",
                "Sun 4 October 2026",
                "11:00am – 5:00pm",
            ],
            "Free",
        );
        assert!(normalise_payload(&p).unwrap().is_none());
    }

    #[test]
    fn single_untimed_day_is_all_day_without_end() {
        let e = normalise_payload(&payload(
            "Family Workshop Paper Flowers",
            &["Saturday 3 October 2026"],
            "Free",
        ))
        .unwrap()
        .unwrap();
        assert!(e.all_day);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-02T23:00:00+00:00");
        assert_eq!(e.ends_at, None);
    }
}
