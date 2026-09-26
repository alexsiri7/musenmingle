//! The text we embed for each event (versioned by [`EMBED_VERSION`]).
//!
//! Template (lines are omitted when empty):
//!
//! ```text
//! <title>
//! <category> at <venue>, <dates>
//! Media: <medium tags as words>
//! Format: <format tags as words>
//! Good for: <good_for as words>
//! Vibe: <vibe tags>
//! Artists: <artists>
//! <one_liner>
//! <whats_cool>
//! <stored excerpt>
//! ```
//!
//! Dates are phrased without reference to "now" ("exhibition at X, 26 Sep
//! 2026 to 31 Jan 2027"; "talk at Y, on Sat 4 Oct 2026 at 19:00") so the
//! text, and its hash, only change when the stored data does. Only stored
//! facts and our enrichment output are used (venue type / borough will join
//! when those fields exist; bump the version then).

use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Europe::London;
use sha2::{Digest, Sha256};

use super::input::{EventFacts, hex};
use super::output::{Enrichment, label};

/// Bump when the template changes: every event is re-embedded.
pub const EMBED_VERSION: i32 = 1;
/// Dimensions of `events.event_embeddings.embedding`.
pub const EMBED_DIMS: usize = 1536;

fn natural_dates(start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> String {
    let s = start.with_timezone(&London);
    match end.map(|e| e.with_timezone(&London)) {
        Some(e) if e.date_naive() != s.date_naive() && e > s => {
            format!("{} to {}", s.format("%-d %b %Y"), e.format("%-d %b %Y"))
        }
        _ if s.time() == NaiveTime::MIN => format!("on {}", s.format("%a %-d %b %Y")),
        _ => format!("on {}", s.format("%a %-d %b %Y at %H:%M")),
    }
}

fn words(tags: &[String]) -> String {
    tags.iter().map(|t| label(t)).collect::<Vec<_>>().join(", ")
}

/// The embedding text for an event, with its enrichment when there is a
/// current one.
pub fn embed_text(f: &EventFacts, e: Option<&Enrichment>) -> String {
    let mut lines = vec![f.title.clone()];
    let mut what = f.category.clone();
    if let Some(v) = f.venue_name.as_deref().filter(|v| !v.is_empty()) {
        what.push_str(" at ");
        what.push_str(v);
    }
    lines.push(format!("{what}, {}", natural_dates(f.starts_at, f.ends_at)));
    if let Some(e) = e {
        for (name, tags) in [
            ("Media", &e.medium_tags),
            ("Format", &e.format_tags),
            ("Good for", &e.good_for),
            ("Vibe", &e.vibe_tags),
        ] {
            if !tags.is_empty() {
                lines.push(format!("{name}: {}", words(tags)));
            }
        }
        if !e.artists.is_empty() {
            lines.push(format!("Artists: {}", e.artists.join(", ")));
        }
        lines.extend(e.one_liner.clone());
        lines.extend(e.whats_cool.clone());
    }
    lines.extend(f.description.clone().filter(|d| !d.trim().is_empty()));
    lines.join("\n")
}

pub fn text_hash(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use uuid::Uuid;

    #[test]
    fn template_includes_enrichment_as_words() {
        let f = EventFacts {
            id: Uuid::nil(),
            title: "Lino printing".into(),
            venue_name: Some("Garden Museum".into()),
            starts_at: Utc.with_ymd_and_hms(2026, 12, 6, 11, 0, 0).unwrap(),
            ends_at: None,
            category: "workshop".into(),
            tags: vec![],
            is_free: false,
            price_min: None,
            price_max: None,
            currency: None,
            description: Some("Make cards.".into()),
            listed_by: vec![],
        };
        let e = Enrichment {
            medium_tags: vec!["printmaking".into()],
            format_tags: vec!["hands_on".into(), "family_friendly".into()],
            good_for: vec!["kids".into()],
            vibe_tags: vec![],
            artists: vec![],
            is_opening: false,
            opening_evidence: None,
            grounding: "listing".into(),
            whats_cool: Some("Carve and print your own cards.".into()),
            one_liner: Some("Family lino printing".into()),
            confidence: 0.9,
        };
        assert_eq!(
            embed_text(&f, Some(&e)),
            "Lino printing\nworkshop at Garden Museum, on Sun 6 Dec 2026 at 11:00\n\
             Media: printmaking\nFormat: hands-on, family-friendly\nGood for: kids\n\
             Family lino printing\nCarve and print your own cards.\nMake cards."
        );
        assert_eq!(
            embed_text(&f, None),
            "Lino printing\nworkshop at Garden Museum, on Sun 6 Dec 2026 at 11:00\nMake cards."
        );
        assert_ne!(
            text_hash(&embed_text(&f, None)),
            text_hash(&embed_text(&f, Some(&e)))
        );
    }
}
