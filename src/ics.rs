//! iCalendar (RFC 5545) output for the subscribable feed `GET /calendar.ics`
//! (pure; the handler is in `web.rs`).
//!
//! Lines end in CRLF and are folded at 75 octets without splitting a UTF-8
//! character. Timed events are written in UTC (`…Z`, so no VTIMEZONE is
//! needed); untimed and long-running events are all-day `VALUE=DATE` spans
//! of their London dates, whose `DTEND` is exclusive. UIDs are
//! `<event id>@musenmingle.interstellarai.net`, the same as the saved-events
//! export in `web.js`, so a calendar app can match the two.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use uuid::Uuid;

use crate::calendar;

/// Domain part of every UID (stable across hosts and deploys).
pub const UID_DOMAIN: &str = "musenmingle.interstellarai.net";

/// One event of a feed.
#[derive(Debug, Clone)]
pub struct FeedEvent {
    pub id: Uuid,
    pub title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub all_day: bool,
    /// Venue name and address, joined for `LOCATION`.
    pub location: Option<String>,
    /// Where to see it (the venue's page); already checked to be http(s).
    pub url: Option<String>,
    /// Plain text for `DESCRIPTION`.
    pub description: String,
}

/// Escape a TEXT value (RFC 5545 3.3.11): backslash, semicolon, comma and
/// newlines; other control characters are dropped.
pub fn escape_text(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' | '\r' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// A content line folded at 75 octets (RFC 5545 3.1), CRLF-terminated.
/// Continuation lines start with one space, so they carry 74 octets.
pub fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + line.len() / 70 * 3 + 2);
    let mut used = 0;
    let mut limit = 75;
    for c in line.chars() {
        let n = c.len_utf8();
        if used + n > limit {
            out.push_str("\r\n ");
            used = 0;
            limit = 74;
        }
        out.push(c);
        used += n;
    }
    out.push_str("\r\n");
    out
}

fn utc_stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn date_value(d: NaiveDate) -> String {
    d.format("%Y%m%d").to_string()
}

/// The `VEVENT` property lines of one event (unfolded).
fn event_lines(e: &FeedEvent, stamp: &str) -> Vec<String> {
    let mut lines = vec![
        "BEGIN:VEVENT".to_string(),
        format!("UID:{}@{UID_DOMAIN}", e.id),
        format!("DTSTAMP:{stamp}"),
    ];
    let (first, last) = calendar::span(e.starts_at, e.ends_at);
    if calendar::is_untimed(e.starts_at, e.all_day) || calendar::is_long_running(first, last) {
        lines.push(format!("DTSTART;VALUE=DATE:{}", date_value(first)));
        lines.push(format!(
            "DTEND;VALUE=DATE:{}",
            date_value(last + Duration::days(1))
        ));
    } else {
        lines.push(format!("DTSTART:{}", utc_stamp(e.starts_at)));
        if let Some(end) = e.ends_at.filter(|end| *end > e.starts_at) {
            lines.push(format!("DTEND:{}", utc_stamp(end)));
        }
    }
    lines.push(format!("SUMMARY:{}", escape_text(&e.title)));
    if let Some(l) = e.location.as_deref().filter(|l| !l.trim().is_empty()) {
        lines.push(format!("LOCATION:{}", escape_text(l)));
    }
    if !e.description.is_empty() {
        lines.push(format!("DESCRIPTION:{}", escape_text(&e.description)));
    }
    // URL is a URI value, not TEXT: never escaped (it is an http(s) URL
    // without control characters or line breaks).
    if let Some(u) = e
        .url
        .as_deref()
        .filter(|u| !u.chars().any(char::is_control))
    {
        lines.push(format!("URL:{u}"));
    }
    lines.push("END:VEVENT".to_string());
    lines
}

/// A complete `VCALENDAR` named `name`.
pub fn calendar(name: &str, events: &[FeedEvent], now: DateTime<Utc>) -> String {
    let stamp = utc_stamp(now);
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Muse & Mingle//London events//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
        "METHOD:PUBLISH".to_string(),
        format!("X-WR-CALNAME:{}", escape_text(name)),
        "X-WR-TIMEZONE:Europe/London".to_string(),
        "REFRESH-INTERVAL;VALUE=DURATION:PT1H".to_string(),
        "X-PUBLISHED-TTL:PT1H".to_string(),
    ];
    for e in events {
        lines.extend(event_lines(e, &stamp));
    }
    lines.push("END:VCALENDAR".to_string());
    lines.iter().map(|l| fold(l)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ev(title: &str, starts_at: DateTime<Utc>) -> FeedEvent {
        FeedEvent {
            id: Uuid::parse_str("0414a989-8ba8-41da-a6ac-b92720727755").unwrap(),
            title: title.into(),
            starts_at,
            ends_at: None,
            all_day: false,
            location: None,
            url: None,
            description: String::new(),
        }
    }

    #[test]
    fn escapes_text_values() {
        assert_eq!(
            escape_text("a\\b; c, d\r\ne\nf\u{7}"),
            "a\\\\b\\; c\\, d\\ne\\nf"
        );
    }

    #[test]
    fn folds_at_75_octets_on_character_boundaries() {
        let short = "SUMMARY:short";
        assert_eq!(fold(short), "SUMMARY:short\r\n");
        let long = format!("SUMMARY:{}", "é".repeat(100));
        let folded = fold(&long);
        assert!(folded.ends_with("\r\n"));
        let lines: Vec<&str> = folded.trim_end_matches("\r\n").split("\r\n").collect();
        assert!(lines.len() > 2);
        for (i, l) in lines.iter().enumerate() {
            assert!(l.len() <= 75, "{i}: {} octets", l.len());
            if i > 0 {
                assert!(l.starts_with(' '));
            }
        }
        // Unfolding gives the original line back.
        assert_eq!(folded.trim_end_matches("\r\n").replace("\r\n ", ""), long);
    }

    #[test]
    fn writes_timed_all_day_and_long_running_events() {
        let now = Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap();
        // 18:30 BST.
        let talk_start = Utc.with_ymd_and_hms(2026, 10, 7, 17, 30, 0).unwrap();
        let talk = FeedEvent {
            ends_at: Some(talk_start + Duration::minutes(90)),
            location: Some("Barbican, Silk St".into()),
            url: Some("https://www.barbican.org.uk/a?b=c;d".into()),
            description: "An excerpt.\n\nvia Muse & Mingle".into(),
            ..ev("Talk: art, craft; and more", talk_start)
        };
        // Date-only exhibition, 16 Oct – 25 Oct (London midnight = 23:00 UTC).
        let show = FeedEvent {
            ends_at: Some(Utc.with_ymd_and_hms(2026, 10, 24, 23, 0, 0).unwrap()),
            all_day: true,
            ..ev(
                "Show",
                Utc.with_ymd_and_hms(2026, 10, 15, 23, 0, 0).unwrap(),
            )
        };
        // Timed but running for months: still an all-day span.
        let long = FeedEvent {
            ends_at: Some(Utc.with_ymd_and_hms(2027, 1, 31, 18, 0, 0).unwrap()),
            ..ev("Long", Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap())
        };
        let ics = calendar("Muse & Mingle, London", &[talk, show, long], now);
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        assert!(!ics.replace("\r\n", "").contains('\n'), "bare LF");
        assert!(ics.contains("X-WR-CALNAME:Muse & Mingle\\, London\r\n"));
        assert!(ics.contains(
            "UID:0414a989-8ba8-41da-a6ac-b92720727755@musenmingle.interstellarai.net\r\n"
        ));
        assert!(ics.contains("DTSTAMP:20261001T090000Z\r\n"));
        assert!(ics.contains("DTSTART:20261007T173000Z\r\nDTEND:20261007T190000Z\r\n"));
        assert!(ics.contains("SUMMARY:Talk: art\\, craft\\; and more\r\n"));
        assert!(ics.contains("LOCATION:Barbican\\, Silk St\r\n"));
        assert!(ics.contains("DESCRIPTION:An excerpt.\\n\\nvia Muse & Mingle\r\n"));
        assert!(ics.contains("URL:https://www.barbican.org.uk/a?b=c;d\r\n"));
        assert!(ics.contains("DTSTART;VALUE=DATE:20261016\r\nDTEND;VALUE=DATE:20261026\r\n"));
        assert!(ics.contains("DTSTART;VALUE=DATE:20260901\r\nDTEND;VALUE=DATE:20270201\r\n"));
        assert_eq!(ics.matches("BEGIN:VEVENT").count(), 3);
        assert_eq!(ics.matches("END:VEVENT").count(), 3);
    }
}
