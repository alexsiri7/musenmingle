//! Native hand-offs for an event (issue #76): the `.ics` download, the
//! Google Calendar template link, and Google / Apple Maps links (pure).
//!
//! These links only take the visitor to Google or Apple when they tap one;
//! they carry the event's own facts and no tracking parameters (`/about`
//! says so).
//!
//! Calendar entries: a timed event keeps its exact UTC times (`...Z`), with
//! no end when the listing gives none. An all-day event, including a
//! months-long exhibition run, is one all-day entry spanning its whole
//! London date range (`DTEND` is the day after the last day, exclusive), so
//! the calendar shows when it can be visited; we don't invent a visit slot.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Europe::London;
use uuid::Uuid;

/// What the hand-offs need to know about an event.
#[derive(Debug, Clone)]
pub struct ShareEvent<'a> {
    pub id: Uuid,
    pub title: &'a str,
    pub venue: Option<&'a str>,
    pub address: Option<&'a str>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub all_day: bool,
    /// The stored excerpt (already cut to the policy's length), if any.
    pub excerpt: Option<&'a str>,
    /// The event's page on its source (the calendar entry's URL).
    pub source_url: Option<String>,
    /// Our page for the event (absolute).
    pub page_url: &'a str,
}

impl ShareEvent<'_> {
    /// "Venue, address" (either may be missing), for LOCATION and maps.
    pub fn location(&self) -> Option<String> {
        let parts: Vec<&str> = [self.venue, self.address]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// First and last London day of an all-day event.
    fn days(&self) -> (NaiveDate, NaiveDate) {
        let day = |t: DateTime<Utc>| t.with_timezone(&London).date_naive();
        let first = day(self.starts_at);
        (first, self.ends_at.map(day).unwrap_or(first).max(first))
    }

    fn description(&self) -> String {
        match self.excerpt.map(str::trim).filter(|s| !s.is_empty()) {
            Some(x) => format!("{x}\n\nvia Muse & Mingle: {}", self.page_url),
            None => format!("via Muse & Mingle: {}", self.page_url),
        }
    }
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn utc_stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn date_stamp(d: NaiveDate) -> String {
    d.format("%Y%m%d").to_string()
}

// ------------------------------------------------------------ iCalendar

/// Escape a TEXT value (RFC 5545 3.3.11): `\`, `;`, `,` and newlines.
pub fn ics_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Fold a content line at 75 octets (RFC 5545 3.1), never inside a UTF-8
/// character; continuation lines start with a space.
pub fn ics_fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + line.len() / 70 * 3);
    let mut width = 0;
    for c in line.chars() {
        let n = c.len_utf8();
        if width + n > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += n;
    }
    out
}

/// The `.ics` file for one event: a VCALENDAR with a single VEVENT.
/// `now` is the DTSTAMP.
pub fn ics(e: &ShareEvent<'_>, now: DateTime<Utc>) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Muse & Mingle//Event//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
        "METHOD:PUBLISH".to_string(),
        "BEGIN:VEVENT".to_string(),
        // Same UID as the Saved-events export (src/web.js), so re-adding
        // updates rather than duplicates.
        format!("UID:{}@musenmingle.interstellarai.net", e.id),
        format!("DTSTAMP:{}", utc_stamp(now)),
    ];
    if e.all_day {
        let (first, last) = e.days();
        lines.push(format!("DTSTART;VALUE=DATE:{}", date_stamp(first)));
        lines.push(format!(
            "DTEND;VALUE=DATE:{}",
            date_stamp(last + Duration::days(1))
        ));
    } else {
        lines.push(format!("DTSTART:{}", utc_stamp(e.starts_at)));
        if let Some(end) = e.ends_at {
            lines.push(format!("DTEND:{}", utc_stamp(end)));
        }
    }
    lines.push(format!("SUMMARY:{}", ics_escape(e.title)));
    if let Some(l) = e.location() {
        lines.push(format!("LOCATION:{}", ics_escape(&l)));
    }
    if let (Some(lat), Some(lng)) = (e.lat, e.lng) {
        lines.push(format!("GEO:{lat:.6};{lng:.6}"));
    }
    lines.push(format!("DESCRIPTION:{}", ics_escape(&e.description())));
    lines.push(format!(
        "URL:{}",
        e.source_url.as_deref().unwrap_or(e.page_url)
    ));
    lines.push("END:VEVENT".into());
    lines.push("END:VCALENDAR".into());
    let mut out = String::new();
    for l in lines {
        out.push_str(&ics_fold(&l));
        out.push_str("\r\n");
    }
    out
}

/// `<slug>.ics` for the Content-Disposition header: ASCII letters and
/// digits of the title, dash-separated, at most 60 characters.
pub fn ics_filename(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 60 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "event.ics".into()
    } else {
        format!("{slug}.ics")
    }
}

/// "Add to Google Calendar": the event template link. Dates are UTC; an
/// all-day event uses `YYYYMMDD/YYYYMMDD` (end exclusive); a timed event
/// without an end starts and ends at its start.
pub fn google_calendar_url(e: &ShareEvent<'_>) -> String {
    let dates = if e.all_day {
        let (first, last) = e.days();
        format!(
            "{}/{}",
            date_stamp(first),
            date_stamp(last + Duration::days(1))
        )
    } else {
        format!(
            "{}/{}",
            utc_stamp(e.starts_at),
            utc_stamp(e.ends_at.unwrap_or(e.starts_at))
        )
    };
    let mut url = format!(
        "https://calendar.google.com/calendar/render?action=TEMPLATE&text={}&dates={}",
        enc(e.title),
        enc(&dates)
    );
    if let Some(l) = e.location() {
        url.push_str("&location=");
        url.push_str(&enc(&l));
    }
    url.push_str("&details=");
    url.push_str(&enc(&e.description()));
    url
}

// ------------------------------------------------------------ maps

/// Where to point the maps apps: coordinates, else the location text.
fn place(e: &ShareEvent<'_>) -> Option<String> {
    match (e.lat, e.lng) {
        (Some(lat), Some(lng)) => Some(format!("{lat},{lng}")),
        _ => e.location(),
    }
}

/// Maps links for an event: open it in Google Maps / Apple Maps, and
/// public-transport directions in each. `None` without a place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapLinks {
    pub google_view: String,
    pub google_directions: String,
    pub apple_view: String,
    pub apple_directions: String,
}

pub fn map_links(e: &ShareEvent<'_>) -> Option<MapLinks> {
    let place = place(e)?;
    let p = enc(&place);
    let apple_view = match (e.lat, e.lng) {
        (Some(_), Some(_)) => format!(
            "https://maps.apple.com/?q={}&ll={p}",
            enc(e.venue.unwrap_or(e.title))
        ),
        _ => format!("https://maps.apple.com/?q={p}"),
    };
    Some(MapLinks {
        google_view: format!("https://www.google.com/maps/search/?api=1&query={p}"),
        google_directions: format!(
            "https://www.google.com/maps/dir/?api=1&destination={p}&travelmode=transit"
        ),
        apple_view,
        apple_directions: format!("https://maps.apple.com/?daddr={p}&dirflg=r"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn event() -> ShareEvent<'static> {
        ShareEvent {
            id: Uuid::parse_str("0414a989-8ba8-41da-a6ac-b92720727755").unwrap(),
            title: "Talk: glass, light; and \\ shadow",
            venue: Some("Barbican Centre"),
            address: Some("Silk St, London EC2Y 8DS"),
            lat: Some(51.52),
            lng: Some(-0.0937),
            starts_at: t("2026-10-24T18:30:00Z"),
            ends_at: Some(t("2026-10-24T20:00:00Z")),
            all_day: false,
            excerpt: Some("Line one,\nline two."),
            source_url: Some("https://www.barbican.org.uk/whats-on/x".into()),
            page_url: "https://musenmingle.interstellarai.net/events/0414a989-8ba8-41da-a6ac-b92720727755",
        }
    }

    #[test]
    fn ics_is_crlf_escaped_folded_and_timed_in_utc() {
        let e = event();
        let out = ics(&e, t("2026-09-27T10:00:00Z"));
        assert!(out.ends_with("END:VCALENDAR\r\n"));
        assert!(!out.replace("\r\n", "").contains('\n'), "bare LF");
        for line in out.split("\r\n") {
            assert!(line.len() <= 75, "{line:?}");
        }
        let unfolded = out.replace("\r\n ", "");
        assert!(unfolded.contains(
            "\r\nUID:0414a989-8ba8-41da-a6ac-b92720727755@musenmingle.interstellarai.net\r\n"
        ));
        assert!(unfolded.contains("\r\nDTSTAMP:20260927T100000Z\r\n"));
        assert!(unfolded.contains("\r\nDTSTART:20261024T183000Z\r\n"));
        assert!(unfolded.contains("\r\nDTEND:20261024T200000Z\r\n"));
        assert!(unfolded.contains("\r\nSUMMARY:Talk: glass\\, light\\; and \\\\ shadow\r\n"));
        assert!(
            unfolded.contains("\r\nLOCATION:Barbican Centre\\, Silk St\\, London EC2Y 8DS\r\n")
        );
        assert!(unfolded.contains(
            "DESCRIPTION:Line one\\,\\nline two.\\n\\nvia Muse & Mingle: https://musenmingle"
        ));
        assert!(unfolded.contains("\r\nURL:https://www.barbican.org.uk/whats-on/x\r\n"));
        // The UID depends only on the id.
        let again = ics(&event(), t("2027-01-01T00:00:00Z"));
        assert!(again.contains("UID:0414a989-8ba8-41da-a6ac-b92720727755@"));
    }

    #[test]
    fn folding_counts_octets_and_keeps_characters_whole() {
        let line = format!("SUMMARY:{}", "é".repeat(80));
        let folded = ics_fold(&line);
        for part in folded.split("\r\n") {
            assert!(part.len() <= 75, "{part:?}");
        }
        assert_eq!(folded.replace("\r\n ", ""), line);
        assert_eq!(ics_fold("short"), "short");
        assert_eq!(ics_escape("a\r\nb\u{7}"), "a\\nb");
    }

    #[test]
    fn all_day_events_span_their_london_days() {
        // An exhibition 1 Oct - 25 Oct (the clocks change on the 25th).
        let e = ShareEvent {
            all_day: true,
            starts_at: t("2026-09-30T23:00:00Z"),
            ends_at: Some(t("2026-10-24T23:00:00Z")),
            ..event()
        };
        let out = ics(&e, t("2026-09-27T10:00:00Z"));
        assert!(out.contains("\r\nDTSTART;VALUE=DATE:20261001\r\n"));
        assert!(out.contains("\r\nDTEND;VALUE=DATE:20261026\r\n"));
        assert!(google_calendar_url(&e).contains("&dates=20261001%2F20261026&"));
        // One day, no end.
        let one = ShareEvent {
            ends_at: None,
            ..e.clone()
        };
        assert!(ics(&one, t("2026-09-27T10:00:00Z")).contains("DTEND;VALUE=DATE:20261002\r\n"));
    }

    #[test]
    fn google_calendar_link_is_utc_and_encoded() {
        // 19:30 BST on 24 Oct is 18:30Z; 19:30 GMT on 26 Oct is 19:30Z.
        let url = google_calendar_url(&event());
        assert!(url.starts_with("https://calendar.google.com/calendar/render?action=TEMPLATE&text=Talk%3A+glass%2C+light%3B+and+%5C+shadow&dates=20261024T183000Z%2F20261024T200000Z&location=Barbican+Centre%2C+Silk+St"), "{url}");
        assert!(url.contains("&details=Line+one%2C%0Aline+two."));
        let gmt = ShareEvent {
            starts_at: t("2026-10-26T19:30:00Z"),
            ends_at: None,
            ..event()
        };
        assert!(google_calendar_url(&gmt).contains("&dates=20261026T193000Z%2F20261026T193000Z&"));
        let no_end = ics(&gmt, t("2026-09-27T10:00:00Z"));
        assert!(!no_end.contains("DTEND"));
    }

    #[test]
    fn maps_links_use_coordinates_else_the_address() {
        let m = map_links(&event()).unwrap();
        assert_eq!(
            m.google_view,
            "https://www.google.com/maps/search/?api=1&query=51.52%2C-0.0937"
        );
        assert_eq!(
            m.google_directions,
            "https://www.google.com/maps/dir/?api=1&destination=51.52%2C-0.0937&travelmode=transit"
        );
        assert_eq!(
            m.apple_view,
            "https://maps.apple.com/?q=Barbican+Centre&ll=51.52%2C-0.0937"
        );
        assert_eq!(
            m.apple_directions,
            "https://maps.apple.com/?daddr=51.52%2C-0.0937&dirflg=r"
        );
        let no_coords = ShareEvent {
            lat: None,
            lng: None,
            ..event()
        };
        let m = map_links(&no_coords).unwrap();
        assert!(
            m.google_view
                .ends_with("query=Barbican+Centre%2C+Silk+St%2C+London+EC2Y+8DS")
        );
        assert!(
            m.apple_view
                .starts_with("https://maps.apple.com/?q=Barbican+Centre%2C+Silk")
        );
        let nowhere = ShareEvent {
            lat: None,
            lng: None,
            venue: None,
            address: None,
            ..event()
        };
        assert_eq!(map_links(&nowhere), None);
    }

    #[test]
    fn filenames_are_ascii_slugs() {
        assert_eq!(ics_filename("Talk: glass, light!"), "talk-glass-light.ics");
        assert_eq!(ics_filename("¿¿"), "event.ics");
        assert!(ics_filename(&"a".repeat(200)).len() <= 64);
    }
}
