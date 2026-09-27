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

pub mod barbican;
pub mod chisenhale_gallery;
pub mod clerkenwell_design_week;
pub mod conway_hall;
pub mod courtauld;
pub mod dandad;
pub mod design_museum;
pub mod foundling_museum;
pub mod four_corners;
pub mod garden_museum;
pub mod goldsmiths_cca;
pub mod handel_hendrix;
pub mod headstone_manor;
pub mod horse_hospital;
pub mod hunterian_museum;
pub mod ibraaz;
pub mod jsonld;
pub mod lux;
pub mod mall_galleries;
pub mod october_gallery;
pub mod old_royal_naval_college;
pub mod royal_museums_greenwich;
pub mod serpentine;
pub mod soane_museum;
pub mod somerset_house;
pub mod tec;
pub mod ticketmaster;
pub mod two_temple_place;
pub mod whitechapel_gallery;
pub mod william_morris_gallery;
pub mod william_morris_society;

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
    #[error("invalid config: {0}")]
    InvalidConfig(String),
}

/// Build the implementation for a `events.sources` row: by its `platform`
/// when it has one (one implementation shared by many venue rows), else by
/// its `key`. Unknown keys or platforms, bad `base_url`s or `config`s and
/// missing credentials return a [`SkipReason`], which the runner records on
/// the source; a misconfigured source never blocks the others.
pub fn build(row: &SourceRow, config: &Config) -> Result<Box<dyn Source>, SkipReason> {
    let base =
        url::Url::parse(&row.base_url).map_err(|e| SkipReason::InvalidBaseUrl(e.to_string()))?;
    if let Some(platform) = row.platform.as_deref() {
        return match platform {
            tec::PLATFORM => Ok(Box::new(tec::Tec::from_row(
                &row.key,
                base,
                row.config.as_ref(),
            )?)),
            other => Err(SkipReason::InvalidConfig(format!(
                "unknown platform {other:?}"
            ))),
        };
    }
    match row.key.as_str() {
        ticketmaster::KEY => match &config.ticketmaster_api_key {
            Some(k) => Ok(Box::new(ticketmaster::Ticketmaster::new(base, k.clone()))),
            None => Err(SkipReason::MissingConfig(TICKETMASTER_API_KEY_ENV)),
        },
        barbican::KEY => Ok(Box::new(barbican::Barbican::new(base))),
        clerkenwell_design_week::KEY => Ok(Box::new(
            clerkenwell_design_week::ClerkenwellDesignWeek::new(base),
        )),
        conway_hall::KEY => Ok(Box::new(conway_hall::ConwayHall::new(base))),
        courtauld::KEY => Ok(Box::new(courtauld::Courtauld::new(base))),
        serpentine::KEY => Ok(Box::new(serpentine::Serpentine::new(base))),
        design_museum::KEY => Ok(Box::new(design_museum::DesignMuseum::new(base))),
        foundling_museum::KEY => Ok(Box::new(foundling_museum::FoundlingMuseum::new(base))),
        four_corners::KEY => Ok(Box::new(four_corners::FourCorners::new(base))),
        garden_museum::KEY => Ok(Box::new(garden_museum::GardenMuseum::new(base))),
        goldsmiths_cca::KEY => Ok(Box::new(goldsmiths_cca::GoldsmithsCca::new(base))),
        handel_hendrix::KEY => Ok(Box::new(handel_hendrix::HandelHendrix::new(base))),
        headstone_manor::KEY => Ok(Box::new(headstone_manor::HeadstoneManor::new(base))),
        horse_hospital::KEY => Ok(Box::new(horse_hospital::HorseHospital::new(base))),
        hunterian_museum::KEY => Ok(Box::new(hunterian_museum::HunterianMuseum::new(base))),
        ibraaz::KEY => Ok(Box::new(ibraaz::Ibraaz::new(base))),
        lux::KEY => Ok(Box::new(lux::Lux::new(base))),
        mall_galleries::KEY => Ok(Box::new(mall_galleries::MallGalleries::new(base))),
        october_gallery::KEY => Ok(Box::new(october_gallery::OctoberGallery::new(base))),
        old_royal_naval_college::KEY => Ok(Box::new(
            old_royal_naval_college::OldRoyalNavalCollege::new(base),
        )),
        royal_museums_greenwich::KEY => Ok(Box::new(
            royal_museums_greenwich::RoyalMuseumsGreenwich::new(base),
        )),
        soane_museum::KEY => Ok(Box::new(soane_museum::SoaneMuseum::new(base))),
        somerset_house::KEY => Ok(Box::new(somerset_house::SomersetHouse::new(base))),
        two_temple_place::KEY => Ok(Box::new(two_temple_place::TwoTemplePlace::new(base))),
        whitechapel_gallery::KEY => {
            Ok(Box::new(whitechapel_gallery::WhitechapelGallery::new(base)))
        }
        william_morris_gallery::KEY => Ok(Box::new(
            william_morris_gallery::WilliamMorrisGallery::new(base),
        )),
        william_morris_society::KEY => Ok(Box::new(
            william_morris_society::WilliamMorrisSociety::new(base),
        )),
        chisenhale_gallery::KEY => Ok(Box::new(chisenhale_gallery::ChisenhaleGallery::new(base))),
        dandad::KEY => Ok(Box::new(dandad::Dandad::new(base))),
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
            platform: None,
            config: None,
        }
    }

    fn tec_row(key: &str, config: serde_json::Value) -> SourceRow {
        SourceRow {
            platform: Some(tec::PLATFORM.into()),
            config: Some(config),
            ..row(key, "https://venue.example/")
        }
    }

    fn test_config() -> Config {
        Config {
            database_url: String::new(),
            ticketmaster_api_key: None,
            github_token: None,
            github_repo: "owner/repo".into(),
            port: 0,
            rate_limit: RateLimitConfig::disabled(),
            source_timeout: Duration::from_secs(1),
            suggestions: Default::default(),
            cors_origins: Vec::new(),
            requesty_api_key: None,
            requesty_base_url: String::new(),
            enrich: Default::default(),
            ntfy_topic: None,
            ntfy_base_url: String::new(),
        }
    }

    fn skip_reason(row: &SourceRow) -> SkipReason {
        match build(row, &test_config()) {
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

        let invalid_config = |row: &SourceRow| match skip_reason(row) {
            SkipReason::InvalidConfig(why) => why,
            other => panic!("{:?}: {other:?}", row.key),
        };
        let unknown_platform = SourceRow {
            platform: Some("nope".into()),
            ..row("some-venue", "https://example.com/")
        };
        assert_eq!(
            invalid_config(&unknown_platform),
            r#"unknown platform "nope""#
        );
        assert!(
            invalid_config(&tec_row("tec-x", serde_json::json!({"api_pth": null})))
                .contains("unknown field `api_pth`")
        );
        assert!(
            invalid_config(&tec_row(
                "tec-x",
                serde_json::json!({"default_category": "film"})
            ))
            .contains("unknown variant `film`")
        );
        assert!(
            invalid_config(&tec_row("tec-x", serde_json::json!({"api_path": null})))
                .contains("neither api_path nor list_path")
        );
    }

    #[test]
    fn platform_rows_build_under_their_own_key() {
        let source = build(
            &tec_row("tec-some-venue", serde_json::json!({})),
            &test_config(),
        )
        .unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(source.key(), "tec-some-venue");
    }
}
