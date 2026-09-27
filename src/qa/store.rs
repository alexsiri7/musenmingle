//! SQL for scraper QA (all in schema `events`).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::{FromRow, PgPool};

use super::rules::{Finding, RunCounts};
use crate::enrich::store::LedgerPass;

/// Statuses of a check that ran to a conclusion; `failed` is retried.
pub const COMPLETED: &[&str] = &["ok", "issues", "no_pages", "invalid"];

pub async fn record_run_stats(pool: &PgPool, run_id: i64, c: RunCounts) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE events.source_runs
         SET events_checked = $2, missing_venue = $3, missing_coords = $4 WHERE id = $1",
    )
    .bind(run_id)
    .bind(c.events_checked)
    .bind(c.missing_venue)
    .bind(c.missing_coords)
    .execute(pool)
    .await?;
    Ok(())
}

/// Counts of the source's runs other than `run_id` that have them, newest
/// first.
pub async fn run_history(
    pool: &PgPool,
    source_id: i64,
    run_id: i64,
    limit: i64,
) -> sqlx::Result<Vec<RunCounts>> {
    let rows: Vec<(i32, i32, i32)> = sqlx::query_as(
        "SELECT events_checked, missing_venue, missing_coords FROM events.source_runs
         WHERE source_id = $1 AND id <> $2 AND events_checked IS NOT NULL
         ORDER BY started_at DESC, id DESC LIMIT $3",
    )
    .bind(source_id)
    .bind(run_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(events_checked, missing_venue, missing_coords)| RunCounts {
                events_checked,
                missing_venue,
                missing_coords,
            },
        )
        .collect())
}

pub async fn insert_findings(
    pool: &PgPool,
    run_id: i64,
    source_id: i64,
    findings: &[Finding],
) -> sqlx::Result<()> {
    for f in findings {
        sqlx::query(
            "INSERT INTO events.qa_findings (run_id, source_id, rule, affected, detail, examples)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(run_id)
        .bind(source_id)
        .bind(f.rule.as_str())
        .bind(f.affected)
        .bind(&f.detail)
        .bind(sqlx::types::Json(&f.examples))
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// The rules hit by the source's latest run that the rules looked at.
pub async fn latest_run_rules(pool: &PgPool, source_id: i64) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT f.rule FROM events.qa_findings f
         WHERE f.run_id = (SELECT id FROM events.source_runs
                           WHERE source_id = $1 AND events_checked IS NOT NULL
                           ORDER BY started_at DESC, id DESC LIMIT 1)
         ORDER BY f.rule",
    )
    .bind(source_id)
    .fetch_all(pool)
    .await
}

/// The latest check of a source that ran to a conclusion.
#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct LatestCheck {
    pub checked_at: DateTime<Utc>,
    pub code_hash: String,
    pub rules_hit: Vec<String>,
}

pub async fn latest_check(pool: &PgPool, source_id: i64) -> sqlx::Result<Option<LatestCheck>> {
    sqlx::query_as(
        "SELECT checked_at, code_hash, rules_hit FROM events.qa_checks
         WHERE source_id = $1 AND status = ANY($2)
         ORDER BY checked_at DESC, id DESC LIMIT 1",
    )
    .bind(source_id)
    .bind(COMPLETED)
    .fetch_optional(pool)
    .await
}

/// Recorded QA spend since `since` (its own budget: `QA_DAILY_CAP_USD`).
pub async fn qa_spent_since(pool: &PgPool, since: DateTime<Utc>) -> sqlx::Result<Decimal> {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(cost_usd), 0) FROM events.enrichment_calls
         WHERE called_at >= $1 AND pass = $2",
    )
    .bind(since)
    .bind(LedgerPass::Qa.as_str())
    .fetch_one(pool)
    .await
}

/// A `events.qa_checks` row about to be stored.
#[derive(Debug, Clone)]
pub struct NewCheck<'a> {
    pub source_id: i64,
    pub run_id: i64,
    pub checked_at: DateTime<Utc>,
    pub reason: &'a str,
    pub code_hash: &'a str,
    pub rules_hit: &'a [String],
    pub pages: Value,
    pub model: &'a str,
    pub prompt_version: i32,
    pub cost_usd: Decimal,
    pub status: &'a str,
    pub wrong_fields: i32,
    pub missed_events: i32,
    pub verdict: Option<Value>,
    pub error: Option<String>,
}

pub async fn insert_check(pool: &PgPool, c: &NewCheck<'_>) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO events.qa_checks
             (source_id, run_id, checked_at, reason, code_hash, rules_hit, pages, model,
              prompt_version, cost_usd, status, wrong_fields, missed_events, verdict, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
         RETURNING id",
    )
    .bind(c.source_id)
    .bind(c.run_id)
    .bind(c.checked_at)
    .bind(c.reason)
    .bind(c.code_hash)
    .bind(c.rules_hit)
    .bind(sqlx::types::Json(&c.pages))
    .bind(c.model)
    .bind(c.prompt_version)
    .bind(c.cost_usd.round_dp(6))
    .bind(c.status)
    .bind(c.wrong_fields)
    .bind(c.missed_events)
    .bind(c.verdict.as_ref().map(sqlx::types::Json))
    .bind(
        c.error
            .as_ref()
            .map(|e| e.chars().take(1000).collect::<String>()),
    )
    .fetch_one(pool)
    .await
}

pub async fn set_check_issue(pool: &PgPool, check_id: i64, number: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE events.qa_checks SET github_issue_number = $2 WHERE id = $1")
        .bind(check_id)
        .bind(number)
        .execute(pool)
        .await?;
    Ok(())
}

/// The open QA issue of a source: `(row id, GitHub issue number)`.
pub async fn open_issue(pool: &PgPool, source_id: i64) -> sqlx::Result<Option<(i64, i64)>> {
    sqlx::query_as(
        "SELECT id, github_issue_number FROM events.qa_issues
         WHERE source_id = $1 AND closed_at IS NULL",
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
}

pub async fn insert_issue(pool: &PgPool, source_id: i64, number: i64) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO events.qa_issues (source_id, github_issue_number) VALUES ($1, $2)")
        .bind(source_id)
        .bind(number)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn close_issue(pool: &PgPool, id: i64, now: DateTime<Utc>) -> sqlx::Result<()> {
    sqlx::query("UPDATE events.qa_issues SET closed_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}
