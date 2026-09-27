//! Luma calendars — one platform source for the London creative
//! communities that publish their events on Luma (luma.com). Each calendar
//! is an `events.sources` row with `platform = 'luma'` and its settings in
//! `config` ([`LumaConfig`]), so adding a calendar is a migration row, not
//! code. The chosen calendars and why are listed in `docs/luma-calendars.md`.
//!
//! * Luma's terms (https://luma.com/terms, checked 2026-09-27) forbid
//!   reproducing site content "except when such actions occur in connection
//!   with bona fide uses of the Service through our publicly supported
//!   interfaces", and accessing the Service by any other means. So we never
//!   read luma.com pages: each calendar is read only through its official
//!   "Subscribe via iCal" feed,
//!   `GET {base_url}/ics/get?entity=calendar&id=<calendar_id>` (one request
//!   per calendar per run). The rows are facts + link only
//!   (`store_description = FALSE, store_image = FALSE`); the feed carries no
//!   images and its DESCRIPTION is only a link, the address and the hosts,
//!   so the source emits no description.
//! * The feed is read as bytes (capped at [`MAX_FEED_BYTES`]), unfolded
//!   (RFC 5545 §3.1, before decoding, so a fold inside a multi-byte
//!   character can't corrupt it) and parsed here: no iCalendar crate.
//! * Times: Luma writes `DTSTART`/`DTEND` in UTC (`…Z`); `TZID=` and
//!   floating times are read as that zone's (or London's) wall clock, and
//!   `VALUE=DATE` items are stored `all_day` (DTEND is exclusive, so the last
//!   day is the day before it). `DURATION` stands in for a missing `DTEND`.
//! * The feed carries a calendar's whole history. Only occurrences that
//!   haven't ended by "now" are kept. A `RRULE` (none seen in Luma's feeds
//!   so far, but iCalendar allows it) is expanded to the occurrences starting
//!   within [`RECURRENCE_WINDOW_DAYS`] days, at most [`MAX_OCCURRENCES`]:
//!   `FREQ=DAILY|WEEKLY` with `INTERVAL`, `COUNT`, `UNTIL`, weekly `BYDAY`,
//!   and `EXDATE`. Any other rule keeps only the first occurrence (not an
//!   error). `STATUS:CANCELLED` events are dropped.
//! * Ids: the `evt-…` part of the `UID`, plus `#<start>` for an occurrence of
//!   a recurring event. The link is the event page named in the DESCRIPTION
//!   ("Get up-to-date information at: https://luma.com/<slug>"), else
//!   `https://luma.com/event/<evt-id>`.
//! * Location: `LOCATION` is "<venue>, <street>, London <postcode>, UK" and
//!   `GEO` the coordinates. Only London events are kept: `GEO` inside
//!   Greater London, or without `GEO` a location mentioning London. When a
//!   host hides the address until you register, `LOCATION` is a URL and
//!   `GEO` is an approximate point: the event is kept if that point is in
//!   London, but with no venue, address or coordinates, so we never publish
//!   a guessed pin. A URL location without `GEO` is online or unknown and is
//!   skipped. A location starting with a house number has no venue name;
//!   it is kept as the address.
//! * Category: a title containing one of the row's `skip_keywords` is
//!   skipped (screenings, listening parties, …); otherwise `map_category`
//!   over the title, then the row's `default_category`; with neither the
//!   event is skipped. Luma gives no price, so price is unknown.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{
    DateTime, Datelike, Days, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc, Weekday,
};
use chrono_tz::Tz;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::{SkipReason, Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{clean_text, dedupe_key, in_london_bbox, london_to_utc, map_category};

/// `events.sources.platform` of Luma calendars.
pub const PLATFORM: &str = "luma";
/// Largest feed read (the busiest calendar seen, 239 events, is ~100 kB).
pub const MAX_FEED_BYTES: usize = 5 * 1024 * 1024;
/// Recurring events are expanded this many days ahead.
pub const RECURRENCE_WINDOW_DAYS: u64 = 90;
/// Upper bound on the occurrences taken from one recurring event.
pub const MAX_OCCURRENCES: usize = 60;
const FEED_PATH: &str = "/ics/get";
const EVENT_PAGE_BASE: &str = "https://luma.com/event/";

/// A calendar's `events.sources.config`. Unknown fields are rejected, so a
/// typo in a seed row is a recorded skip rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LumaConfig {
    /// The calendar's id (`cal-…`), from its "Add iCal subscription" link.
    pub calendar_id: String,
    /// For calendars whose events are all one kind.
    #[serde(default)]
    pub default_category: Option<Category>,
    /// Words or phrases whose presence in a title skips the event.
    #[serde(default)]
    pub skip_keywords: Vec<String>,
}

impl LumaConfig {
    pub fn from_json(config: Option<&Value>) -> Result<Self, SkipReason> {
        let config = config.ok_or_else(|| SkipReason::InvalidConfig("config is not set".into()))?;
        let config: LumaConfig = serde_json::from_value(config.clone())
            .map_err(|e| SkipReason::InvalidConfig(e.to_string()))?;
        let valid_id = config
            .calendar_id
            .strip_prefix("cal-")
            .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()));
        if !valid_id {
            return Err(SkipReason::InvalidConfig(format!(
                "calendar_id {:?} is not a cal-… id",
                config.calendar_id
            )));
        }
        Ok(config)
    }
}

pub struct Luma {
    key: String,
    base_url: Url,
    config: LumaConfig,
    now: Option<DateTime<Utc>>,
}

impl Luma {
    pub fn from_row(key: &str, base_url: Url, config: Option<&Value>) -> Result<Self, SkipReason> {
        Ok(Self {
            key: key.to_string(),
            base_url,
            config: LumaConfig::from_json(config)?,
            now: None,
        })
    }

    /// Fix "now" (tests): which occurrences are upcoming.
    pub fn with_now(mut self, now: DateTime<Utc>) -> Self {
        self.now = Some(now);
        self
    }

    pub fn feed_url(&self) -> Result<Url, SourceError> {
        let mut url = self
            .base_url
            .join(FEED_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("entity", "calendar")
            .append_pair("id", &self.config.calendar_id);
        Ok(url)
    }
}

// ---------------------------------------------------------------------------
// iCalendar reading

/// One content line: `NAME;PARAM=V;…:VALUE`.
#[derive(Debug, Clone)]
struct Property {
    params: BTreeMap<String, String>,
    value: String,
}

type Component = BTreeMap<String, Vec<Property>>;

/// Undo RFC 5545 line folding (a line break followed by a space or tab) on
/// the raw bytes, then decode.
fn unfold(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let fold = match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => 2,
            b'\n' => 1,
            _ => 0,
        };
        if fold > 0 && matches!(bytes.get(i + fold), Some(b' ' | b'\t')) {
            i += fold + 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_line(line: &str) -> Option<(String, Property)> {
    // The value starts after the first ':' outside a quoted parameter.
    let mut in_quotes = false;
    let colon = line.char_indices().find_map(|(i, c)| match c {
        '"' => {
            in_quotes = !in_quotes;
            None
        }
        ':' if !in_quotes => Some(i),
        _ => None,
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| {
            (
                k.trim().to_ascii_uppercase(),
                v.trim_matches('"').to_string(),
            )
        })
        .collect();
    Some((
        name,
        Property {
            params,
            value: value.to_string(),
        },
    ))
}

/// The VEVENTs of a feed (nested components such as VALARM are ignored).
fn vevents(text: &str) -> Result<Vec<Component>, SourceError> {
    if !text.trim_start().starts_with("BEGIN:VCALENDAR") {
        return Err(SourceError::Parse("not an iCalendar feed".into()));
    }
    let mut events = Vec::new();
    let mut current: Option<Component> = None;
    let mut nested = 0usize;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        match (line, current.as_mut()) {
            ("BEGIN:VEVENT", None) => current = Some(Component::new()),
            ("END:VEVENT", Some(_)) if nested == 0 => events.extend(current.take()),
            (l, Some(_)) if l.starts_with("BEGIN:") => nested += 1,
            (l, Some(_)) if l.starts_with("END:") => nested = nested.saturating_sub(1),
            (l, Some(event)) if nested == 0 => {
                if let Some((name, prop)) = parse_line(l) {
                    event.entry(name).or_default().push(prop);
                }
            }
            _ => {}
        }
    }
    Ok(events)
}

/// Unescape a TEXT value (RFC 5545 §3.3.11).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn first<'a>(event: &'a Component, name: &str) -> Option<&'a Property> {
    event.get(name).and_then(|v| v.first())
}

fn text(event: &Component, name: &str) -> Option<String> {
    first(event, name)
        .map(|p| clean_text(&unescape(&p.value)))
        .filter(|t| !t.is_empty())
}

/// A DTSTART/DTEND/EXDATE value.
#[derive(Debug, Clone, Copy, PartialEq)]
enum IcsTime {
    /// `VALUE=DATE`.
    Date(NaiveDate),
    /// Wall-clock time in a zone (`TZID=`; floating times use London), or
    /// UTC (`…Z`, `Tz::UTC`).
    Local(NaiveDateTime, Tz),
}

impl IcsTime {
    fn utc(self) -> DateTime<Utc> {
        match self {
            IcsTime::Date(d) => london_to_utc(d.and_time(NaiveTime::MIN)),
            IcsTime::Local(t, tz) => local_to_utc(t, tz),
        }
    }

    fn shifted(self, days: u64) -> Option<IcsTime> {
        Some(match self {
            IcsTime::Date(d) => IcsTime::Date(d.checked_add_days(Days::new(days))?),
            IcsTime::Local(t, tz) => IcsTime::Local(t.checked_add_days(Days::new(days))?, tz),
        })
    }

    fn date(self) -> NaiveDate {
        match self {
            IcsTime::Date(d) => d,
            IcsTime::Local(t, _) => t.date(),
        }
    }
}

fn local_to_utc(t: NaiveDateTime, tz: Tz) -> DateTime<Utc> {
    use chrono::TimeZone;
    if tz == chrono_tz::Europe::London {
        return london_to_utc(t);
    }
    tz.from_local_datetime(&t)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(t + Duration::hours(1))).earliest())
        .map_or_else(|| t.and_utc(), |t| t.with_timezone(&Utc))
}

fn parse_time_value(value: &str, params: &BTreeMap<String, String>) -> Option<IcsTime> {
    let value = value.trim();
    if params
        .get("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("DATE"))
        || value.len() == 8
    {
        return NaiveDate::parse_from_str(value, "%Y%m%d")
            .ok()
            .map(IcsTime::Date);
    }
    let (value, utc) = match value.strip_suffix('Z') {
        Some(v) => (v, true),
        None => (value, false),
    };
    let t = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    let tz = if utc {
        Tz::UTC
    } else {
        params
            .get("TZID")
            .and_then(|z| z.parse::<Tz>().ok())
            .unwrap_or(chrono_tz::Europe::London)
    };
    Some(IcsTime::Local(t, tz))
}

fn time_prop(event: &Component, name: &str) -> Option<IcsTime> {
    first(event, name).and_then(|p| parse_time_value(&p.value, &p.params))
}

/// `DURATION` (`P1D`, `PT2H30M`, `P1W`).
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim().strip_prefix('P')?;
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    let mut total = Duration::zero();
    let mut take = |part: &str, units: &[(char, i64)]| -> Option<()> {
        let mut n = String::new();
        for c in part.chars() {
            if c.is_ascii_digit() {
                n.push(c);
            } else {
                let secs = units.iter().find(|(u, _)| *u == c)?.1;
                total += Duration::seconds(n.parse::<i64>().ok()? * secs);
                n.clear();
            }
        }
        n.is_empty().then_some(())
    };
    take(date, &[('W', 604_800), ('D', 86_400)])?;
    take(time, &[('H', 3_600), ('M', 60), ('S', 1)])?;
    Some(total)
}

/// The occurrence starts of an event within `[now, horizon]` (plus the
/// first one, which the caller filters by end time).
fn occurrences(
    event: &Component,
    start: IcsTime,
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
) -> (Vec<IcsTime>, bool) {
    let Some(rule) = first(event, "RRULE").map(|p| p.value.clone()) else {
        return (vec![start], false);
    };
    let parts: BTreeMap<String, String> = rule
        .split(';')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_ascii_uppercase(), v.to_ascii_uppercase()))
        .collect();
    let supported = parts.keys().all(|k| {
        matches!(
            k.as_str(),
            "FREQ" | "INTERVAL" | "COUNT" | "UNTIL" | "BYDAY" | "WKST"
        )
    });
    let step_days = match parts.get("FREQ").map(String::as_str) {
        Some("DAILY") if !parts.contains_key("BYDAY") => 1,
        Some("WEEKLY") => 7,
        _ => 0,
    };
    let interval: u64 = parts
        .get("INTERVAL")
        .and_then(|i| i.parse().ok())
        .filter(|i| *i > 0)
        .unwrap_or(1);
    if !supported || step_days == 0 {
        tracing::debug!(rule = %rule, "unsupported RRULE: first occurrence only");
        return (vec![start], false);
    }
    let count: Option<usize> = parts.get("COUNT").and_then(|c| c.parse().ok());
    let until = parts
        .get("UNTIL")
        .and_then(|u| parse_time_value(u, &BTreeMap::new()))
        .map(|u| match u {
            // A date UNTIL includes that whole day.
            IcsTime::Date(d) => london_to_utc(d.and_time(NaiveTime::MIN)) + Duration::days(1),
            other => other.utc(),
        });
    let weekdays: Vec<Weekday> = match parts.get("BYDAY") {
        Some(days) => days
            .split(',')
            .filter_map(|d| match d.trim() {
                "MO" => Some(Weekday::Mon),
                "TU" => Some(Weekday::Tue),
                "WE" => Some(Weekday::Wed),
                "TH" => Some(Weekday::Thu),
                "FR" => Some(Weekday::Fri),
                "SA" => Some(Weekday::Sat),
                "SU" => Some(Weekday::Sun),
                _ => None,
            })
            .collect(),
        None => vec![start.date().weekday()],
    };
    if weekdays.is_empty() {
        return (vec![start], false);
    }
    let exdates: Vec<DateTime<Utc>> = event
        .get("EXDATE")
        .into_iter()
        .flatten()
        .flat_map(|p| {
            p.value
                .split(',')
                .filter_map(|v| parse_time_value(v, &p.params))
                .map(IcsTime::utc)
                .collect::<Vec<_>>()
        })
        .collect();

    // Walk day by day through the periods (a period is `interval` days or
    // weeks, starting at DTSTART's day, or its week's Monday for WEEKLY).
    let period_days = step_days * interval;
    let week_start = start.date().weekday().num_days_from_monday() as u64;
    let mut out = Vec::new();
    let mut seen = 0usize;
    let mut offset: u64 = 0;
    // Ends at the horizon (or UNTIL/COUNT/the occurrence cap) at the latest.
    while let Some(candidate) = start.shifted(offset) {
        let at = candidate.utc();
        if until.is_some_and(|u| at > u) || at > horizon || count.is_some_and(|c| seen >= c) {
            break;
        }
        let in_period = if step_days == 7 {
            let since_monday = offset + week_start;
            (since_monday / 7) % interval == 0 && weekdays.contains(&candidate.date().weekday())
        } else {
            offset % period_days == 0
        };
        if in_period {
            seen += 1;
            if at >= now - Duration::days(1) && !exdates.contains(&at) {
                out.push(candidate);
                if out.len() >= MAX_OCCURRENCES {
                    break;
                }
            }
        }
        offset += 1;
    }
    (out, true)
}

/// The event page named in the DESCRIPTION, if any.
fn page_url(description: &str) -> Option<String> {
    description
        .split(|c: char| c.is_whitespace())
        .map(|w| w.trim_end_matches(['.', ',', ')']))
        .find(|w| w.starts_with("https://luma.com/") || w.starts_with("https://lu.ma/"))
        .filter(|w| Url::parse(w).is_ok())
        .map(str::to_string)
}

fn time_json(t: IcsTime) -> Value {
    match t {
        IcsTime::Date(d) => json!(d.format("%Y-%m-%d").to_string()),
        IcsTime::Local(..) => json!(t.utc().to_rfc3339()),
    }
}

/// Parse a calendar's iCal feed into one [`RawEvent`] per upcoming
/// occurrence (see the module docs). `now` decides what is upcoming.
pub fn parse_feed(bytes: &[u8], now: DateTime<Utc>) -> Result<Vec<RawEvent>, SourceError> {
    let horizon = now + Duration::days(RECURRENCE_WINDOW_DAYS as i64);
    let mut out: Vec<RawEvent> = Vec::new();
    for event in vevents(&unfold(bytes))? {
        if text(&event, "STATUS").is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED")) {
            continue;
        }
        let Some(uid) = text(&event, "UID") else {
            continue;
        };
        let id = uid.split('@').next().unwrap_or(&uid).to_string();
        let Some(start) = time_prop(&event, "DTSTART") else {
            continue;
        };
        let length = match (time_prop(&event, "DTEND"), start) {
            (Some(end), _) => end.utc() - start.utc(),
            (None, _) => first(&event, "DURATION")
                .and_then(|p| parse_duration(&p.value))
                .unwrap_or_else(|| match start {
                    IcsTime::Date(_) => Duration::days(1),
                    IcsTime::Local(..) => Duration::zero(),
                }),
        };
        let description = first(&event, "DESCRIPTION")
            .map(|p| unescape(&p.value))
            .unwrap_or_default();
        let url = page_url(&description).unwrap_or_else(|| format!("{EVENT_PAGE_BASE}{id}"));
        let geo = first(&event, "GEO").and_then(|p| {
            let (lat, lng) = p.value.split_once([';', ','])?;
            Some(json!([
                lat.trim().parse::<f64>().ok()?,
                lng.trim().parse::<f64>().ok()?
            ]))
        });
        let (starts, recurring) = occurrences(&event, start, now, horizon);
        for occurrence in starts {
            let ends = match occurrence {
                // DTEND of a date is exclusive: the last day is the one before.
                IcsTime::Date(d) => {
                    let days = length.num_days().max(1) as u64;
                    IcsTime::Date(d.checked_add_days(Days::new(days - 1)).unwrap_or(d))
                }
                IcsTime::Local(t, tz) => IcsTime::Local(t + length.max(Duration::zero()), tz),
            };
            let ended = match ends {
                IcsTime::Date(d) => london_to_utc(d.and_time(NaiveTime::MIN)) + Duration::days(1),
                other => other.utc().max(occurrence.utc()),
            };
            if ended <= now {
                continue;
            }
            let source_event_id = if recurring {
                format!("{id}#{}", occurrence.utc().format("%Y%m%dT%H%M%SZ"))
            } else {
                id.clone()
            };
            if out.iter().any(|r| r.source_event_id == source_event_id) {
                continue;
            }
            out.push(RawEvent {
                source_event_id,
                source_url: Some(url.clone()),
                payload: json!({
                    "uid": id,
                    "title": text(&event, "SUMMARY"),
                    "url": url,
                    "location": text(&event, "LOCATION"),
                    "geo": geo,
                    "all_day": matches!(occurrence, IcsTime::Date(_)),
                    "start": time_json(occurrence),
                    "end": time_json(ends),
                }),
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Normalising

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn contains_keyword(title: &str, keyword: &str) -> bool {
    let (title, keyword) = (words(title), words(keyword));
    !keyword.is_empty()
        && title
            .windows(keyword.len())
            .any(|w| w == keyword.as_slice())
}

/// Normalise a Luma [`RawEvent`] payload for a calendar.
pub fn normalise_payload(
    payload: &Value,
    config: &LumaConfig,
) -> Result<Option<NewEvent>, SourceError> {
    let s = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = s("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event without a title".into()))?;
    if config
        .skip_keywords
        .iter()
        .any(|k| contains_keyword(&title, k))
    {
        return Ok(None);
    }
    let Some(category) = map_category(&[title.as_str()]).or(config.default_category) else {
        return Ok(None);
    };

    // Where: see the module docs.
    let geo = payload
        .get("geo")
        .and_then(Value::as_array)
        .and_then(|g| Some((g.first()?.as_f64()?, g.get(1)?.as_f64()?)));
    let location = s("location").map(clean_text).filter(|l| !l.is_empty());
    let hidden = location
        .as_deref()
        .is_none_or(|l| l.starts_with("http://") || l.starts_with("https://"));
    let in_london = match geo {
        Some((lat, lng)) => in_london_bbox(lat, lng),
        None => {
            !hidden
                && location
                    .as_deref()
                    .is_some_and(|l| words(l).contains(&"london".into()))
        }
    };
    if !in_london {
        return Ok(None);
    }
    let (venue_name, address, lat, lng) = match (&location, hidden) {
        (Some(location), false) => {
            let (head, rest) = location.split_once(", ").unwrap_or((location, ""));
            let numbered = head.starts_with(|c: char| c.is_ascii_digit());
            let (venue, address) = if numbered || rest.is_empty() {
                (None, location.clone())
            } else {
                (Some(head.to_string()), rest.to_string())
            };
            (venue, Some(address), geo.map(|g| g.0), geo.map(|g| g.1))
        }
        _ => (None, None, None, None),
    };

    // When.
    let bad = |k: &str| SourceError::Parse(format!("{title:?}: bad {k} {:?}", payload.get(k)));
    let all_day = payload.get("all_day").and_then(Value::as_bool) == Some(true);
    let (starts_at, ends_at) = if all_day {
        let day = |k: &str| {
            s(k).and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .ok_or_else(|| bad(k))
        };
        let (first_day, last_day) = (day("start")?, day("end")?);
        let midnight = |d: NaiveDate| london_to_utc(d.and_time(NaiveTime::MIN));
        (
            midnight(first_day),
            (last_day > first_day).then(|| midnight(last_day)),
        )
    } else {
        let at = |k: &str| {
            s(k).and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc))
                .ok_or_else(|| bad(k))
        };
        let starts_at = at("start")?;
        (starts_at, at("end").ok().filter(|e| *e > starts_at))
    };

    Ok(Some(NewEvent {
        dedupe_key: dedupe_key(&title, starts_at, venue_name.as_deref()),
        title,
        description: None,
        venue_name,
        address,
        lat,
        lng,
        starts_at,
        ends_at,
        all_day,
        price: Price::default(),
        url: s("url").map(str::to_string),
        image_url: None,
        category,
        tags: Vec::new(),
    }))
}

#[async_trait]
impl Source for Luma {
    fn key(&self) -> &str {
        &self.key
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self.feed_url()?;
        let feed = ctx.get_bytes_limited(&url, MAX_FEED_BYTES).await?;
        parse_feed(&feed.bytes, self.now.unwrap_or_else(Utc::now))
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload, &self.config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-27T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn feed(events: &str) -> Vec<RawEvent> {
        let ics = format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{events}END:VCALENDAR\r\n");
        parse_feed(ics.as_bytes(), now()).unwrap()
    }

    fn config(v: Value) -> LumaConfig {
        LumaConfig::from_json(Some(&v)).unwrap()
    }

    #[test]
    fn config_needs_a_calendar_id() {
        assert!(LumaConfig::from_json(None).is_err());
        assert!(LumaConfig::from_json(Some(&json!({}))).is_err());
        assert!(LumaConfig::from_json(Some(&json!({"calendar_id": "nml"}))).is_err());
        assert!(LumaConfig::from_json(Some(&json!({"calendar_id": "cal-a/../b"}))).is_err());
        assert!(
            LumaConfig::from_json(Some(&json!({"calendar_id": "cal-abc", "venue": "x"}))).is_err()
        );
        let c = config(json!({"calendar_id": "cal-abc", "default_category": "talk"}));
        assert_eq!(c.default_category, Some(Category::Talk));
    }

    #[test]
    fn unfolds_before_decoding_and_unescapes() {
        // "é" (C3 A9) folded between its two bytes.
        let bytes = b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:evt-1@events.lu.ma\nSUMMARY:Caf\xC3\n \xA9 talk\\, part 1\\; more\nDTSTART:20261001T170000Z\nEND:VEVENT\nEND:VCALENDAR\n";
        let raws = parse_feed(bytes, now()).unwrap();
        assert_eq!(raws[0].payload["title"], "Café talk, part 1; more");
        assert_eq!(raws[0].source_event_id, "evt-1");
        assert_eq!(
            raws[0].source_url.as_deref(),
            Some("https://luma.com/event/evt-1")
        );
    }

    #[test]
    fn not_a_feed_is_a_parse_error() {
        assert!(parse_feed(b"<html></html>", now()).is_err());
    }

    #[test]
    fn past_and_cancelled_events_are_dropped() {
        let raws = feed(
            "BEGIN:VEVENT\nUID:evt-past\nSUMMARY:Old\nDTSTART:20260901T170000Z\nDTEND:20260901T190000Z\nEND:VEVENT\n\
             BEGIN:VEVENT\nUID:evt-off\nSUMMARY:Off\nSTATUS:CANCELLED\nDTSTART:20261001T170000Z\nEND:VEVENT\n\
             BEGIN:VEVENT\nUID:evt-now\nSUMMARY:Running\nDTSTART:20260927T070000Z\nDTEND:20260927T090000Z\nEND:VEVENT\n",
        );
        let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
        assert_eq!(ids, ["evt-now"]);
    }

    #[test]
    fn weekly_rule_with_exdate_count_and_tzid() {
        let raws = feed(
            "BEGIN:VEVENT\nUID:evt-w\nSUMMARY:Sketch club\n\
             DTSTART;TZID=Europe/London:20260923T190000\nDTEND;TZID=Europe/London:20260923T210000\n\
             RRULE:FREQ=WEEKLY;COUNT=6\nEXDATE;TZID=Europe/London:20261007T190000\nEND:VEVENT\n",
        );
        let starts: Vec<_> = raws.iter().map(|r| r.payload["start"].clone()).collect();
        // 23 Sep (past), 30 Sep, [7 Oct excluded], 14, 21 Oct (BST), 28 Oct (GMT).
        assert_eq!(
            starts,
            [
                json!("2026-09-30T18:00:00+00:00"),
                json!("2026-10-14T18:00:00+00:00"),
                json!("2026-10-21T18:00:00+00:00"),
                json!("2026-10-28T19:00:00+00:00"),
            ]
        );
        assert_eq!(raws[0].source_event_id, "evt-w#20260930T180000Z");
        assert_eq!(raws[0].payload["end"], json!("2026-09-30T20:00:00+00:00"));
    }

    #[test]
    fn rules_are_bounded_by_the_window_until_and_byday() {
        let raws = feed(
            "BEGIN:VEVENT\nUID:evt-d\nSUMMARY:Daily\nDTSTART:20260928T100000Z\nRRULE:FREQ=DAILY;INTERVAL=2\nEND:VEVENT\n",
        );
        assert_eq!(
            raws.len(),
            45,
            "every other day for 90 days: 28 Sep to 25 Dec"
        );
        let raws = feed(
            "BEGIN:VEVENT\nUID:evt-b\nSUMMARY:Twice weekly\nDTSTART:20260928T100000Z\n\
             RRULE:FREQ=WEEKLY;BYDAY=MO,TH;UNTIL=20261008T235959Z\nEND:VEVENT\n",
        );
        let days: Vec<_> = raws.iter().map(|r| r.payload["start"].clone()).collect();
        assert_eq!(
            days,
            [
                json!("2026-09-28T10:00:00+00:00"),
                json!("2026-10-01T10:00:00+00:00"),
                json!("2026-10-05T10:00:00+00:00"),
                json!("2026-10-08T10:00:00+00:00"),
            ]
        );
        let raws = feed(
            "BEGIN:VEVENT\nUID:evt-m\nSUMMARY:Monthly\nDTSTART:20261001T100000Z\nRRULE:FREQ=MONTHLY\nEND:VEVENT\n",
        );
        assert_eq!(raws.len(), 1, "unsupported rule: first occurrence only");
        assert_eq!(raws[0].source_event_id, "evt-m");
    }

    #[test]
    fn date_events_are_all_day_with_an_exclusive_end() {
        let raws = feed(
            "BEGIN:VEVENT\nUID:evt-a\nSUMMARY:Pop-up\nDTSTART;VALUE=DATE:20260707\nDTEND;VALUE=DATE:20261201\nEND:VEVENT\n\
             BEGIN:VEVENT\nUID:evt-o\nSUMMARY:One day\nDTSTART;VALUE=DATE:20261003\nEND:VEVENT\n",
        );
        assert_eq!(raws[0].payload["start"], "2026-07-07");
        assert_eq!(raws[0].payload["end"], "2026-11-30");
        assert_eq!(raws[1].payload["end"], "2026-10-03");
        let c = config(json!({"calendar_id": "cal-x", "default_category": "exhibition"}));
        let mut payload = raws[1].payload.clone();
        payload["location"] = json!("Gallery, 1 Road, London N1 1AA, UK");
        let ev = normalise_payload(&payload, &c).unwrap().unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-10-02T23:00:00+00:00");
        assert_eq!(ev.ends_at, None);
    }

    fn event(location: Option<&str>, geo: Option<(f64, f64)>) -> Value {
        json!({
            "uid": "evt-1", "title": "Zine workshop", "url": "https://luma.com/abc",
            "location": location, "geo": geo.map(|(a, b)| json!([a, b])),
            "all_day": false, "start": "2026-10-01T17:00:00+00:00", "end": "2026-10-01T19:00:00+00:00",
        })
    }

    #[test]
    fn location_rules() {
        let c = config(json!({"calendar_id": "cal-x"}));
        let n = |p: Value| normalise_payload(&p, &c).unwrap();
        let ev = n(event(
            Some("The Bersey Warehouse, 293 Old St, London EC1V 9LA, UK"),
            Some((51.52, -0.08)),
        ))
        .unwrap();
        assert_eq!(ev.venue_name.as_deref(), Some("The Bersey Warehouse"));
        assert_eq!(
            ev.address.as_deref(),
            Some("293 Old St, London EC1V 9LA, UK")
        );
        assert_eq!(ev.lat, Some(51.52));
        assert_eq!(ev.category, Category::Workshop);
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-10-01T17:00:00+00:00");

        let street = n(event(Some("222 Brixton Rd, London SW9 6AH, UK"), None)).unwrap();
        assert_eq!(street.venue_name, None);
        assert_eq!(
            street.address.as_deref(),
            Some("222 Brixton Rd, London SW9 6AH, UK")
        );

        let hidden = n(event(
            Some("https://luma.com/event/evt-1"),
            Some((51.53, -0.07)),
        ))
        .unwrap();
        assert_eq!(
            (hidden.venue_name, hidden.address, hidden.lat),
            (None, None, None)
        );

        assert_eq!(n(event(Some("https://zoom.us/j/1"), None)), None, "online");
        assert_eq!(
            n(event(Some("Studio, Philadelphia"), Some((39.9, -75.2)))),
            None
        );
        assert_eq!(n(event(Some("Hall, Manchester"), None)), None);
    }

    #[test]
    fn category_skip_keywords_then_keywords_then_default() {
        let n = |title: &str, c: Value| {
            let mut p = event(Some("Venue, London"), None);
            p["title"] = json!(title);
            normalise_payload(&p, &config(c))
                .unwrap()
                .map(|e| e.category)
        };
        let skip = json!({"calendar_id": "cal-x", "skip_keywords": ["screening", "sound bath"],
                          "default_category": "community"});
        assert_eq!(n("Film Screening and Q&A", skip.clone()), None);
        assert_eq!(n("Sunday Sound Bath", skip.clone()), None);
        assert_eq!(n("Sound design talk", skip.clone()), Some(Category::Talk));
        assert_eq!(n("Hangout", skip), Some(Category::Community));
        assert_eq!(n("Hangout", json!({"calendar_id": "cal-x"})), None);
    }
}
