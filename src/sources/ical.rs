//! Reading iCalendar feeds (RFC 5545), shared by the sources that read one
//! (`luma`, `vortex_jazz_club`): line unfolding, content lines, VEVENTs,
//! TEXT unescaping and DATE/DATE-TIME values. No iCalendar crate.

use std::collections::BTreeMap;

use chrono::{DateTime, Days, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::Tz;

use super::SourceError;
use crate::normalise::{clean_text, london_to_utc};

/// One content line: `NAME;PARAM=V;…:VALUE`.
#[derive(Debug, Clone)]
pub(crate) struct Property {
    pub(crate) params: BTreeMap<String, String>,
    pub(crate) value: String,
}

pub(crate) type Component = BTreeMap<String, Vec<Property>>;

/// Undo RFC 5545 line folding (a line break followed by a space or tab) on
/// the raw bytes, then decode.
pub(crate) fn unfold(bytes: &[u8]) -> String {
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
pub(crate) fn vevents(text: &str) -> Result<Vec<Component>, SourceError> {
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
pub(crate) fn unescape(s: &str) -> String {
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

pub(crate) fn first<'a>(event: &'a Component, name: &str) -> Option<&'a Property> {
    event.get(name).and_then(|v| v.first())
}

pub(crate) fn text(event: &Component, name: &str) -> Option<String> {
    first(event, name)
        .map(|p| clean_text(&unescape(&p.value)))
        .filter(|t| !t.is_empty())
}

/// A DTSTART/DTEND/EXDATE value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum IcsTime {
    /// `VALUE=DATE`.
    Date(NaiveDate),
    /// Wall-clock time in a zone (`TZID=`; floating times use London), or
    /// UTC (`…Z`, `Tz::UTC`).
    Local(NaiveDateTime, Tz),
}

impl IcsTime {
    pub(crate) fn utc(self) -> DateTime<Utc> {
        match self {
            IcsTime::Date(d) => london_to_utc(d.and_time(NaiveTime::MIN)),
            IcsTime::Local(t, tz) => local_to_utc(t, tz),
        }
    }

    pub(crate) fn shifted(self, days: u64) -> Option<IcsTime> {
        Some(match self {
            IcsTime::Date(d) => IcsTime::Date(d.checked_add_days(Days::new(days))?),
            IcsTime::Local(t, tz) => IcsTime::Local(t.checked_add_days(Days::new(days))?, tz),
        })
    }

    pub(crate) fn date(self) -> NaiveDate {
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

pub(crate) fn parse_time_value(value: &str, params: &BTreeMap<String, String>) -> Option<IcsTime> {
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
