//! Core data types shared by sources, normalisation and the repository.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Event category. Stored as text with a CHECK constraint in `events.events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Exhibition,
    Expo,
    Community,
    Talk,
    Workshop,
    /// Concerts and live music at the 'arty' end (issue #209, option (b)):
    /// classical, contemporary, experimental, jazz, sound art. Subtags are
    /// `crate::music`'s.
    Music,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::Exhibition,
        Category::Expo,
        Category::Community,
        Category::Talk,
        Category::Workshop,
        Category::Music,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Exhibition => "exhibition",
            Category::Expo => "expo",
            Category::Community => "community",
            Category::Talk => "talk",
            Category::Workshop => "workshop",
            Category::Music => "music",
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a source gets its events. Stored as text with a CHECK constraint in
/// `events.sources`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum SourceKind {
    /// A third-party API (e.g. Ticketmaster).
    Api,
    /// A venue's own website.
    Scraper,
    /// A third-party site listing many venues' events (none at present). It
    /// never takes precedence in a merge (it only fills gaps) and is the
    /// last choice for an event's main link, after the venue's own site.
    Aggregator,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Api => "api",
            SourceKind::Scraper => "scraper",
            SourceKind::Aggregator => "aggregator",
        }
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A manual merge correction. Stored as text with a CHECK constraint in
/// `events.merge_overrides`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum OverrideAction {
    NeverMerge,
    ForceMerge,
}

/// One item as fetched from a source, before normalisation.
///
/// `payload` is whatever the source found (API JSON object, JSON-LD node,
/// scraped fields) and is stored verbatim in `events.event_sources.raw`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawEvent {
    /// Stable identifier of the event within the source.
    pub source_event_id: String,
    /// Canonical URL of the event on the source, if any.
    pub source_url: Option<String>,
    pub payload: serde_json::Value,
}

/// One session of a multi-session event (issue #207): a start and an
/// optional end, both instants. Stored as a JSON array in
/// `events.events.sessions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Session {
    pub starts_at: DateTime<Utc>,
    #[serde(default)]
    pub ends_at: Option<DateTime<Utc>>,
}

impl Session {
    /// When the session is over: its end, else its start plus
    /// [`crate::listing::LIVE_GRACE_MINUTES`] (the rule for one-offs).
    pub fn effective_end(&self) -> DateTime<Utc> {
        self.ends_at.filter(|e| *e > self.starts_at).unwrap_or(
            self.starts_at + chrono::Duration::minutes(crate::listing::LIVE_GRACE_MINUTES),
        )
    }
}

/// The sessions still to come or running at `now`: (the next one, how many
/// sessions in all). `None` when there are no sessions or all are over.
pub fn next_session(sessions: &[Session], now: DateTime<Utc>) -> Option<(Session, usize)> {
    sessions
        .iter()
        .find(|s| s.effective_end() > now)
        .map(|s| (*s, sessions.len()))
}

/// Price information after parsing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    pub is_free: bool,
    pub min: Option<Decimal>,
    pub max: Option<Decimal>,
    pub currency: Option<String>,
}

/// A normalised event ready to be upserted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEvent {
    pub title: String,
    pub description: Option<String>,
    pub venue_name: Option<String>,
    pub address: Option<String>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    /// The source gave dates but no time of day: `starts_at` is London
    /// midnight of the first day and `ends_at` London midnight of the last
    /// day (inclusive), or `None` for a single day.
    pub all_day: bool,
    /// The separate sessions of a multi-session event (issue #207), in
    /// order; empty for a one-off or a continuous run. When set,
    /// `starts_at`/`ends_at` are the envelope (first session's start, last
    /// session's end): see [`NewEvent::set_sessions`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<Session>,
    pub price: Price,
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub category: Category,
    pub tags: Vec<String>,
    pub dedupe_key: String,
}

impl NewEvent {
    /// Make this a multi-session event: sort and dedupe `sessions`, drop
    /// ends not after their start, and set the envelope (`starts_at` = the
    /// first start, `ends_at` = the last session's end, else its start when
    /// later than the first; `all_day` only when every session is all-day,
    /// i.e. starts at London midnight, and then `ends_at` is the last
    /// session's day, as for any all-day run). Fewer than two sessions
    /// leave a plain timed event (no `sessions`). The caller recomputes
    /// `dedupe_key` from the new `starts_at` if it depends on it.
    pub fn set_sessions(&mut self, mut sessions: Vec<Session>) {
        for s in &mut sessions {
            s.ends_at = s.ends_at.filter(|e| *e > s.starts_at);
        }
        sessions.sort();
        sessions.dedup_by_key(|s| s.starts_at);
        let (Some(first), Some(last)) = (sessions.first().copied(), sessions.last().copied())
        else {
            return;
        };
        self.starts_at = first.starts_at;
        // All-day sessions (from London midnight): an all-day envelope, its
        // `ends_at` the last day's midnight as for any all-day run.
        self.all_day = sessions
            .iter()
            .all(|s| crate::normalise::is_london_midnight(s.starts_at));
        self.ends_at = if self.all_day {
            Some(last.starts_at)
        } else {
            last.ends_at.or(Some(last.starts_at))
        }
        .filter(|e| *e > first.starts_at);
        self.sessions = if sessions.len() >= 2 {
            sessions
        } else {
            Vec::new()
        };
    }
}
