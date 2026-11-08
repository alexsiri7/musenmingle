//! Reading iCalendar feeds (RFC 5545), shared by the sources that read one
//! (`luma`, `vortex_jazz_club`): line unfolding, content lines, VEVENTs and
//! TEXT unescaping. No iCalendar crate.

use std::collections::BTreeMap;

use super::SourceError;
use crate::normalise::clean_text;

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
