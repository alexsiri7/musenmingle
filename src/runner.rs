//! The ingestion runner behind `thaleia-ingest`.
//!
//! One invocation (a Railway cron tick) does:
//!
//! 1. take a Postgres advisory lock so overlapping ticks never run twice;
//! 2. load enabled sources whose `interval_minutes` has elapsed;
//! 3. record a skip on any that cannot be built (no run row, no health
//!    check), and for the rest (sequentially, so per-domain politeness is trivially kept):
//!    fetch with a timeout, normalise, drop past events, upsert, and record
//!    an `events.source_runs` row (events found, errors, duration);
//! 4. run the health checker for that source;
//! 5. file GitHub issues for site suggestions the API left `pending`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::fetch::FetchContext;
use crate::health::{HealthAction, HealthChecker};
use crate::repo::{self, NewRun, SourceRow};
use crate::sources::{SkipReason, Source};
use crate::suggestions;

/// Arbitrary constant key for `pg_try_advisory_lock` (session-level locks
/// are not schema objects).
pub const INGEST_LOCK_KEY: i64 = 0x0074_6861_6c65_6961; // "thaleia"

/// Events that ended (or, without an end, started) more than this long ago
/// are skipped.
pub const PAST_GRACE: chrono::Duration = chrono::Duration::days(1);

/// Builds a source implementation for a DB row (`Err` = skip, recorded on the
/// source).
pub type SourceFactory =
    Box<dyn Fn(&SourceRow) -> Result<Box<dyn Source>, SkipReason> + Send + Sync>;

pub struct Runner {
    pub pool: PgPool,
    pub ctx: FetchContext,
    pub factory: SourceFactory,
    pub health: HealthChecker,
    pub source_timeout: Duration,
}

/// Outcome of one source within a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReport {
    pub key: String,
    pub ok: bool,
    pub events_found: i32,
    pub created: i32,
    pub skipped: i32,
    pub errors: i32,
    pub health: Option<HealthAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunSummary {
    /// Another ingest run holds the lock.
    Locked,
    Ran(Vec<SourceReport>),
}

impl Runner {
    /// Run every due source once.
    pub async fn run_once(&self, now: DateTime<Utc>) -> anyhow::Result<RunSummary> {
        let mut lock_conn = self.pool.acquire().await?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(INGEST_LOCK_KEY)
            .fetch_one(&mut *lock_conn)
            .await?;
        if !locked {
            tracing::info!("another ingest run holds the lock; exiting");
            return Ok(RunSummary::Locked);
        }
        let result = self.run_due(now).await;
        let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(INGEST_LOCK_KEY)
            .execute(&mut *lock_conn)
            .await;
        result.map(RunSummary::Ran)
    }

    async fn run_due(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<SourceReport>> {
        let due = repo::due_sources(&self.pool, now).await?;
        tracing::info!(count = due.len(), "sources due");
        let mut reports = Vec::new();
        for row in due {
            let source = match (self.factory)(&row) {
                Ok(source) => source,
                Err(reason) => {
                    tracing::warn!(source = %row.key, %reason, "source cannot run; skipping");
                    repo::record_skip(&self.pool, row.id, &reason.to_string(), now).await?;
                    continue;
                }
            };
            let mut report = self.run_source(&row, source.as_ref(), now).await?;
            report.health = match self.health.check_source(&self.pool, &row).await {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::error!(source = %row.key, error = %e, "health check failed");
                    None
                }
            };
            tracing::info!(?report, "source finished");
            reports.push(report);
        }
        if let Some(filer) = self.health.filer() {
            match suggestions::file_pending(&self.pool, filer, now).await {
                Ok(filed) if !filed.is_empty() => {
                    tracing::info!(issues = ?filed, "filed pending site suggestions")
                }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "filing pending site suggestions failed"),
            }
        } else {
            tracing::warn!(
                "no GitHub filer configured; pending site suggestions will not be filed"
            );
        }
        Ok(reports)
    }

    /// Run one source and record its `source_runs` row.
    pub async fn run_source(
        &self,
        row: &SourceRow,
        source: &dyn Source,
        now: DateTime<Utc>,
    ) -> anyhow::Result<SourceReport> {
        let started_at = Utc::now();
        let _ = self.ctx.take_errors();
        let mut errors: Vec<String> = Vec::new();
        let (mut found, mut created, mut skipped) = (0i32, 0i32, 0i32);

        let fetched = tokio::time::timeout(self.source_timeout, source.fetch(&self.ctx)).await;
        let ok = match fetched {
            Err(_) => {
                errors.push(format!("timed out after {:?}", self.source_timeout));
                false
            }
            Ok(Err(e)) => {
                errors.push(format!("fetch failed: {e}"));
                false
            }
            Ok(Ok(raws)) => {
                for raw in &raws {
                    match source.normalise(raw) {
                        Ok(None) => skipped += 1,
                        Ok(Some(ev)) => {
                            let last = ev.ends_at.unwrap_or(ev.starts_at);
                            if last < now - PAST_GRACE {
                                skipped += 1;
                                continue;
                            }
                            match repo::upsert_event(&self.pool, row.id, &ev, raw).await {
                                Ok(o) => {
                                    found += 1;
                                    created += i32::from(o.created);
                                }
                                Err(e) => {
                                    errors.push(format!("upsert {}: {e}", raw.source_event_id))
                                }
                            }
                        }
                        Err(e) => errors.push(format!("normalise {}: {e}", raw.source_event_id)),
                    }
                }
                true
            }
        };
        errors.extend(self.ctx.take_errors());

        let error_summary = if errors.is_empty() {
            None
        } else {
            let mut s = errors
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            if errors.len() > 10 {
                s.push_str(&format!("\n… and {} more", errors.len() - 10));
            }
            Some(s)
        };
        let n_errors = i32::try_from(errors.len()).unwrap_or(i32::MAX);
        repo::record_run(
            &self.pool,
            &NewRun {
                source_id: row.id,
                started_at,
                finished_at: Utc::now(),
                events_found: found,
                errors: n_errors,
                error_summary,
                ok,
            },
        )
        .await?;
        Ok(SourceReport {
            key: row.key.clone(),
            ok,
            events_found: found,
            created,
            skipped,
            errors: n_errors,
            health: None,
        })
    }
}
