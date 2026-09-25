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
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Exhibition,
        Category::Expo,
        Category::Community,
        Category::Talk,
        Category::Workshop,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Exhibition => "exhibition",
            Category::Expo => "expo",
            Category::Community => "community",
            Category::Talk => "talk",
            Category::Workshop => "workshop",
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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
    pub price: Price,
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub category: Category,
    pub tags: Vec<String>,
    pub dedupe_key: String,
}
