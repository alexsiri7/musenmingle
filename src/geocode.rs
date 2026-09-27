//! Venue locations (issue #204): after each ingest run, [`VenueChecks`]
//! geocodes venues without coordinates from their UK postcode on
//! [postcodes.io](https://postcodes.io) (ONS Postcode Directory, Open
//! Government Licence, credited on `/about`), through [`FetchContext`]
//! (robots.txt, User-Agent, rate limit), then raises an owner alert (ntfy,
//! at most once a day) while any venue with upcoming events still has no
//! coordinates, and a notice when that clears. No Google, no AI.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use url::Url;

use crate::enrich::store;
use crate::fetch::{FetchContext, FetchError};
use crate::notify::Notifier;
use crate::repo;

pub const DEFAULT_POSTCODES_IO_BASE: &str = "https://api.postcodes.io";

/// `events.alert_state` key for venues without coordinates.
pub const ALERT: &str = "venues-without-coordinates";
const ALERT_TITLE: &str = "Muse & Mingle: venues without a location";
const ALERT_OK_TITLE: &str = "Muse & Mingle: every venue has a location";

/// Postcode lookups per run (the first run after deploy does the backlog
/// over a few runs; later runs see one or two new venues).
pub const MAX_LOOKUPS_PER_RUN: usize = 40;

/// How long a postcode that found nothing waits before it is tried again.
pub fn retry_after() -> Duration {
    Duration::days(7)
}

#[derive(Deserialize)]
struct Lookup {
    result: Option<LookupResult>,
}

#[derive(Deserialize)]
struct LookupResult {
    latitude: Option<f64>,
    longitude: Option<f64>,
}

/// The centroid of a UK postcode (`SE5 8UH`) on postcodes.io at `base`:
/// a live postcode, else a terminated one; `None` when neither knows it or
/// it has no location.
pub async fn postcode_point(
    ctx: &FetchContext,
    base: &Url,
    postcode: &str,
) -> Result<Option<(f64, f64)>, FetchError> {
    // postcodes.io takes the postcode with or without its space; without
    // keeps the URL plain.
    let compact: String = postcode.split_whitespace().collect();
    for path in ["postcodes", "terminated_postcodes"] {
        let mut url = base.clone();
        url.path_segments_mut()
            .map_err(|_| FetchError::InvalidUrl(base.to_string()))?
            .pop_if_empty()
            .extend([path, compact.as_str()]);
        match ctx.get_json::<Lookup>(&url).await {
            Ok(l) => {
                return Ok(l
                    .result
                    .and_then(|r| r.latitude.zip(r.longitude))
                    .filter(|(lat, lng)| lat.is_finite() && lng.is_finite()));
            }
            Err(FetchError::Status { status, .. }) if status.as_u16() == 404 => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// What one [`VenueChecks::run`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VenueCheckReport {
    pub geocoded: usize,
    pub not_found: usize,
    pub failed: usize,
    /// Venues with upcoming events still without coordinates.
    pub missing: Vec<String>,
}

/// Geocoding and the coordinates alert, run after each ingest run.
pub struct VenueChecks {
    pub postcodes_base: Url,
    pub notifier: Box<dyn Notifier>,
}

impl VenueChecks {
    pub async fn run(
        &self,
        pool: &PgPool,
        ctx: &FetchContext,
        now: DateTime<Utc>,
    ) -> sqlx::Result<VenueCheckReport> {
        let mut report = VenueCheckReport::default();
        let todo = repo::venues_to_geocode(pool, now - retry_after(), MAX_LOOKUPS_PER_RUN).await?;
        let date = crate::normalise::london_date(now).format("%Y-%m-%d");
        for (id, postcode) in &todo {
            match postcode_point(ctx, &self.postcodes_base, postcode).await {
                Ok(Some((lat, lng))) => {
                    let source = format!("postcodes.io {postcode} {date}");
                    repo::set_venue_point(pool, *id, lat, lng, &source).await?;
                    report.geocoded += 1;
                }
                Ok(None) => {
                    repo::venue_geocode_checked(pool, *id, now).await?;
                    report.not_found += 1;
                }
                Err(e) => {
                    tracing::warn!(venue_id = id, error = %e, "postcode lookup failed");
                    report.failed += 1;
                }
            }
        }
        if report.geocoded > 0 {
            // Hand the new points to the venues' events and set boroughs.
            repo::sync_venues(pool).await?;
        }
        report.missing = repo::venues_without_coords(pool, now).await?;
        self.alert(pool, &report.missing, now).await;
        Ok(report)
    }

    async fn alert(&self, pool: &PgPool, missing: &[String], now: DateTime<Utc>) {
        if missing.is_empty() {
            match store::alert_clear(pool, ALERT, now).await {
                Ok(Some(_)) => {
                    if let Err(e) = self
                        .notifier
                        .notify(
                            ALERT_OK_TITLE,
                            "Every venue with upcoming events has coordinates again.",
                            "default",
                        )
                        .await
                    {
                        tracing::error!(error = %e, "venue-location OK notification failed");
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::error!(error = %e, "clearing the venue-location alert failed"),
            }
            return;
        }
        let body = alert_body(missing);
        let state = match store::alert_raise(pool, ALERT, &body, now).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "recording the venue-location alert failed");
                return;
            }
        };
        let today = crate::normalise::london_date(now);
        let due = state
            .last_notified_at
            .is_none_or(|t| crate::normalise::london_date(t) != today);
        tracing::warn!(
            count = missing.len(),
            notify = due,
            "venues without coordinates"
        );
        if !due {
            return;
        }
        match self.notifier.notify(ALERT_TITLE, &body, "default").await {
            Ok(()) => {
                if let Err(e) = store::alert_notified(pool, ALERT, now).await {
                    tracing::error!(error = %e, "recording the venue-location notification failed");
                }
            }
            Err(e) => tracing::error!(error = %e, "venue-location notification failed"),
        }
    }
}

/// The alert text: how many, and the first few names.
pub fn alert_body(missing: &[String]) -> String {
    const SHOWN: usize = 10;
    let mut names = missing
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if missing.len() > SHOWN {
        names.push_str(&format!(" and {} more", missing.len() - SHOWN));
    }
    format!(
        "{} venue(s) with upcoming events have no coordinates and no postcode we could look up: \
         {names}. Add their points (or an alias) in a migration.",
        missing.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_body_lists_the_first_ten() {
        let names: Vec<String> = (1..=12).map(|i| format!("V{i}")).collect();
        let body = alert_body(&names);
        assert!(body.starts_with("12 venue(s)"), "{body}");
        assert!(body.contains("V10 and 2 more"), "{body}");
        assert!(!body.contains("V11"), "{body}");
    }
}
