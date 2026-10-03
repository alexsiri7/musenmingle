//! The Photographers' Gallery (Soho, W1) — listing-only CSS scraper over the
//! paginated "What's on" page.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): the Drupal default;
//!   `User-agent: *` disallows only core/profile/admin/search/user paths, so
//!   `/whats-on` and its `?page=N` pages are allowed.
//! * There is no JSON-LD. `/whats-on` lists current and upcoming items as
//!   `article.o-event.o-teaser` cards: a post type
//!   (`.o-teaser__post-type`: "Exhibition", "Talks & Events", "Tours",
//!   "Workshops & Courses", "Soho Photography Quarter", sometimes two joined
//!   by a comma), a date line (`p.o-teaser__date`), the title link
//!   (`h3.o-teaser__title a`, to `/whats-on/<slug>`) and a summary
//!   (`.o-teaser__body-text`; the site nests a stray `<p>` inside it, which
//!   HTML5 tree construction closes early, so the real text lands in the
//!   following sibling `<p>` instead — handled in [`parse_listing`]). The
//!   pager's `rel="next"` link is followed, at most [`MAX_PAGES`] pages.
//!   Detail pages add only longer prose (tickets are sold on another site),
//!   so no detail page is fetched.
//! * Date lines are either a date-only range (`18 Sep 2026 - 15 Nov 2026`),
//!   stored as London midnight of the first and last day, or a London
//!   wall-clock start and one date (`6:30pm, Thu 26 Nov 2026`), stored with
//!   no end. Anything else is a parse error.
//! * Category from the post type: exhibitions and the Soho Photography
//!   Quarter's outdoor displays → exhibition; talks & events, bookshop events
//!   (book launches and signings) and exhibition tours → talk; workshops &
//!   courses → workshop. Items without a post type (the photobooth), open
//!   calls, other types and runs of dates that aren't exhibitions
//!   (multi-week courses, whose session times appear only on their pages)
//!   are skipped (`Ok(None)`); `qa_scope` tells the scraper check.
//! * Terms allow personal use only, so the seed row is facts + link only;
//!   the summary and image are still emitted as found (the upsert applies the
//!   policy).
//! * The listing states no prices, so price is unknown. Every item is placed
//!   at the gallery.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc};

pub const KEY: &str = "photographers-gallery";
/// Upper bound on listing pages per run (the site shows two).
pub const MAX_PAGES: usize = 5;
const LISTING_PATH: &str = "/whats-on";
const DETAIL_PREFIX: &str = "/whats-on/";
const VENUE_NAME: &str = "The Photographers' Gallery";
const VENUE_ADDRESS: &str = "16–18 Ramillies Street, London W1F 7LW";

pub struct PhotographersGallery {
    base_url: Url,
    max_pages: usize,
}

impl PhotographersGallery {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            max_pages: MAX_PAGES,
        }
    }

    /// Override the per-run cap on listing pages (tests).
    pub fn with_max_pages(mut self, n: usize) -> Self {
        self.max_pages = n;
        self
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn element_text(e: ElementRef<'_>) -> String {
    clean_text(&e.text().collect::<Vec<_>>().join(" "))
}

/// Extract the listing's teasers, in document order, de-duplicated by slug.
/// `page_url` is the address the page was fetched from; links must stay on
/// its host.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<RawEvent> {
    let doc = Html::parse_document(html);
    let mut out: Vec<RawEvent> = Vec::new();
    for card in doc.select(&selector("article.o-event.o-teaser")) {
        let first = |s: &str| {
            card.select(&selector(s))
                .next()
                .map(element_text)
                .filter(|t| !t.is_empty())
        };
        let Some(a) = card.select(&selector("h3.o-teaser__title a[href]")).next() else {
            continue;
        };
        let Some(Ok(url)) = a.value().attr("href").map(|h| page_url.join(h)) else {
            continue;
        };
        let Some(slug) = url
            .path()
            .strip_prefix(DETAIL_PREFIX)
            .filter(|s| !s.is_empty() && !s.contains('/'))
        else {
            continue;
        };
        let title = element_text(a);
        if title.is_empty()
            || url.host() != page_url.host()
            || url.query().is_some()
            || out.iter().any(|r| r.source_event_id == slug)
        {
            continue;
        }
        out.push(RawEvent {
            source_event_id: slug.to_string(),
            source_url: Some(url.to_string()),
            payload: json!({
                "url": url.as_str(),
                "title": title,
                "post_type": first(".o-teaser__post-type"),
                "date_text": first(".o-teaser__date"),
                // The site's markup nests a stray `<p>` inside
                // `.o-teaser__body-text`; per the HTML5 tree-construction
                // algorithm that implicitly closes the outer `<p>`, so the
                // real prose ends up as a following, unclassed sibling `<p>`
                // rather than inside the matched node. Prefer the matched
                // node's own text (in case the markup is ever fixed), and
                // fall back to that sibling.
                "summary": card
                    .select(&selector(".o-teaser__body-text"))
                    .next()
                    .map(element_text)
                    .filter(|t| !t.is_empty())
                    .or_else(|| {
                        card.select(&selector(".o-teaser__body-text ~ p"))
                            .next()
                            .map(element_text)
                            .filter(|t| !t.is_empty())
                    }),
                "image_url": card
                    .select(&selector(".o-teaser__thumb img[data-srcset]"))
                    .next()
                    .and_then(|i| i.value().attr("data-srcset"))
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|src| page_url.join(src).ok())
                    .map(String::from),
            }),
        });
    }
    out
}

/// The pager's next page (`rel="next"`), if any, resolved against `page_url`
/// and kept on its host and listing path.
pub fn next_page(html: &str, page_url: &Url) -> Option<Url> {
    let doc = Html::parse_document(html);
    let href = doc
        .select(&selector("nav.pager a[rel=next][href]"))
        .next()?
        .value()
        .attr("href")?;
    let url = page_url.join(href).ok()?;
    (url.host() == page_url.host() && url.path() == page_url.path() && url != *page_url)
        .then_some(url)
}

/// When an item happens, from its date line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// First and last day (equal for a single day), no time given.
    Days(NaiveDate, NaiveDate),
    /// A start date and London wall-clock time.
    Starts(NaiveDate, NaiveTime),
}

/// "Thu 26 Nov 2026" / "04 Aug 2026" (weekday optional, month abbreviated
/// or full) → date.
fn parse_day(s: &str) -> Option<NaiveDate> {
    let mut tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens
        .first()
        .is_some_and(|t| t.chars().all(|c| c.is_ascii_alphabetic()))
    {
        tokens.remove(0);
    }
    let joined = tokens.join(" ");
    NaiveDate::parse_from_str(&joined, "%d %b %Y")
        .or_else(|_| NaiveDate::parse_from_str(&joined, "%d %B %Y"))
        .ok()
}

/// "6:30pm" / "11:00am" / "3pm" → time.
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

/// Parse a teaser's date line (see the module docs for the two forms).
pub fn parse_when(text: &str) -> Result<When, SourceError> {
    let err = || SourceError::Parse(format!("unrecognised date line {text:?}"));
    let text = clean_text(text);
    if let Some((time, day)) = text.split_once(',') {
        let time = parse_clock(time).ok_or_else(err)?;
        let day = parse_day(day).ok_or_else(err)?;
        return Ok(When::Starts(day, time));
    }
    let (first, last) = match text.split_once(['-', '–', '—']) {
        Some((a, b)) => (parse_day(a), parse_day(b)),
        None => (parse_day(&text), parse_day(&text)),
    };
    match (first, last) {
        (Some(first), Some(last)) if first <= last => Ok(When::Days(first, last)),
        _ => Err(err()),
    }
}

/// Category from the teaser's post type(s); `None` means out of scope.
pub fn category(post_type: &str) -> Option<Category> {
    let types: Vec<String> = post_type
        .split([',', '‚'])
        .map(|t| clean_text(t).to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let has = |want: &str| types.iter().any(|t| t == want);
    if has("exhibition") || has("exhibitions") || has("soho photography quarter") {
        Some(Category::Exhibition)
    } else if has("workshops & courses") {
        Some(Category::Workshop)
    } else if has("talks & events") || has("bookshop event") || has("tours") {
        Some(Category::Talk)
    } else {
        None
    }
}

fn is_open_call(title: &str) -> bool {
    title.to_lowercase().starts_with("open call")
}

/// Normalise a Photographers' Gallery [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("item without title".into()))?;
    let Some(category) = text("post_type").and_then(category) else {
        return Ok(None);
    };
    if is_open_call(&title) {
        return Ok(None);
    }
    let date_text = text("date_text")
        .ok_or_else(|| SourceError::Parse(format!("{title:?} has no date line")))?;
    let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
    let (starts_at, ends_at, all_day): (DateTime<Utc>, _, _) = match parse_when(date_text)? {
        When::Days(first, last) if last > first && category != Category::Exhibition => {
            return Ok(None);
        }
        When::Days(first, last) => (
            midnight(first),
            (last > first).then(|| midnight(last)),
            true,
        ),
        When::Starts(day, time) => (london_to_utc(day.and_time(time)), None, false),
    };

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(text("summary")),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at,
        ends_at,
        all_day,
        price: Default::default(),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: vec!["photography".to_string()],
    }))
}

#[async_trait]
impl Source for PhotographersGallery {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let mut url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let mut out: Vec<RawEvent> = Vec::new();
        for page in 0..self.max_pages {
            let html = match ctx.get_text(&url).await {
                Ok(html) => html,
                // The first page failing fails the run; a later one is a
                // soft error and the items found so far are kept.
                Err(e) if page > 0 => {
                    ctx.report_error(format!("listing page {}: {e}", page + 1));
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            for raw in parse_listing(&html, &url) {
                if !out.iter().any(|r| r.source_event_id == raw.source_event_id) {
                    out.push(raw);
                }
            }
            match next_page(&html, &url) {
                Some(next) => url = next,
                None => break,
            }
        }
        if out.is_empty() {
            return Err(SourceError::Parse(
                "no event teasers found on the What's on page".into(),
            ));
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only items whose post type is Exhibition, Soho Photography Quarter, \
             Talks & Events, Bookshop Event, Tours or Workshops & Courses. Left out \
             on purpose: items with no post type (the Autofoto photobooth), open \
             calls, and anything but an exhibition whose date line is a range of \
             days (courses such as \"Collecting Photography\", whose session times \
             appear only on their own pages).",
        )
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

    #[test]
    fn parses_date_lines() {
        assert_eq!(
            parse_when("18 Sep 2026 - 15 Nov 2026").unwrap(),
            When::Days(d("2026-09-18"), d("2026-11-15"))
        );
        assert_eq!(
            parse_when("14 Oct 2026 - 21 Feb 2027").unwrap(),
            When::Days(d("2026-10-14"), d("2027-02-21"))
        );
        assert_eq!(
            parse_when("Fri 18 Sep 2026 - Sun 15 Nov 2026").unwrap(),
            When::Days(d("2026-09-18"), d("2026-11-15"))
        );
        assert_eq!(
            parse_when("Sat 31 Oct 2026").unwrap(),
            When::Days(d("2026-10-31"), d("2026-10-31"))
        );
        assert_eq!(
            parse_when("3:00pm, Sat 31 Oct 2026").unwrap(),
            When::Starts(d("2026-10-31"), t(15, 0))
        );
        assert_eq!(
            parse_when("11:00am, Sat 14 Nov 2026").unwrap(),
            When::Starts(d("2026-11-14"), t(11, 0))
        );
        assert_eq!(
            parse_when("6:30pm, Thu 04 Feb 2027").unwrap(),
            When::Starts(d("2027-02-04"), t(18, 30))
        );
        assert_eq!(
            parse_when("12:00pm, Sat 14 Nov 2026").unwrap(),
            When::Starts(d("2026-11-14"), t(12, 0))
        );
    }

    #[test]
    fn unrecognised_date_lines_are_errors() {
        for text in [
            "Autumn 2026",
            "15 Nov 2026 - 18 Sep 2026",
            "18:30, Thu 26 Nov 2026",
            "13:00pm, Thu 26 Nov 2026",
            "26 Nov",
            "",
        ] {
            assert!(parse_when(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn categories_from_post_types() {
        assert_eq!(category("Exhibition"), Some(Category::Exhibition));
        assert_eq!(
            category("Soho Photography Quarter"),
            Some(Category::Exhibition)
        );
        assert_eq!(category("Workshops & Courses"), Some(Category::Workshop));
        assert_eq!(category("Talks & Events"), Some(Category::Talk));
        assert_eq!(
            category("Talks & Events\u{201a} Bookshop Event"),
            Some(Category::Talk)
        );
        assert_eq!(category("Tours"), Some(Category::Talk));
        assert_eq!(category("Bookshop Event"), Some(Category::Talk));
        assert_eq!(category(""), None);
    }

    fn payload(post_type: Option<&str>, title: &str, date: &str) -> Value {
        json!({
            "url": "https://thephotographersgallery.org.uk/whats-on/x",
            "title": title,
            "post_type": post_type,
            "date_text": date,
        })
    }

    #[test]
    fn timed_tour_is_not_all_day() {
        let e = normalise_payload(&payload(
            Some("Tours"),
            "Exhibition Tour",
            "6:30pm, Thu 26 Nov 2026",
        ))
        .unwrap()
        .unwrap();
        // GMT in November.
        assert_eq!(e.starts_at.to_rfc3339(), "2026-11-26T18:30:00+00:00");
        assert_eq!(e.ends_at, None);
        assert!(!e.all_day);
        assert_eq!(e.category, Category::Talk);
        // BST in October.
        let e = normalise_payload(&payload(
            Some("Tours"),
            "Exhibition Tour",
            "3:00pm, Sat 24 Oct 2026",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-24T14:00:00+00:00");
    }

    #[test]
    fn exhibition_range_is_all_day() {
        let e = normalise_payload(&payload(
            Some("Exhibition"),
            "Roots",
            "18 Sep 2026 - 15 Nov 2026",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-09-17T23:00:00+00:00");
        assert_eq!(
            e.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-11-15T00:00:00+00:00")
        );
        assert!(e.all_day);
    }

    #[test]
    fn skips_courses_runs_open_calls_and_untyped_items() {
        for p in [
            payload(
                Some("Workshops & Courses"),
                "Course | Collecting Photography 2026",
                "12 Oct 2026 - 02 Nov 2026",
            ),
            payload(
                Some("Talks & Events"),
                "Open Call | Future of Archives",
                "04 Aug 2026 - 01 Nov 2026",
            ),
            payload(None, "Autofoto photobooth", "01 Mar 2026 - 31 Dec 2026"),
            payload(Some("Shop"), "Print sale", "Sat 31 Oct 2026"),
        ] {
            assert!(normalise_payload(&p).unwrap().is_none(), "{p}");
        }
    }

    #[test]
    fn single_day_workshop_is_kept() {
        let e = normalise_payload(&payload(
            Some("Workshops & Courses"),
            "Cyanotype workshop",
            "11:00am, Sat 14 Nov 2026",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.category, Category::Workshop);
        assert!(!e.all_day);
    }

    #[test]
    fn missing_date_is_an_error() {
        let p = json!({"title": "No date", "post_type": "Exhibition"});
        assert!(normalise_payload(&p).is_err());
    }

    #[test]
    fn next_page_stays_on_the_listing() {
        let page = Url::parse("https://thephotographersgallery.org.uk/whats-on").unwrap();
        let html = |href: &str| {
            format!(r#"<nav class="pager"><a href="{href}" rel="next">Next</a></nav>"#)
        };
        assert_eq!(
            next_page(&html("?page=1"), &page)
                .map(String::from)
                .as_deref(),
            Some("https://thephotographersgallery.org.uk/whats-on?page=1")
        );
        assert_eq!(
            next_page(&html("https://elsewhere.example/?page=1"), &page),
            None
        );
        assert_eq!(next_page(&html("/shop?page=1"), &page), None);
        assert_eq!(next_page("<nav class=\"pager\"></nav>", &page), None);
    }

    #[test]
    fn the_qa_scope_names_what_is_kept_and_left_out() {
        let scope = PhotographersGallery::new(Url::parse("https://x.test/").unwrap())
            .qa_scope()
            .unwrap();
        for kept in [
            "Exhibition",
            "Soho Photography Quarter",
            "Talks & Events",
            "Bookshop Event",
            "Tours",
            "Workshops & Courses",
        ] {
            assert!(scope.contains(kept), "{kept}");
            assert!(category(kept).is_some(), "{kept}");
        }
        for word in ["no post type", "photobooth", "open", "range of", "courses"] {
            assert!(scope.contains(word), "{word}");
        }
    }
}
