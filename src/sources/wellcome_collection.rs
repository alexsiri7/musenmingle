//! Wellcome Collection, through its public Content API (issue #34).
//!
//! * Endpoint: `GET {base}/content/v0/events?timespan=future&pageSize=100`
//!   (no key; `api.wellcomecollection.org` has no robots.txt, i.e. allow
//!   all). There are a few dozen future items, so one request per run is
//!   usual; at most [`MAX_PAGES`] pages.
//! * Each result has `times`: an exhibition has one span (its run); an
//!   event with several sessions lists the overall span first and then each
//!   session. [`occurrences`] keeps the sessions (dropping any time that
//!   contains another, and duplicates), and [`raw_events`] emits one
//!   [`RawEvent`] per session that has not ended yet.
//! * Times are UTC (`...Z`) and match the site's London wall-clock times
//!   (`2026-10-03T11:00:00.000Z` is shown as "Saturday 3 October 2026,
//!   12:00 – 13:30"). Exhibitions are date-only on the site ("26 March – 29
//!   November 2026"), so they become `all_day` spans of London dates.
//! * Category: `isExhibition` → exhibition (skipping permanent ones, format
//!   "Permanent exhibition" or ending in 2090); format Discussion/Talk and
//!   tours → talk; Workshop → workshop; Session/Relaxed opening/Late/
//!   Festival → community (a "tour" session is a talk); other formats by
//!   the shared keyword rules. Performances, screenings and online-only
//!   events are skipped.
//! * Links: `https://wellcomecollection.org/events/<uid>` (exhibitions:
//!   `/exhibitions/<uid>`). The list has no description; images are the
//!   Prismic `16:9` crop (the thumbnailer fetches them, never the pages).

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveTime, Utc};
use serde_json::{Value, json};
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, RawEvent};
use crate::normalise::{clean_text, dedupe_key, london_date, london_to_utc, parse_datetime};

pub const KEY: &str = "wellcome-collection";
pub const SITE: &str = "https://wellcomecollection.org";
const EVENTS_PATH: &str = "/content/v0/events";
pub const PAGE_SIZE: u32 = 100;
pub const MAX_PAGES: u64 = 3;
const VENUE: &str = "Wellcome Collection";
const ADDRESS: &str = "183 Euston Road, London NW1 2BE";
const LAT: f64 = 51.525_900;
const LNG: f64 = -0.133_960;
/// Permanent exhibitions end on 2090-01-01.
const OPEN_ENDED_YEAR: i32 = 2090;

pub struct WellcomeCollection {
    base_url: Url,
}

impl WellcomeCollection {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }

    fn page_url(&self, page: u64) -> Result<Url, SourceError> {
        let mut url = self
            .base_url
            .join(EVENTS_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("timespan", "future")
            .append_pair("sort", "times.startDateTime")
            .append_pair("sortOrder", "asc")
            .append_pair("pageSize", &PAGE_SIZE.to_string())
            .append_pair("page", &page.to_string());
        Ok(url)
    }
}

#[async_trait]
impl Source for WellcomeCollection {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let now = Utc::now();
        let mut out = Vec::new();
        let mut page = 1u64;
        loop {
            let body: Value = ctx.get_json(&self.page_url(page)?).await?;
            if body.get("results").and_then(Value::as_array).is_none() {
                return Err(SourceError::Parse(format!(
                    "page {page} has no results array"
                )));
            }
            let (raws, errors) = raw_events(&body, now);
            for e in errors {
                ctx.report_error(format!("wellcome-collection: {e}"));
            }
            out.extend(raws);
            let total_pages = body.get("totalPages").and_then(Value::as_u64).unwrap_or(1);
            if page >= total_pages || page >= MAX_PAGES {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_event(&raw.payload)
    }
}

fn s<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn is_exhibition(ev: &Value) -> bool {
    ev.get("isExhibition").and_then(Value::as_bool) == Some(true)
}

/// One session (or an exhibition's run): start, end and whether it's
/// fully booked in the building.
#[derive(Debug, Clone, PartialEq)]
pub struct Occurrence {
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
    pub fully_booked: bool,
}

/// The item's occurrences. An exhibition has one (its first time); for
/// other events a time that contains another one is the overall span of a
/// series and is dropped, and duplicate sessions are kept once. An end
/// before its start (a data slip) is dropped.
pub fn occurrences(ev: &Value) -> Vec<Occurrence> {
    let mut times: Vec<Occurrence> = Vec::new();
    for t in ev
        .get("times")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(start) = s(t, "/startDateTime").and_then(parse_datetime) else {
            continue;
        };
        let end = s(t, "/endDateTime")
            .and_then(parse_datetime)
            .filter(|e| *e >= start);
        let fully_booked =
            t.pointer("/isFullyBooked/inVenue").and_then(Value::as_bool) == Some(true);
        let o = Occurrence {
            start,
            end,
            fully_booked,
        };
        if !times.iter().any(|x| x.start == o.start && x.end == o.end) {
            times.push(o);
        }
    }
    if is_exhibition(ev) {
        times.truncate(1);
        return times;
    }
    let contains = |a: &Occurrence, b: &Occurrence| {
        a != b
            && a.start <= b.start
            && match (a.end, b.end) {
                (Some(ae), Some(be)) => ae >= be,
                (Some(ae), None) => ae >= b.start,
                (None, _) => false,
            }
    };
    times
        .iter()
        .filter(|a| !times.iter().any(|b| contains(a, b)))
        .cloned()
        .collect()
}

fn source_url(ev: &Value, uid: &str) -> String {
    let section = if is_exhibition(ev) {
        "exhibitions"
    } else {
        "events"
    };
    format!("{SITE}/{section}/{uid}")
}

/// Turn one API page into raw events: one per occurrence that has not
/// ended by `now`. The payload is the API item plus an `occurrence` object
/// (`start`, `end`, `fully_booked`). Items without an id or uid are
/// returned as error messages.
pub fn raw_events(page: &Value, now: DateTime<Utc>) -> (Vec<RawEvent>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for ev in page
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(id), Some(uid)) = (s(ev, "/id"), s(ev, "/uid")) else {
            errors.push(format!(
                "item without id/uid: {:?}",
                s(ev, "/title").unwrap_or("?")
            ));
            continue;
        };
        let exhibition = is_exhibition(ev);
        for o in occurrences(ev) {
            if o.end.unwrap_or(o.start) < now {
                continue;
            }
            let mut payload = ev.clone();
            payload["occurrence"] = json!({
                "start": o.start.to_rfc3339(),
                "end": o.end.map(|e| e.to_rfc3339()),
                "fully_booked": o.fully_booked,
            });
            let source_event_id = if exhibition {
                id.to_string()
            } else {
                format!("{id}-{}", o.start.format("%Y%m%dT%H%MZ"))
            };
            out.push(RawEvent {
                source_event_id,
                source_url: Some(source_url(ev, uid)),
                payload,
            });
        }
    }
    (out, errors)
}

/// The category for a non-exhibition item, from its format label (and the
/// title for sessions that are tours). `None` = out of scope.
fn event_category(format: &str, title: &str) -> Option<Category> {
    let is_tour = |t: &str| {
        crate::normalise::words(t)
            .iter()
            .any(|w| w == "tour" || w == "tours")
    };
    match format.to_lowercase().as_str() {
        "discussion" | "talk" | "lecture" | "in conversation" | "gallery tour" | "tour" => {
            Some(Category::Talk)
        }
        "workshop" => Some(Category::Workshop),
        "session" | "relaxed opening" | "late" | "festival" | "event" if is_tour(title) => {
            Some(Category::Talk)
        }
        "session" | "relaxed opening" | "late" | "festival" | "event" => Some(Category::Community),
        "performance" | "screening" | "film" | "concert" | "music" | "permanent exhibition" => None,
        // A format we haven't seen: the shared keyword rules decide.
        _ => crate::normalise::map_category(&[format, title]),
    }
}

fn london_midnight(t: DateTime<Utc>) -> DateTime<Utc> {
    london_to_utc(london_date(t).and_time(NaiveTime::MIN))
}

/// Normalise one raw event (an API item plus its `occurrence`).
pub fn normalise_event(ev: &Value) -> Result<Option<NewEvent>, SourceError> {
    let title = s(ev, "/title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("item without title".into()))?;
    let format = s(ev, "/format/label").unwrap_or("");

    // Online-only events are out of scope; "In our building" keeps an item.
    let in_building = ev
        .pointer("/locations/attendance")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|x| s(x, "/id") == Some("in-our-building")));
    let online = ev.pointer("/locations/isOnline").and_then(Value::as_bool) == Some(true);
    if online && !in_building {
        return Ok(None);
    }

    let start = s(ev, "/occurrence/start")
        .and_then(parse_datetime)
        .ok_or_else(|| SourceError::Parse(format!("no start for {title:?}")))?;
    let end = s(ev, "/occurrence/end").and_then(parse_datetime);
    let fully_booked = ev
        .pointer("/occurrence/fully_booked")
        .and_then(Value::as_bool)
        == Some(true);

    let exhibition = is_exhibition(ev);
    let (category, starts_at, ends_at, all_day) = if exhibition {
        if format.eq_ignore_ascii_case("permanent exhibition")
            || end.is_some_and(|e| e.year() >= OPEN_ENDED_YEAR)
        {
            return Ok(None);
        }
        // Exhibitions are date-only on the site: London dates, inclusive.
        let starts_at = london_midnight(start);
        let ends_at = end.map(london_midnight).filter(|e| *e > starts_at);
        (Category::Exhibition, starts_at, ends_at, true)
    } else {
        let Some(category) = event_category(format, &title) else {
            return Ok(None);
        };
        (category, start, end, false)
    };

    let mut tags: Vec<String> = Vec::new();
    let mut tag = |t: &str| {
        let t = clean_text(t).to_lowercase();
        if !t.is_empty() && !tags.contains(&t) {
            tags.push(t);
        }
    };
    tag(format);
    for list in ["/audiences", "/interpretations"] {
        for x in ev
            .pointer(list)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(l) = s(x, "/label") {
                tag(l);
            }
        }
    }
    if fully_booked {
        tag("fully booked");
    }

    let image_url = s(ev, "/image/16:9/url")
        .or_else(|| s(ev, "/image/url"))
        .map(str::to_string);
    let url = s(ev, "/uid").map(|uid| source_url(ev, uid));

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE)),
        title,
        description: None,
        venue_name: Some(VENUE.into()),
        address: Some(ADDRESS.into()),
        lat: Some(LAT),
        lng: Some(LNG),
        starts_at,
        ends_at,
        all_day,
        price: Default::default(),
        url,
        image_url,
        category,
        tags,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        parse_datetime(s).unwrap()
    }

    #[test]
    fn a_series_drops_its_overall_span_and_duplicates() {
        let ev = json!({"times": [
            {"startDateTime": "2026-07-04T10:30:00.000Z", "endDateTime": "2026-12-13T15:30:00.000Z"},
            {"startDateTime": "2026-07-04T10:30:00.000Z", "endDateTime": "2026-07-04T11:30:00.000Z"},
            {"startDateTime": "2026-10-25T14:30:00.000Z", "endDateTime": "2026-10-25T15:30:00.000Z",
             "isFullyBooked": {"inVenue": true, "online": false}},
            {"startDateTime": "2026-10-25T14:30:00.000Z", "endDateTime": "2026-10-25T15:30:00.000Z"},
        ]});
        let o = occurrences(&ev);
        assert_eq!(o.len(), 2, "{o:?}");
        assert_eq!(o[0].start, at("2026-07-04T10:30:00Z"));
        assert_eq!(o[1].end, Some(at("2026-10-25T15:30:00Z")));
        assert!(o[1].fully_booked);
    }

    #[test]
    fn an_end_before_the_start_is_dropped() {
        let ev = json!({"times": [
            {"startDateTime": "2026-10-24T14:30:00.000Z", "endDateTime": "2026-08-13T15:30:00.000Z"},
        ]});
        assert_eq!(occurrences(&ev)[0].end, None);
    }

    #[test]
    fn a_single_timed_event_is_kept_as_is() {
        let ev = json!({"times": [
            {"startDateTime": "2026-10-03T11:00:00.000Z", "endDateTime": "2026-10-03T12:30:00.000Z"},
        ]});
        assert_eq!(occurrences(&ev).len(), 1);
    }

    #[test]
    fn categories_follow_the_format() {
        assert_eq!(event_category("Discussion", "x"), Some(Category::Talk));
        assert_eq!(event_category("Gallery tour", "x"), Some(Category::Talk));
        assert_eq!(event_category("Workshop", "x"), Some(Category::Workshop));
        assert_eq!(
            event_category("Session", "Relaxed openings"),
            Some(Category::Community)
        );
        assert_eq!(
            event_category("Session", "Audio-described tours"),
            Some(Category::Talk)
        );
        assert_eq!(event_category("Performance", "x"), None);
        assert_eq!(event_category("Screening", "x"), None);
    }
}
