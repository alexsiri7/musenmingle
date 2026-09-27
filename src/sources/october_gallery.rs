//! October Gallery (Bloomsbury, WC1) — exhibitions from the JSON-LD on their
//! detail pages, and talks from the Events page.
//!
//! * robots.txt (checked 2026-09-27, saved as a fixture): `User-agent: *`
//!   allows `/` and disallows only admin, API and AJAX paths (`/artlogic/`,
//!   `/api/`, `/results.php`, …).
//! * `/exhibitions/` has no JSON-LD. Its "Current" and "Forthcoming"
//!   sections link each show (`.exhib-title h2 a` → `/exhibitions/<slug>`);
//!   everything from the "Recent" anchor (`a[name="previous"]`) on, and the
//!   by-year menu, is the archive. Unlinked items (art-fair booths
//!   elsewhere) have no page and are left out.
//! * Each exhibition page carries one `ExhibitionEvent` with date-only
//!   `startDate`/`endDate` (matching the printed "8th October - 21st
//!   November"), stored `all_day` at London midnight of the first and last
//!   day, and an `offers` price (0: free). Its `description` is empty or a
//!   one-line lead, so the description is the page's own prose
//!   (`article.main-text > p`). At most [`MAX_DETAIL_PAGES`] pages a run.
//! * `/events/` has no JSON-LD and no per-event pages. Only its
//!   "Forthcoming Events" block (the `.flex-parent` that isn't the past
//!   events' `.moreflexitems`) is read: title (`.minor-heading`), a free-text
//!   `.date-words` block of `<br>`-separated lines ("Saturday 17th October,
//!   2026", "11 am – 12.30 pm", "October Gallery, Ground Floor", "Free
//!   Entry") and the description (`.minor-text`). Times are London
//!   wall-clock; a line may hold both date and time ("Saturday, 19th
//!   September 3 - 4:30 pm", "Saturday, March 22, 3 pm – 4.30 pm"), and a
//!   time line may carry a label or a place ("Talk 6.30 – 8 pm", "3.00 –
//!   4.30pm at October Gallery.", "7 – 8.15 pm (doors open 6 pm)"); lines
//!   about doors opening are not the start time. Every event on the page
//!   has a time, so a card without one is a parse error, never `all_day`. A
//!   date without a year is resolved against the London date of the fetch
//!   (`listed_on`, see [`infer_date`]). The link is the Events page itself
//!   (booking goes to Eventbrite, which is not the venue's page).
//! * Events: walk-throughs are talks; otherwise `map_category` on the title
//!   decides, and anything it doesn't place as a talk, workshop or community
//!   event (music lates, …) is skipped (`Ok(None)`). Price is `parse_price`
//!   over the date block ("Free Entry", "Tickets: £7 + booking fee").
//! * Everything is placed at the gallery.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::chisenhale_gallery::{infer_date, parse_time_range};
use super::jsonld::{extract_events, first_offer, image_url};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, map_category,
    normalise_title_for_key, parse_price, price_from_amounts,
};

pub const KEY: &str = "october-gallery";
/// Per-run cap on exhibition page fetches.
pub const MAX_DETAIL_PAGES: usize = 10;
const EXHIBITIONS_PATH: &str = "/exhibitions/";
const EVENTS_PATH: &str = "/events/";
const VENUE_NAME: &str = "October Gallery";
const VENUE_ADDRESS: &str = "24 Old Gloucester Street, London WC1N 3AL";
/// The gallery's own JSON-LD `geo`.
const VENUE_LAT: f64 = 51.5215;
const VENUE_LNG: f64 = -0.1236;
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

pub struct OctoberGallery {
    base_url: Url,
    max_detail_pages: usize,
}

impl OctoberGallery {
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

/// The paths (`/exhibitions/<slug>`) of the current and forthcoming
/// exhibitions, in page order, de-duplicated. `page_url` is the address the
/// listing was fetched from; links must stay on its host.
pub fn parse_listing(html: &str, page_url: &Url) -> Vec<String> {
    let doc = Html::parse_document(html);
    let mut out: Vec<String> = Vec::new();
    for e in doc.select(&selector(
        r#"#inner-content a[name="previous"], #inner-content .exhib-title h2 a[href]"#,
    )) {
        let Some(href) = e.value().attr("href") else {
            break;
        };
        let Ok(url) = page_url.join(href) else {
            continue;
        };
        let valid = url.host_str() == page_url.host_str()
            && url.query().is_none()
            && url
                .path()
                .strip_prefix(EXHIBITIONS_PATH)
                .is_some_and(|slug| !slug.is_empty() && !slug.contains('/'));
        if valid && !out.iter().any(|p| p == url.path()) {
            out.push(url.path().to_string());
        }
    }
    out
}

/// Parse an exhibition page (fetched from `page_url`) into a [`RawEvent`]:
/// its `ExhibitionEvent` node plus the page's prose. `None` without one.
pub fn parse_detail(html: &str, page_url: &Url) -> Option<RawEvent> {
    let doc = Html::parse_document(html);
    let event = extract_events(&doc).into_iter().next()?;
    let prose: Vec<String> = doc
        .select(&selector("#inner-content article.main-text > p"))
        .map(|p| p.inner_html())
        .collect();
    let slug = page_url.path().trim_matches('/').rsplit('/').next()?;
    Some(RawEvent {
        source_event_id: slug.to_string(),
        source_url: Some(page_url.to_string()),
        payload: json!({
            "kind": "exhibition",
            "url": page_url.as_str(),
            "jsonld": event,
            "prose": (!prose.is_empty()).then(|| prose.join("\n")),
        }),
    })
}

/// Parse the "Forthcoming Events" cards of the Events page. `page_url` is
/// the address the page was fetched from (it is each event's link);
/// `listed_on` is the London date of the fetch, kept for year inference.
pub fn parse_events(html: &str, page_url: &Url, listed_on: NaiveDate) -> Vec<RawEvent> {
    let doc = Html::parse_document(html);
    let mut out: Vec<RawEvent> = Vec::new();
    for card in doc.select(&selector(
        "#inner-content .flex-parent:not(.moreflexitems) > .flex-item > article",
    )) {
        let first = |s: &str| card.select(&selector(s)).next();
        let Some(title) = first(".minor-heading")
            .map(element_text)
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        let date_lines: Vec<String> = first(".date-words")
            .map(|d| {
                d.inner_html()
                    .split("<br>")
                    .map(clean_text)
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let id = normalise_title_for_key(&format!(
            "{title} {}",
            date_lines.first().map(String::as_str).unwrap_or_default()
        ));
        if out.iter().any(|r| r.source_event_id == id) {
            continue;
        }
        out.push(RawEvent {
            source_event_id: id,
            source_url: Some(page_url.to_string()),
            payload: json!({
                "kind": "event",
                "url": page_url.as_str(),
                "title": title,
                "date_lines": date_lines,
                "description": first(".minor-text").map(|d| d.inner_html()),
                "image_url": first("figure img[src]")
                    .and_then(|i| i.value().attr("src"))
                    .and_then(|src| page_url.join(src).ok())
                    .map(String::from),
                "listed_on": listed_on.to_string(),
            }),
        });
    }
    out
}

/// A printed day at the start of a line, "Saturday 17th October, 2026",
/// "Saturday, 19th September 3 - 4:30 pm" or "Saturday, March 22, 3 pm"
/// (weekday and year optional), and whatever follows it.
pub fn parse_day_line(line: &str, listed_on: NaiveDate) -> Option<(NaiveDate, String)> {
    let line = line.replace(',', " ");
    let mut tokens = line.split_whitespace().peekable();
    let name_index = |token: &str, names: &[&str]| {
        let lower = token.to_lowercase();
        if !lower.chars().all(char::is_alphabetic) {
            return None;
        }
        names.iter().position(|n| lower.starts_with(n))
    };
    let day_number = |token: &str| -> Option<u32> {
        token
            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .parse()
            .ok()
    };
    if tokens
        .peek()
        .is_some_and(|t| name_index(t, &WEEKDAYS).is_some())
    {
        tokens.next();
    }
    let first = tokens.next()?;
    let (day, month) = match name_index(first, &MONTHS) {
        Some(month) => (day_number(tokens.next()?)?, month),
        None => (day_number(first)?, name_index(tokens.next()?, &MONTHS)?),
    };
    let month = month as u32 + 1;
    let year = tokens
        .peek()
        .filter(|t| t.len() == 4)
        .and_then(|t| t.parse::<i32>().ok());
    if year.is_some() {
        tokens.next();
    }
    let date = match year {
        Some(y) => NaiveDate::from_ymd_opt(y, month, day),
        None => infer_date(month, day, listed_on),
    }?;
    Some((date, tokens.collect::<Vec<_>>().join(" ")))
}

/// The time or time range in a line, after any label and before any place
/// or aside: "11 am – 12.30 pm", "Talk 6.30 – 8 pm", "3.00 – 4.30pm at
/// October Gallery.", "7 – 8.15 pm (doors open 6 pm)". Doors-opening lines
/// ("Bar and doors open 5.30 pm") are not the event's time.
pub fn parse_time_line(line: &str) -> Option<(NaiveTime, Option<NaiveTime>)> {
    let clock = line.split(" at ").next()?.split('(').next()?;
    if clock.to_lowercase().contains("doors open") {
        return None;
    }
    let clock: String = clock
        .split_whitespace()
        .skip_while(|w| !w.chars().any(|c| c.is_ascii_digit()))
        .collect();
    parse_time_range(&clock)
}

/// Category of an Events-page item; `None` means out of scope.
pub fn event_category(title: &str) -> Option<Category> {
    let lower = title.to_lowercase();
    if ["walk through", "walkthrough", "walk-through"]
        .iter()
        .any(|w| lower.contains(w))
    {
        return Some(Category::Talk);
    }
    map_category(&[title])
        .filter(|c| matches!(c, Category::Talk | Category::Workshop | Category::Community))
}

fn normalise_exhibition(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let ev = payload
        .get("jsonld")
        .ok_or_else(|| SourceError::Parse("payload without jsonld".into()))?;
    if ev
        .get("eventAttendanceMode")
        .and_then(Value::as_str)
        .is_some_and(|m| m.contains("OnlineEventAttendanceMode"))
    {
        return Ok(None);
    }
    let title = ev
        .get("name")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("ExhibitionEvent without name".into()))?;
    let day = |key: &str| {
        ev.get(key)
            .and_then(Value::as_str)
            .and_then(|s| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
    };
    let first = day("startDate")
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no date-only startDate")))?;
    let last = day("endDate").unwrap_or(first);
    if last < first {
        return Err(SourceError::Parse(format!(
            "{title:?} ends before it starts"
        )));
    }
    let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
    let starts_at = midnight(first);

    let price = first_offer(ev)
        .and_then(|o| {
            let amount: Decimal = match o.get("price")? {
                Value::String(s) => s.trim().parse().ok()?,
                Value::Number(n) => n.to_string().parse().ok()?,
                _ => return None,
            };
            Some(price_from_amounts(
                Some(amount),
                None,
                o.get("priceCurrency").and_then(Value::as_str),
            ))
        })
        .unwrap_or_default();
    let description = clean_description(payload.get("prose").and_then(Value::as_str))
        .or_else(|| clean_description(ev.get("description").and_then(Value::as_str)));

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        title,
        description,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: Some(VENUE_LAT),
        lng: Some(VENUE_LNG),
        starts_at,
        ends_at: (last > first).then(|| midnight(last)),
        all_day: true,
        price,
        url: payload
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        image_url: image_url(ev),
        category: Category::Exhibition,
        tags: vec![],
    }))
}

fn normalise_event(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let text = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = text("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event without title".into()))?;
    let Some(category) = event_category(&title) else {
        return Ok(None);
    };
    let listed_on = text("listed_on")
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .ok_or_else(|| SourceError::Parse(format!("{title:?}: no listed_on")))?;
    let lines: Vec<&str> = payload
        .get("date_lines")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let err = || SourceError::Parse(format!("{title:?}: unrecognised date lines {lines:?}"));
    let (day, rest) = lines
        .iter()
        .find_map(|l| parse_day_line(l, listed_on))
        .ok_or_else(err)?;
    let (start, end) = if rest.is_empty() {
        lines.iter().find_map(|l| parse_time_line(l))
    } else {
        parse_time_line(&rest)
    }
    .ok_or_else(err)?;
    let starts_at = london_to_utc(day.and_time(start));
    let ends_at = end
        .filter(|e| *e > start)
        .map(|e| london_to_utc(day.and_time(e)));

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
        all_day: false,
        price: parse_price(&lines.join("\n")),
        url: text("url").map(str::to_string),
        image_url: text("image_url").map(str::to_string),
        category,
        tags: vec![],
    }))
}

/// Normalise an October Gallery [`RawEvent`] payload (an exhibition page or
/// an Events-page card).
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    match payload.get("kind").and_then(Value::as_str) {
        Some("exhibition") => normalise_exhibition(payload),
        Some("event") => normalise_event(payload),
        other => Err(SourceError::Parse(format!(
            "unknown payload kind {other:?}"
        ))),
    }
}

#[async_trait]
impl Source for OctoberGallery {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = |path: &str| {
            self.base_url
                .join(path)
                .map_err(|e| SourceError::Config(e.to_string()))
        };
        let listing_url = url(EXHIBITIONS_PATH)?;
        let paths = parse_listing(&ctx.get_text(&listing_url).await?, &listing_url);
        if paths.is_empty() {
            return Err(SourceError::Parse(
                "no current or forthcoming exhibitions found on the Exhibitions page".into(),
            ));
        }
        let mut raws = Vec::new();
        for path in paths.iter().take(self.max_detail_pages) {
            let page_url = url(path)?;
            match ctx.get_text(&page_url).await {
                Ok(html) => match parse_detail(&html, &page_url) {
                    Some(raw) => raws.push(raw),
                    None => ctx.report_error(format!("{path}: no ExhibitionEvent JSON-LD")),
                },
                Err(e) => ctx.report_error(format!("{path}: {e}")),
            }
        }
        let events_url = url(EVENTS_PATH)?;
        match ctx.get_text(&events_url).await {
            Ok(html) => raws.extend(parse_events(&html, &events_url, london_date(Utc::now()))),
            Err(e) => ctx.report_error(format!("{EVENTS_PATH}: {e}")),
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

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn t(s: &str) -> NaiveTime {
        NaiveTime::parse_from_str(s, "%H:%M").unwrap()
    }

    #[test]
    fn parses_day_lines() {
        let listed_on = d("2026-09-27");
        let day = |line: &str| parse_day_line(line, listed_on);
        assert_eq!(
            day("Saturday 17th October, 2026"),
            Some((d("2026-10-17"), String::new()))
        );
        assert_eq!(
            day("Wednesday, 10th June, 2026"),
            Some((d("2026-06-10"), String::new()))
        );
        assert_eq!(
            day("Saturday, 19th September 3 - 4:30 pm"),
            Some((d("2026-09-19"), "3 - 4:30 pm".to_string()))
        );
        assert_eq!(
            day("Tuesday 12th January"),
            Some((d("2027-01-12"), String::new()))
        );
        assert_eq!(
            day("Saturday, March 22, 3 pm – 4.30 pm"),
            Some((d("2027-03-22"), "3 pm – 4.30 pm".to_string()))
        );
        assert_eq!(
            day("Saturday, 7th March, 2026 3 pm – 4.30 pm"),
            Some((d("2026-03-07"), "3 pm – 4.30 pm".to_string()))
        );
        for line in [
            "11 am – 12.30 pm",
            "October Gallery, Ground Floor",
            "Free Entry",
            "Tickets: £7 + booking fee",
            "",
        ] {
            assert_eq!(day(line), None, "{line:?}");
        }
    }

    #[test]
    fn parses_time_lines() {
        assert_eq!(
            parse_time_line("11 am – 12.30 pm"),
            Some((t("11:00"), Some(t("12:30"))))
        );
        assert_eq!(
            parse_time_line("3 - 4:30 pm"),
            Some((t("15:00"), Some(t("16:30"))))
        );
        assert_eq!(
            parse_time_line("6.00–8.30pm"),
            Some((t("18:00"), Some(t("20:30"))))
        );
        assert_eq!(
            parse_time_line("Talk 6.30 – 8 pm"),
            Some((t("18:30"), Some(t("20:00"))))
        );
        assert_eq!(
            parse_time_line("3.00 – 4.30pm at October Gallery."),
            Some((t("15:00"), Some(t("16:30"))))
        );
        assert_eq!(
            parse_time_line("7 – 8.15 pm (doors open 6 pm)"),
            Some((t("19:00"), Some(t("20:15"))))
        );
        for line in [
            "Bar and doors open 5.30 pm",
            "October Gallery, Ground Floor",
            "Saturday 17th October, 2026",
            "Tickets: £10 (plus Booking Fee)",
            "Free entry (booking essential)",
            "Theatre (2nd floor)",
            "Duration: 40 minutes",
        ] {
            assert_eq!(parse_time_line(line), None, "{line:?}");
        }
    }

    #[test]
    fn event_categories() {
        assert_eq!(
            event_category("Gallery Talk: Romuald Hazoumè in Conversation with Gerard Houghton"),
            Some(Category::Talk)
        );
        assert_eq!(
            event_category(
                "Exhibition Walkthrough with Sokari Douglas Camp in collaboration with WAAW"
            ),
            Some(Category::Talk)
        );
        assert_eq!(
            event_category(
                "Xanthe Somers: Poetic Threads | Exhibition Walk Through & Family Art Day"
            ),
            Some(Category::Talk)
        );
        assert_eq!(event_category("OG LATES x TAOSOL"), None);
        assert_eq!(event_category("1-54 Art Fair preview"), None);
    }

    fn event(date_lines: &[&str]) -> Value {
        json!({
            "kind": "event",
            "url": "https://www.octobergallery.co.uk/events/",
            "title": "Gallery Talk: Someone in Conversation",
            "date_lines": date_lines,
            "listed_on": "2026-09-27",
        })
    }

    #[test]
    fn timed_talk_on_its_own_lines() {
        let e = normalise_payload(&event(&[
            "Saturday 17th October, 2026",
            "11 am – 12.30 pm",
            "October Gallery, Ground Floor",
            "Free Entry",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-10-17T10:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-17T11:30:00+00:00");
        assert!(!e.all_day);
        assert!(e.price.is_free);
        assert_eq!(e.category, Category::Talk);
    }

    #[test]
    fn date_and_time_on_one_line() {
        let e = normalise_payload(&event(&["Saturday, 19th September  3 - 4:30 pm"]))
            .unwrap()
            .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2026-09-19T14:00:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-09-19T15:30:00+00:00");
    }

    #[test]
    fn labelled_time_after_doors_line_and_ticket_price() {
        let e = normalise_payload(&event(&[
            "Thursday, 29th May, 2025",
            "Bar and doors open 5.30 pm",
            "Talk 6.30 – 8 pm",
            "Tickets: £5 + booking fee",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(e.starts_at.to_rfc3339(), "2025-05-29T17:30:00+00:00");
        assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2025-05-29T19:00:00+00:00");
        assert!(!e.all_day);
        assert_eq!(e.price.min, Some(Decimal::from(5)));
        assert!(!e.price.is_free);
    }

    #[test]
    fn unreadable_event_dates_are_errors() {
        for lines in [
            &["Autumn 2026"][..],
            &[],
            &["Saturday 17th October, 2026 late"],
            &["Saturday 17th October, 2026", "Free Entry"],
        ] {
            assert!(normalise_payload(&event(lines)).is_err(), "{lines:?}");
        }
    }

    #[test]
    fn listing_stops_at_recent_and_stays_on_host() {
        let html = r#"<div id="inner-content">
            <div class="exhib-title"><h2><a href="https://elsewhere.example/exhibitions/away">Away</a></h2></div>
            <div class="exhib-title"><h2><a href="/exhibitions/now">Now</a></h2></div>
            <div class="exhib-title"><h2><a href="/exhibitions/now">Now again</a></h2></div>
            <div class="exhib-title"><h2>Art fair booth</h2></div>
            <div class="exhib-title"><h2><a href="/exhibitions/next">Next</a></h2></div>
            <a name="previous"></a>
            <div class="exhib-title"><h2><a href="/exhibitions/past">Past</a></h2></div>
        </div>"#;
        let page_url = Url::parse("https://www.octobergallery.co.uk/exhibitions/").unwrap();
        assert_eq!(
            parse_listing(html, &page_url),
            ["/exhibitions/now", "/exhibitions/next"]
        );
    }

    #[test]
    fn exhibition_without_dates_is_an_error() {
        let payload = json!({
            "kind": "exhibition",
            "jsonld": {"@type": "ExhibitionEvent", "name": "Show", "startDate": "2026-10-08T18:00"},
        });
        assert!(normalise_payload(&payload).is_err());
    }
}
