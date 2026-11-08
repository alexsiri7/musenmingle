//! Vortex Jazz Club (Gillett Square, Dalston, N16) — jazz and improvised
//! music from the site's iCal feed (issue #215).
//!
//! * robots.txt (checked 2026-11-08, saved as a fixture): the `User-agent: *`
//!   group disallows only `/wp-admin/`.
//! * The site runs The Events Calendar, whose REST API is off (404), but its
//!   iCal export of the upcoming list, `/events/?ical=1` (about 50 events,
//!   three months ahead), carries everything the listing does: one request a
//!   run, no detail pages. The event pages' JSON-LD has the same times with
//!   the same wrong offset, so it adds nothing.
//! * Times: the feed labels every time `TZID=UTC+0`, but they are London
//!   wall clock: the Vortex Jam Session of Sunday 6 September 2026 is
//!   `DTSTART;TZID=UTC+0:20260906T194500` (and `+00:00` in the page's
//!   JSON-LD) while its page prints "7.45PM". So a time without `Z` is read
//!   as London wall clock, whatever its `TZID`. `DTSTART` is the door time,
//!   which is what the page prints ("Sun 08 November 2026, 7PM" for "Doors
//!   7:00 PM, Music 7:30 PM"); `DTEND` the end of the last set, so a
//!   two-show night is one event spanning both. A date-only item (none seen
//!   so far) is `all_day`, its `DTEND` exclusive.
//! * Every event is at the club (the feed's `LOCATION` is always "11 Gillett
//!   Square, London, N16 8AZ", with `GEO`; "Downstairs" shows are in its
//!   basement). The feed gives no prices, so price is unknown.
//! * Category: every event is music (gigs, jam sessions, album launches, the
//!   NYE parties), tagged `jazz`, the club's own genre, plus `experimental`
//!   when the title or description names free improvisation, free jazz or
//!   the avant-garde ([`EXPERIMENTAL_PHRASES`]); `crate::music` adds the
//!   rest from titles. Nothing is skipped.
//! * Ids: the WordPress post id, the `UID`'s first part
//!   (`27584-1794164400-1794179700@www.vortexjazz.co.uk`); the link is the
//!   `URL`, which must be an `/event/` page on the site.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde_json::{Value, json};
use url::Url;

use super::ical::{first, text, unescape, unfold, vevents};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_description, clean_text, dedupe_key, london_to_utc, padded_words};

pub const KEY: &str = "vortex-jazz-club";
pub const FEED_PATH: &str = "/events/?ical=1";
const EVENT_PREFIX: &str = "/event/";
const VENUE_NAME: &str = "Vortex Jazz Club";
const VENUE_ADDRESS: &str = "11 Gillett Square, Dalston, London N16 8AZ";
/// Phrases (as [`padded_words`]) in a title or description that make a gig
/// `experimental`. Not "experimental" itself: the descriptions use it for
/// players' other work ("experimental performance" in a dancer's bio).
const EXPERIMENTAL_PHRASES: &[&str] = &[
    "free improvisation",
    "free improvised",
    "free improv",
    "improvised music",
    "free jazz",
    "avant garde",
];

pub struct VortexJazzClub {
    base_url: Url,
}

impl VortexJazzClub {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// Parse the iCal feed fetched from `feed_url` into one [`RawEvent`] per
/// VEVENT, in feed order. A VEVENT without a post id, a start or an
/// `/event/` link on the feed's host is an error.
pub fn parse_feed(
    feed: &str,
    feed_url: &Url,
) -> Result<Vec<Result<RawEvent, SourceError>>, SourceError> {
    let events = vevents(&unfold(feed.as_bytes()))?;
    Ok(events
        .iter()
        .map(|event| {
            let title = text(event, "SUMMARY");
            let bad = |what: &str| SourceError::Parse(format!("{title:?}: {what}"));
            let id = text(event, "UID")
                .and_then(|uid| uid.split(['-', '@']).next().map(str::to_string))
                .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
                .ok_or_else(|| bad("no post id in the UID"))?;
            let url = text(event, "URL")
                .and_then(|u| feed_url.join(&u).ok())
                .filter(|u| {
                    u.host_str() == feed_url.host_str()
                        && u.path()
                            .strip_prefix(EVENT_PREFIX)
                            .is_some_and(|slug| !slug.trim_matches('/').is_empty())
                })
                .ok_or_else(|| bad("no /event/ link"))?;
            let dtstart = first(event, "DTSTART")
                .map(|p| p.value.trim().to_string())
                .ok_or_else(|| bad("no DTSTART"))?;
            let geo = first(event, "GEO").and_then(|p| {
                let (lat, lng) = p.value.split_once(';')?;
                Some(json!([
                    lat.trim().parse::<f64>().ok()?,
                    lng.trim().parse::<f64>().ok()?
                ]))
            });
            Ok(RawEvent {
                source_event_id: id,
                source_url: Some(url.to_string()),
                payload: json!({
                    "url": url.as_str(),
                    "title": title,
                    "description": first(event, "DESCRIPTION").map(|p| unescape(&p.value)),
                    "dtstart": dtstart,
                    "dtend": first(event, "DTEND").map(|p| p.value.trim().to_string()),
                    "geo": geo,
                    "image_url": first(event, "ATTACH").map(|p| p.value.trim().to_string()),
                }),
            })
        })
        .collect())
}

/// A `DTSTART`/`DTEND` value: a date, or a time read as London wall clock
/// unless it is UTC (`…Z`); see the module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FeedTime {
    Date(NaiveDate),
    At(DateTime<Utc>),
}

fn parse_feed_time(value: &str) -> Option<FeedTime> {
    if value.len() == 8 {
        return NaiveDate::parse_from_str(value, "%Y%m%d")
            .ok()
            .map(FeedTime::Date);
    }
    let (value, utc) = match value.strip_suffix('Z') {
        Some(v) => (v, true),
        None => (value, false),
    };
    let t = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    Some(FeedTime::At(if utc {
        t.and_utc()
    } else {
        london_to_utc(t)
    }))
}

/// Whether a gig's title or description names experimental music.
pub fn is_experimental(title: &str, description: &str) -> bool {
    let hay = format!("{}{}", padded_words(title), padded_words(description));
    EXPERIMENTAL_PHRASES
        .iter()
        .any(|p| hay.contains(&format!(" {p} ")))
}

/// Normalise a Vortex Jazz Club [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let s = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = s("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event without a title".into()))?;
    let bad = |k: &str| SourceError::Parse(format!("{title:?}: bad {k} {:?}", payload.get(k)));
    let start = s("dtstart")
        .and_then(parse_feed_time)
        .ok_or_else(|| bad("dtstart"))?;
    let end = s("dtend").and_then(parse_feed_time);
    let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
    let (starts_at, ends_at, all_day) = match (start, end) {
        (FeedTime::Date(first_day), end) => {
            // DTEND of a date is exclusive: the last day is the one before.
            let last_day = match end {
                Some(FeedTime::Date(d)) => d.pred_opt().filter(|d| *d > first_day),
                _ => None,
            };
            (midnight(first_day), last_day.map(midnight), true)
        }
        (FeedTime::At(starts_at), end) => {
            let ends_at = match end {
                Some(FeedTime::At(e)) => Some(e).filter(|e| *e > starts_at),
                _ => None,
            };
            (starts_at, ends_at, false)
        }
    };
    let description = s("description").unwrap_or_default();
    let mut tags = vec!["jazz".to_string()];
    if is_experimental(&title, description) {
        tags.push("experimental".to_string());
    }
    let geo = payload
        .get("geo")
        .and_then(Value::as_array)
        .and_then(|g| Some((g.first()?.as_f64()?, g.get(1)?.as_f64()?)));

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE_NAME)),
        description: clean_description(Some(description)),
        title,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: geo.map(|g| g.0),
        lng: geo.map(|g| g.1),
        starts_at,
        ends_at,
        all_day,
        price: Price::default(),
        url: s("url").map(str::to_string),
        image_url: s("image_url").map(str::to_string),
        category: Category::Music,
        tags,
    }))
}

#[async_trait]
impl Source for VortexJazzClub {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(FEED_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let items = parse_feed(&ctx.get_text(&url).await?, &url)?;
        if items.is_empty() {
            return Err(SourceError::Parse("no events in the iCal feed".into()));
        }
        let mut out = Vec::new();
        for item in items {
            match item {
                Ok(raw) => out.push(raw),
                Err(e) => ctx.report_error(e.to_string()),
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

    fn payload(dtstart: &str, dtend: Option<&str>) -> Value {
        json!({
            "url": "https://www.vortexjazz.co.uk/event/x/",
            "title": "Vortex Jam Session",
            "dtstart": dtstart,
            "dtend": dtend,
        })
    }

    fn rfc3339(t: Option<DateTime<Utc>>) -> Option<String> {
        t.map(|t| t.to_rfc3339())
    }

    #[test]
    fn times_are_london_wall_clock_despite_the_utc_label() {
        // Printed "Sun 06 September 2026, 7.45PM" (BST).
        let bst = normalise_payload(&payload("20260906T194500", Some("20260906T230000")))
            .unwrap()
            .unwrap();
        assert_eq!(bst.starts_at.to_rfc3339(), "2026-09-06T18:45:00+00:00");
        assert_eq!(
            rfc3339(bst.ends_at).as_deref(),
            Some("2026-09-06T22:00:00+00:00")
        );
        assert!(!bst.all_day);
        let gmt = normalise_payload(&payload("20261108T190000", None))
            .unwrap()
            .unwrap();
        assert_eq!(gmt.starts_at.to_rfc3339(), "2026-11-08T19:00:00+00:00");
        assert!(gmt.ends_at.is_none());
        let utc = normalise_payload(&payload("20260906T184500Z", None))
            .unwrap()
            .unwrap();
        assert_eq!(utc.starts_at, bst.starts_at);
    }

    #[test]
    fn date_only_items_are_all_day_with_an_exclusive_end() {
        let one = normalise_payload(&payload("20261224", Some("20261225")))
            .unwrap()
            .unwrap();
        assert!(one.all_day);
        assert_eq!(one.starts_at.to_rfc3339(), "2026-12-24T00:00:00+00:00");
        assert!(one.ends_at.is_none());
        let two = normalise_payload(&payload("20260830", Some("20260901")))
            .unwrap()
            .unwrap();
        assert_eq!(two.starts_at.to_rfc3339(), "2026-08-29T23:00:00+00:00");
        assert_eq!(
            rfc3339(two.ends_at).as_deref(),
            Some("2026-08-30T23:00:00+00:00")
        );
    }

    #[test]
    fn a_bad_start_is_a_parse_error() {
        let err = normalise_payload(&payload("soon", None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("bad dtstart"), "{err}");
    }

    #[test]
    fn experimental_from_the_venue_words() {
        assert!(is_experimental(
            "MOPOMOSO: International Free Improvised Music",
            ""
        ));
        assert!(is_experimental("Trio", "legendary free jazz repertoire"));
        assert!(is_experimental("Trio", "an avant-garde set"));
        assert!(!is_experimental("Vortex Jam Session", "Bring your horn"));
        assert!(!is_experimental("Trio", "freely improvising"));
        assert!(!is_experimental(
            "Duo",
            "her work spans experimental performance"
        ));
    }

    #[test]
    fn a_vevent_without_an_event_link_or_post_id_is_an_error() {
        let feed_url = Url::parse("https://www.vortexjazz.co.uk/events/?ical=1").unwrap();
        let feed = "BEGIN:VCALENDAR\r\n\
            BEGIN:VEVENT\r\nUID:1-2-3@x\r\nSUMMARY:Off site\r\nDTSTART:20261108T190000\r\n\
            URL:https://elsewhere.example/event/a/\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:abc@x\r\nSUMMARY:No id\r\nDTSTART:20261108T190000\r\n\
            URL:https://www.vortexjazz.co.uk/event/b/\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:7-2-3@x\r\nSUMMARY:Gig\\, live\r\nDTSTART:20261108T190000\r\n\
            URL:https://www.vortexjazz.co.uk/event/c/\r\nEND:VEVENT\r\n\
            END:VCALENDAR\r\n";
        let items = parse_feed(feed, &feed_url).unwrap();
        assert!(items[0].as_ref().unwrap_err().to_string().contains("link"));
        assert!(
            items[1]
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("post id")
        );
        let gig = items[2].as_ref().unwrap();
        assert_eq!(gig.source_event_id, "7");
        assert_eq!(gig.payload["title"], "Gig, live");
    }

    #[test]
    fn not_a_feed_is_a_parse_error() {
        let feed_url = Url::parse("https://www.vortexjazz.co.uk/events/?ical=1").unwrap();
        assert!(parse_feed("<html></html>", &feed_url).is_err());
    }
}
