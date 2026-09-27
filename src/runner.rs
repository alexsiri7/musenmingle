//! The ingestion runner behind `musenmingle-ingest`.
//!
//! One invocation (a Railway cron tick) does:
//!
//! 1. take a Postgres advisory lock so overlapping ticks never run twice;
//! 2. load enabled sources whose `interval_minutes` has elapsed;
//! 3. record a skip on any that cannot be built (no run row, no health
//!    check), and for the rest (sequentially, so per-domain politeness is trivially kept):
//!    fetch with a timeout, normalise, drop past events, upsert, and record
//!    an `events.source_runs` row (events found, errors, duration);
//! 4. run the health checker for that source, then the scraper QA rules on
//!    its normalised events (`crate::qa::rules`) and, when the source is due
//!    one, the AI check of the pages its run fetched (`crate::qa`; page
//!    capture is on only for that run);
//! 5. bring stored rows in line with the sources' content policy
//!    (`repo::enforce_content_policy`) and make missing thumbnails
//!    (`crate::thumbs`) — every tick, even when no source was due;
//! 6. keep AI fields in step with the stored facts (`crate::enrich::sync`)
//!    and, when a Requesty key is configured, run the AI enrichment and
//!    embedding pass within its spend caps (`crate::enrich`);
//! 7. file GitHub issues for site suggestions and contact requests the API
//!    left pending, within the forms' daily cap, and send the owner's daily
//!    digest while the cap holds some back (`crate::issue_cap`).

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::fetch::FetchContext;
use crate::health::{HealthAction, HealthChecker, RunStats};
use crate::model::NewEvent;
use crate::qa::rules::{self, Finding, RuleEvent};
use crate::repo::{self, NewRun, SourceRow};
use crate::sources::{SkipReason, Source};
use crate::suggestions;
use crate::thumbs::{self, ThumbConfig};

/// Arbitrary constant key for `pg_try_advisory_lock` (session-level locks
/// are not schema objects). The value spells "thaleia", the project's former
/// name; it was deliberately kept by the rename to Muse & Mingle, because a new
/// key would let an old and a new ingest run overlap while a deploy rolls out.
pub const INGEST_LOCK_KEY: i64 = 0x0074_6861_6c65_6961;

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
    /// AI enrichment + embeddings (`None` without `REQUESTY_API_KEY`).
    pub enrich: Option<crate::enrich::Enricher>,
    /// Scraper QA AI checks (`None` without `REQUESTY_API_KEY` or with
    /// `QA_MAX_CHECKS_PER_RUN=0`); the rules run regardless.
    pub qa: Option<crate::qa::QaChecker>,
    /// Venue geocoding and the venue-location alert (#204); `None` skips
    /// both (the venue sync still runs).
    pub venues: Option<crate::geocode::VenueChecks>,
    /// The daily cap on GitHub issues filed for the site's forms.
    pub form_issues: crate::issue_cap::FormIssueCap,
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
    /// Scraper QA rules hit by this run.
    pub qa_findings: usize,
    /// Status of the scraper QA check made after this run, if any.
    pub qa_check: Option<String>,
}

/// A normalised event of a run, for the scraper QA rules and check.
#[derive(Debug, Clone, PartialEq)]
pub struct RunEvent {
    pub source_event_id: String,
    pub source_url: Option<String>,
    pub event: NewEvent,
    /// Stored (false: dropped as past, or the upsert failed).
    pub kept: bool,
}

/// A source's run: its report, `events.source_runs` id and events.
#[derive(Debug, Clone)]
pub struct SourceRun {
    pub report: SourceReport,
    pub run_id: i64,
    pub events: Vec<RunEvent>,
    /// Page texts of the stored listings, by event, for this ingest run's
    /// AI enrichment (issue #208; transient, never stored or logged).
    pub page_texts: crate::enrich::PageTexts,
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
        let mut qa_tick = crate::qa::QaTick::default();
        // Held only until this run's enrichment pass, then dropped.
        let mut page_texts = crate::enrich::PageTexts::new();
        for row in due {
            let source = match (self.factory)(&row) {
                Ok(source) => source,
                Err(reason) => {
                    tracing::warn!(source = %row.key, %reason, "source cannot run; skipping");
                    repo::record_skip(&self.pool, row.id, &reason.to_string(), now).await?;
                    continue;
                }
            };
            let qa_due = match &self.qa {
                Some(qa) => qa
                    .plan(&self.pool, &row, source.as_ref(), now, &qa_tick)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!(source = %row.key, error = %e, "planning the scraper check failed");
                        None
                    }),
                None => None,
            };
            if qa_due.is_some() {
                self.ctx.start_capture();
            }
            let run = self.run_source(&row, source.as_ref(), now).await;
            let captured = self.ctx.finish_capture();
            let mut run = run?;
            run.report.health = match self.health.check_source(&self.pool, &row).await {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::error!(source = %row.key, error = %e, "health check failed");
                    None
                }
            };
            if run.report.ok {
                let findings = self.qa_rules(&row, &run, now).await;
                run.report.qa_findings = findings.len();
                if let (Some(qa), Some(due)) = (&self.qa, qa_due) {
                    let reason = due.reason.as_str();
                    match qa
                        .check(
                            &self.pool,
                            &self.ctx,
                            &row,
                            &run,
                            captured,
                            &findings,
                            due,
                            self.health.filer(),
                            now,
                            &mut qa_tick,
                        )
                        .await
                    {
                        Ok(o) => {
                            tracing::info!(
                                source = %row.key,
                                reason,
                                status = o.status,
                                wrong = o.wrong,
                                missed = o.missed,
                                cost_usd = crate::enrich::usd(o.cost_usd),
                                issue = ?o.issue,
                                "scraper check finished"
                            );
                            run.report.qa_check = Some(o.status.to_string());
                        }
                        Err(e) => {
                            tracing::error!(source = %row.key, error = %e, "scraper check failed")
                        }
                    }
                }
            }
            page_texts.extend(std::mem::take(&mut run.page_texts));
            let report = run.report;
            tracing::info!(?report, "source finished");
            reports.push(report);
        }
        match repo::enforce_content_policy(&self.pool).await {
            Ok(r) if r != repo::PolicyReport::default() => {
                tracing::info!(report = ?r, "content policy applied to stored rows")
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "content policy pass failed"),
        }
        match thumbs::run(&self.pool, &self.ctx, &ThumbConfig::default(), now).await {
            Ok(r) => tracing::info!(
                made = r.made,
                failed = r.failed,
                deferred = r.deferred,
                "thumbnail pass finished"
            ),
            Err(e) => tracing::error!(error = %e, "thumbnail pass failed"),
        }
        // Thumbnail fetches may report soft errors; they are not source errors.
        let _ = self.ctx.take_errors();
        match crate::enrich::sync(&self.pool, now).await {
            Ok(r) if r != crate::enrich::SyncReport::default() => {
                tracing::info!(report = ?r, "AI fields synced with stored facts")
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "AI field sync failed"),
        }
        if let Some(enricher) = &self.enrich {
            match enricher.run(&self.pool, now, &page_texts).await {
                Ok(r) => tracing::info!(
                    model = %enricher.config.model,
                    queued = r.queued,
                    enriched = r.enriched,
                    failed = r.failed,
                    calls = r.calls,
                    tokens_in = r.tokens_in,
                    tokens_out = r.tokens_out,
                    cost_usd = crate::enrich::usd(r.cost_usd),
                    spent_today_usd = crate::enrich::usd(r.spent_today_usd),
                    embedded = r.embedded,
                    stopped = ?r.stopped,
                    "enrichment pass finished"
                ),
                Err(e) => tracing::error!(error = %e, "enrichment pass failed"),
            }
        }
        if let Some(filer) = self.health.filer() {
            let per_day = self.form_issues.per_day;
            let mut capped = false;
            match suggestions::file_pending(&self.pool, filer, now, per_day).await {
                Ok((filed, held)) => {
                    capped |= held;
                    if !filed.is_empty() {
                        tracing::info!(issues = ?filed, "filed pending site suggestions");
                    }
                }
                Err(e) => tracing::error!(error = %e, "filing pending site suggestions failed"),
            }
            match crate::contact::file_pending(&self.pool, filer, now, per_day).await {
                Ok((delivered, held)) => {
                    capped |= held;
                    if delivered > 0 {
                        tracing::info!(count = delivered, "filed pending contact requests");
                    }
                }
                Err(e) => tracing::error!(error = %e, "filing pending contact requests failed"),
            }
            self.form_issues.digest(&self.pool, now, capped).await;
        } else {
            tracing::warn!(
                "no GitHub filer configured; pending site suggestions and contact requests will not be filed"
            );
        }
        // Venues first: it fills events' missing coordinates, which the
        // borough sync then reads.
        match repo::sync_venues(&self.pool).await {
            Ok(r) if r == repo::VenueSync::default() => {}
            Ok(r) => tracing::info!(
                created = r.created,
                updated = r.updated,
                linked = r.linked,
                located = r.located,
                "venues synced"
            ),
            Err(e) => tracing::error!(error = %e, "syncing venues failed"),
        }
        if let Some(checks) = &self.venues {
            match checks.run(&self.pool, &self.ctx, now).await {
                Ok(r) => tracing::info!(
                    geocoded = r.geocoded,
                    not_found = r.not_found,
                    failed = r.failed,
                    missing = r.missing.len(),
                    "venue locations checked"
                ),
                Err(e) => tracing::error!(error = %e, "checking venue locations failed"),
            }
        }
        match repo::sync_venue_types(&self.pool).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(count = n, "venue types updated"),
            Err(e) => tracing::error!(error = %e, "updating venue types failed"),
        }
        match repo::sync_boroughs(&self.pool).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(count = n, "boroughs updated"),
            Err(e) => tracing::error!(error = %e, "updating boroughs failed"),
        }
        match repo::sync_music_tags(&self.pool).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(count = n, "music subtags updated"),
            Err(e) => tracing::error!(error = %e, "updating music subtags failed"),
        }
        Ok(reports)
    }

    /// Apply the scraper QA rules to a successful run and store their hits
    /// (errors are logged: they never fail the tick).
    async fn qa_rules(&self, row: &SourceRow, run: &SourceRun, now: DateTime<Utc>) -> Vec<Finding> {
        let events: Vec<RuleEvent> = run
            .events
            .iter()
            .map(|r| RuleEvent {
                source_event_id: r.source_event_id.clone(),
                title: r.event.title.clone(),
                starts_at: r.event.starts_at,
                ends_at: r.event.ends_at,
                all_day: r.event.all_day,
                venue_missing: r
                    .event
                    .venue_name
                    .as_deref()
                    .is_none_or(|v| v.trim().is_empty()),
                coords_missing: r.event.lat.is_none() || r.event.lng.is_none(),
                kept: r.kept,
            })
            .collect();
        match self.store_rules(row, run.run_id, &events, now).await {
            Ok(findings) => {
                for f in &findings {
                    tracing::warn!(
                        source = %row.key,
                        rule = f.rule.as_str(),
                        affected = f.affected,
                        detail = %f.detail,
                        "QA rule hit"
                    );
                }
                findings
            }
            Err(e) => {
                tracing::error!(source = %row.key, error = %e, "scraper QA rules failed");
                Vec::new()
            }
        }
    }

    async fn store_rules(
        &self,
        row: &SourceRow,
        run_id: i64,
        events: &[RuleEvent],
        now: DateTime<Utc>,
    ) -> sqlx::Result<Vec<Finding>> {
        let history = crate::qa::store::run_history(&self.pool, row.id, run_id, 5).await?;
        let runs = repo::recent_runs(&self.pool, row.id, 10).await?;
        let stats: Vec<RunStats> = runs.iter().map(RunStats::from).collect();
        let findings = rules::evaluate(events, &history, &stats, now);
        crate::qa::store::record_run_stats(&self.pool, run_id, rules::RunCounts::of(events))
            .await?;
        crate::qa::store::insert_findings(&self.pool, run_id, row.id, &findings).await?;
        Ok(findings)
    }

    /// Run one source and record its `source_runs` row.
    pub async fn run_source(
        &self,
        row: &SourceRow,
        source: &dyn Source,
        now: DateTime<Utc>,
    ) -> anyhow::Result<SourceRun> {
        let started_at = Utc::now();
        let _ = self.ctx.take_errors();
        let mut errors: Vec<String> = Vec::new();
        let (mut found, mut created, mut skipped) = (0i32, 0i32, 0i32);
        let mut events: Vec<RunEvent> = Vec::new();
        let mut page_texts = crate::enrich::PageTexts::new();

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
                            let run_event = |kept| RunEvent {
                                source_event_id: raw.source_event_id.clone(),
                                source_url: raw.source_url.clone(),
                                event: ev.clone(),
                                kept,
                            };
                            let last = ev.ends_at.unwrap_or(ev.starts_at);
                            if last < now - PAST_GRACE {
                                skipped += 1;
                                events.push(run_event(false));
                                continue;
                            }
                            match repo::upsert_listing(&self.pool, row.id, &ev, raw).await {
                                Ok((o, page)) => {
                                    if let Some(page) = page {
                                        page_texts.insert(o.event_id, page);
                                    }
                                    found += 1;
                                    created += i32::from(o.created);
                                    events.push(run_event(true));
                                }
                                Err(e) => {
                                    errors.push(format!("upsert {}: {e}", raw.source_event_id));
                                    events.push(run_event(false));
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
        let run_id = repo::record_run(
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
        Ok(SourceRun {
            report: SourceReport {
                key: row.key.clone(),
                ok,
                events_found: found,
                created,
                skipped,
                errors: n_errors,
                health: None,
                qa_findings: 0,
                qa_check: None,
            },
            run_id,
            events,
            page_texts,
        })
    }
}
