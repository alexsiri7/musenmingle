//! What the model sees about an event: ONLY facts we already store in
//! `events.events` (title, venue, dates, category, source tags, price, the
//! stored excerpt) plus the names of the sites we found it on. Nothing is
//! fetched, and a facts-only source (no stored description) sends just the
//! facts. The address, coordinates, URLs and images are not sent.

use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Europe::London;
use rust_decimal::Decimal;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

/// One stored event as the enrichment pass reads it.
#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct EventFacts {
    pub id: Uuid,
    pub title: String,
    pub venue_name: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub category: String,
    pub tags: Vec<String>,
    pub is_free: bool,
    pub price_min: Option<Decimal>,
    pub price_max: Option<Decimal>,
    pub currency: Option<String>,
    /// The stored excerpt (already cut to <= 300 characters, and NULL for
    /// sources whose terms don't let us keep descriptions).
    pub description: Option<String>,
    /// Display names of the sources listing the event, sorted.
    pub listed_by: Vec<String>,
}

/// The JSON object sent for one event.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PromptEvent {
    pub id: String,
    pub title: String,
    pub venue: Option<String>,
    pub when: String,
    pub category: String,
    pub source_tags: Vec<String>,
    pub price: Option<String>,
    pub excerpt: Option<String>,
    pub listed_by: Vec<String>,
}

fn fmt_day(t: DateTime<Utc>) -> String {
    t.with_timezone(&London).format("%a %-d %b %Y").to_string()
}

fn fmt_day_time(t: DateTime<Utc>) -> String {
    let l = t.with_timezone(&London);
    if l.time() == NaiveTime::MIN {
        fmt_day(t)
    } else {
        l.format("%a %-d %b %Y, %H:%M").to_string()
    }
}

/// London dates, independent of "now" so the input hash is stable:
/// "Sat 3 Oct 2026, 18:30–20:00", "Sat 3 Oct 2026 – Sun 3 Jan 2027".
pub fn when_text(start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> String {
    match end {
        Some(end)
            if end > start
                && end.with_timezone(&London).date_naive()
                    != start.with_timezone(&London).date_naive() =>
        {
            format!("{} – {}", fmt_day(start), fmt_day(end))
        }
        Some(end) if end > start && start.with_timezone(&London).time() != NaiveTime::MIN => {
            format!(
                "{}–{}",
                fmt_day_time(start),
                end.with_timezone(&London).format("%H:%M")
            )
        }
        _ => fmt_day_time(start),
    }
}

fn money(amount: Decimal, currency: Option<&str>) -> String {
    let amount = if amount.fract().is_zero() {
        amount.trunc().to_string()
    } else {
        format!("{amount:.2}")
    };
    match currency {
        None | Some("GBP") => format!("£{amount}"),
        Some("EUR") => format!("€{amount}"),
        Some("USD") => format!("${amount}"),
        Some(other) => format!("{amount} {other}"),
    }
}

impl EventFacts {
    pub fn price_text(&self) -> Option<String> {
        if self.is_free {
            return Some("Free".into());
        }
        let c = self.currency.as_deref();
        match (self.price_min, self.price_max) {
            (Some(a), Some(b)) if a != b => Some(format!("{}–{}", money(a, c), money(b, c))),
            (Some(a), _) => Some(money(a, c)),
            (None, Some(b)) => Some(format!("up to {}", money(b, c))),
            (None, None) => None,
        }
    }

    /// The event as sent to the model, under the batch-local id `id`.
    pub fn prompt_event(&self, id: &str) -> PromptEvent {
        PromptEvent {
            id: id.to_string(),
            title: self.title.clone(),
            venue: self.venue_name.clone(),
            when: when_text(self.starts_at, self.ends_at),
            category: self.category.clone(),
            source_tags: self.tags.clone(),
            price: self.price_text(),
            excerpt: self.description.clone().filter(|d| !d.trim().is_empty()),
            listed_by: self.listed_by.clone(),
        }
    }

    /// sha256 of exactly what the model would see (without the batch id):
    /// a new hash means the stored enrichment may be out of date.
    pub fn input_hash(&self) -> String {
        let json = serde_json::to_string(&self.prompt_event("")).expect("serialisable");
        hex(&Sha256::digest(json.as_bytes()))
    }

    /// The text artists and opening evidence must be quoted from.
    pub fn grounding_text(&self) -> String {
        let p = self.prompt_event("");
        [
            p.title,
            p.venue.unwrap_or_default(),
            p.source_tags.join(" | "),
            p.excerpt.unwrap_or_default(),
        ]
        .join(" | ")
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn facts() -> EventFacts {
        EventFacts {
            id: Uuid::nil(),
            title: "Rosa Barba".into(),
            venue_name: Some("Barbican".into()),
            starts_at: Utc.with_ymd_and_hms(2026, 10, 3, 17, 30, 0).unwrap(),
            ends_at: Some(Utc.with_ymd_and_hms(2026, 10, 3, 19, 0, 0).unwrap()),
            category: "talk".into(),
            tags: vec!["Art & design".into()],
            is_free: false,
            price_min: Some(Decimal::new(12, 0)),
            price_max: Some(Decimal::new(1850, 2)),
            currency: Some("GBP".into()),
            description: Some("A new site-specific work.".into()),
            listed_by: vec!["Barbican".into()],
        }
    }

    #[test]
    fn prompt_event_has_london_times_and_prices() {
        let p = facts().prompt_event("e1");
        assert_eq!(p.when, "Sat 3 Oct 2026, 18:30–20:00");
        assert_eq!(p.price.as_deref(), Some("£12–£18.50"));
        let mut f = facts();
        f.ends_at = Some(Utc.with_ymd_and_hms(2027, 1, 3, 12, 0, 0).unwrap());
        assert_eq!(f.prompt_event("e1").when, "Sat 3 Oct 2026 – Sun 3 Jan 2027");
    }

    #[test]
    fn hash_changes_only_with_what_the_model_sees() {
        let a = facts();
        let mut b = facts();
        b.id = Uuid::new_v4();
        assert_eq!(a.input_hash(), b.input_hash(), "id is not input");
        b.description = Some("Changed.".into());
        assert_ne!(a.input_hash(), b.input_hash());
        let mut c = facts();
        c.description = Some("   ".into());
        let mut d = facts();
        d.description = None;
        assert_eq!(c.input_hash(), d.input_hash(), "blank excerpt = none");
    }
}
