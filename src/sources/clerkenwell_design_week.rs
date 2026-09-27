//! Clerkenwell Design Week — the annual design festival (one event a year).
//!
//! * robots.txt (checked 2026-09-26, saved as a fixture): `User-agent: *` /
//!   `Allow: /`.
//! * The homepage carries a schema.org `Event` for the next festival in its
//!   JSON-LD graph (`startDate` / `endDate` as UTC midnights of the London
//!   days, e.g. `2027-05-25T00:00:00Z` for "25-27 MAY 2027"), so one request
//!   per run is enough. The dates are read as London days (stored as London
//!   midnight of the first and last day, like the exhibition sources).
//! * The node's `name` can lag behind its dates ("Clerkenwell Design Week
//!   2026" with 2027 dates on 2026-09-26), so a trailing year in the name is
//!   replaced by the festival's start year.
//! * The festival is spread over the neighbourhood's showrooms and venues;
//!   the venue is "Clerkenwell" with the JSON-LD's address and no
//!   coordinates. Category: expo.

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveTime, Utc};
use scraper::Html;
use serde_json::Value;
use url::Url;

use super::jsonld::{extract_events, image_url, str_or_name};
use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, london_date, london_to_utc, parse_london_wall_clock,
};

pub const KEY: &str = "clerkenwell-design-week";
const SITE: &str = "https://www.clerkenwelldesignweek.com/";

pub struct ClerkenwellDesignWeek {
    base_url: Url,
}

impl ClerkenwellDesignWeek {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// The festival's JSON-LD `Event` node(s) from the homepage.
pub fn parse_home(html: &str) -> Vec<RawEvent> {
    extract_events(&Html::parse_document(html))
        .into_iter()
        .filter_map(|node| {
            let start = node.get("startDate").and_then(Value::as_str)?;
            // One festival a year: its start date identifies it.
            let id = start.get(..10)?.to_string();
            Some(RawEvent {
                source_event_id: id,
                source_url: Some(SITE.to_string()),
                payload: node,
            })
        })
        .collect()
}

/// "Clerkenwell Design Week 2026" with 2027 dates → "… 2027".
pub fn festival_title(name: &str, start_year: i32) -> String {
    let name = clean_text(name);
    match name.rsplit_once(' ') {
        Some((head, year)) if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) => {
            format!("{head} {start_year}")
        }
        _ => name,
    }
}

/// A JSON-LD date as the London day it names, at London midnight.
fn london_day(s: &str) -> Option<DateTime<Utc>> {
    let day = london_date(parse_london_wall_clock(s)?);
    Some(london_to_utc(day.and_time(NaiveTime::MIN)))
}

/// Normalise the festival's JSON-LD node. Online-only festivals are skips.
pub fn normalise_payload(node: &Value) -> Result<Option<NewEvent>, SourceError> {
    let s = |k: &str| node.get(k).and_then(Value::as_str);
    if s("eventAttendanceMode").is_some_and(|m| m.ends_with("OnlineEventAttendanceMode")) {
        return Ok(None);
    }
    let name = s("name").ok_or_else(|| SourceError::Parse("Event without name".into()))?;
    let starts_at = s("startDate")
        .and_then(london_day)
        .ok_or_else(|| SourceError::Parse(format!("bad startDate in {node}")))?;
    let ends_at = s("endDate").and_then(london_day);
    let title = festival_title(name, london_date(starts_at).year());
    let venue = str_or_name(node, "location")
        .map(clean_text)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "Clerkenwell".to_string());
    let address = node
        .get("location")
        .and_then(|l| str_or_name(l, "address"))
        .map(clean_text)
        .filter(|a| !a.is_empty());

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(&venue)),
        title,
        description: clean_description(s("description")),
        venue_name: Some(venue),
        address,
        lat: None,
        lng: None,
        starts_at,
        ends_at: ends_at.filter(|e| *e > starts_at),
        all_day: true,
        price: Price::default(),
        url: Some(SITE.to_string()),
        image_url: image_url(node),
        category: Category::Expo,
        tags: vec!["design".to_string(), "festival".to_string()],
    }))
}

#[async_trait]
impl Source for ClerkenwellDesignWeek {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let html = ctx.get_text(&self.base_url).await?;
        let events = parse_home(&html);
        if events.is_empty() {
            return Err(SourceError::Parse(
                "no JSON-LD Event with a startDate on the homepage".into(),
            ));
        }
        Ok(events)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stale_year_in_the_name_follows_the_dates() {
        assert_eq!(
            festival_title("Clerkenwell Design Week 2026", 2027),
            "Clerkenwell Design Week 2027"
        );
        assert_eq!(
            festival_title("Clerkenwell Design Week", 2027),
            "Clerkenwell Design Week"
        );
    }

    #[test]
    fn utc_midnights_are_london_days() {
        let e = normalise_payload(&json!({
            "@type": "Event",
            "name": "Clerkenwell Design Week 2026",
            "startDate": "2027-05-25T00:00:00Z",
            "endDate": "2027-05-27T00:00:00Z",
        }))
        .unwrap()
        .unwrap();
        // BST: London midnight is 23:00 UTC the day before.
        assert_eq!(e.starts_at.to_rfc3339(), "2027-05-24T23:00:00+00:00");
        assert_eq!(
            e.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2027-05-26T23:00:00+00:00")
        );
        assert_eq!(e.title, "Clerkenwell Design Week 2027");
        assert_eq!(e.venue_name.as_deref(), Some("Clerkenwell"));
    }

    #[test]
    fn online_only_is_skipped_and_missing_dates_are_errors() {
        let online = json!({
            "name": "CDW Online",
            "startDate": "2027-05-25",
            "eventAttendanceMode": "https://schema.org/OnlineEventAttendanceMode",
        });
        assert!(normalise_payload(&online).unwrap().is_none());
        assert!(normalise_payload(&json!({"name": "CDW"})).is_err());
    }
}
