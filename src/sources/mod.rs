//! Event sources.
//!
//! A [`Source`] fetches [`RawEvent`]s through a [`FetchContext`] (which
//! enforces User-Agent, robots.txt and rate limits) and normalises each one
//! into a [`NewEvent`]. Adding a source: see `docs/adding-a-scraper.md`.

use async_trait::async_trait;

use crate::config::{Config, TICKETMASTER_API_KEY_ENV};
use crate::fetch::{FetchContext, FetchError};
use crate::model::{NewEvent, RawEvent};
use crate::repo::SourceRow;

pub mod artrabbit;
pub mod barbican;
pub mod chisenhale_gallery;
pub mod courtauld;
pub mod design_museum;
pub mod garden_museum;
pub mod goldsmiths_cca;
pub mod jsonld;
pub mod mall_galleries;
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

/// Why a `events.sources` row could not be turned into a [`Source`].
/// Never contains secret values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkipReason {
    #[error("{0} not set")]
    MissingConfig(&'static str),
    #[error("invalid base_url: {0}")]
    InvalidBaseUrl(String),
    #[error("no implementation for this source key")]
    UnknownKey,
}

/// Build the implementation for a `events.sources` row. Unknown keys, bad
/// `base_url`s and missing credentials return a [`SkipReason`], which the
/// runner records on the source; a misconfigured source never blocks the
/// others.
pub fn build(row: &SourceRow, config: &Config) -> Result<Box<dyn Source>, SkipReason> {
    let base =
        url::Url::parse(&row.base_url).map_err(|e| SkipReason::InvalidBaseUrl(e.to_string()))?;
    match row.key.as_str() {
        ticketmaster::KEY => match &config.ticketmaster_api_key {
            Some(k) => Ok(Box::new(ticketmaster::Ticketmaster::new(base, k.clone()))),
            None => Err(SkipReason::MissingConfig(TICKETMASTER_API_KEY_ENV)),
        },
        artrabbit::KEY => Ok(Box::new(artrabbit::ArtRabbit::new(base))),
        barbican::KEY => Ok(Box::new(barbican::Barbican::new(base))),
        courtauld::KEY => Ok(Box::new(courtauld::Courtauld::new(base))),
        serpentine::KEY => Ok(Box::new(serpentine::Serpentine::new(base))),
        design_museum::KEY => Ok(Box::new(design_museum::DesignMuseum::new(base))),
        garden_museum::KEY => Ok(Box::new(garden_museum::GardenMuseum::new(base))),
        goldsmiths_cca::KEY => Ok(Box::new(goldsmiths_cca::GoldsmithsCca::new(base))),
        mall_galleries::KEY => Ok(Box::new(mall_galleries::MallGalleries::new(base))),
        somerset_house::KEY => Ok(Box::new(somerset_house::SomersetHouse::new(base))),
        whitechapel_gallery::KEY => {
            Ok(Box::new(whitechapel_gallery::WhitechapelGallery::new(base)))
        }
        chisenhale_gallery::KEY => Ok(Box::new(chisenhale_gallery::ChisenhaleGallery::new(base))),
        _ => Err(SkipReason::UnknownKey),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::RateLimitConfig;
    use crate::model::SourceKind;

    fn row(key: &str, base_url: &str) -> SourceRow {
        SourceRow {
            id: 1,
            key: key.into(),
            kind: SourceKind::Api,
            base_url: base_url.into(),
            domain: "example.com".into(),
            interval_minutes: 60,
            enabled: true,
            last_run_at: None,
        }
    }

    fn skip_reason(row: &SourceRow) -> SkipReason {
        let config = Config {
            database_url: String::new(),
            ticketmaster_api_key: None,
            github_token: None,
            github_repo: "owner/repo".into(),
            port: 0,
            rate_limit: RateLimitConfig::disabled(),
            source_timeout: Duration::from_secs(1),
            suggestions: Default::default(),
            cors_origins: Vec::new(),
        };
        match build(row, &config) {
            Ok(_) => panic!("{:?} unexpectedly built", row.key),
            Err(reason) => reason,
        }
    }

    #[test]
    fn unbuildable_rows_say_why() {
        let missing_key = skip_reason(&row(ticketmaster::KEY, "https://app.ticketmaster.com/"));
        assert_eq!(
            missing_key,
            SkipReason::MissingConfig(TICKETMASTER_API_KEY_ENV)
        );
        assert_eq!(missing_key.to_string(), "TICKETMASTER_API_KEY not set");

        assert_eq!(
            skip_reason(&row("nope", "https://example.com/")),
            SkipReason::UnknownKey
        );
        assert!(matches!(
            skip_reason(&row(barbican::KEY, "not a url")),
            SkipReason::InvalidBaseUrl(_)
        ));
    }
}
