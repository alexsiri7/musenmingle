//! Event sources.
//!
//! A [`Source`] fetches [`RawEvent`]s through a [`FetchContext`] (which
//! enforces User-Agent, robots.txt and rate limits) and normalises each one
//! into a [`NewEvent`]. Adding a source: see `docs/adding-a-scraper.md`.

use async_trait::async_trait;

use crate::config::Config;
use crate::fetch::{FetchContext, FetchError};
use crate::model::{NewEvent, RawEvent};
use crate::repo::SourceRow;

pub mod design_museum;
pub mod jsonld;
pub mod serpentine;
pub mod somerset_house;
pub mod ticketmaster;
pub mod whitechapel_gallery;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("configuration error: {0}")]
    Config(String),
}

/// One source of events (an API or a scraper).
#[async_trait]
pub trait Source: Send + Sync {
    /// Stable key, matching `events.sources.key`.
    fn key(&self) -> &str;

    /// Fetch raw items. All network access MUST go through `ctx`.
    /// Non-fatal per-item problems should be reported with
    /// [`FetchContext::report_error`] rather than failing the whole fetch.
    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError>;

    /// Normalise one raw item. `Ok(None)` means "not relevant, skip"
    /// (e.g. a concert from a general-purpose API); that is not an error.
    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError>;
}

/// Build the implementation for a `events.sources` row, if one exists and is
/// configured. Unknown keys and missing credentials return `Ok(None)` with a
/// warning so a misconfigured source never blocks the others.
pub fn build(row: &SourceRow, config: &Config) -> Option<Box<dyn Source>> {
    let base = match url::Url::parse(&row.base_url) {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(source = %row.key, error = %e, "invalid base_url; skipping");
            return None;
        }
    };
    match row.key.as_str() {
        ticketmaster::KEY => match &config.ticketmaster_api_key {
            Some(k) => Some(Box::new(ticketmaster::Ticketmaster::new(base, k.clone()))),
            None => {
                tracing::warn!("TICKETMASTER_API_KEY not set; skipping ticketmaster");
                None
            }
        },
        serpentine::KEY => Some(Box::new(serpentine::Serpentine::new(base))),
        design_museum::KEY => Some(Box::new(design_museum::DesignMuseum::new(base))),
        somerset_house::KEY => Some(Box::new(somerset_house::SomersetHouse::new(base))),
        whitechapel_gallery::KEY => {
            Some(Box::new(whitechapel_gallery::WhitechapelGallery::new(base)))
        }
        other => {
            tracing::warn!(source = other, "no implementation for source key; skipping");
            None
        }
    }
}
