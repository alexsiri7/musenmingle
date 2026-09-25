//! Database access. All SQL is schema-qualified with `events.`.
//!
//! Runtime-checked queries only (`sqlx::query*`), no compile-time macros, so
//! builds never need a live database.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{AssertSqlSafe, FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::model::{NewEvent, RawEvent};

#[derive(Debug, Clone, FromRow)]
pub struct SourceRow {
    pub id: i64,
    pub key: String,
    pub kind: String,
    pub base_url: String,
    pub domain: String,
    pub interval_minutes: i32,
    pub enabled: bool,
    pub last_run_at: Option<DateTime<Utc>>,
}

const SOURCE_COLS: &str = "id, key, kind, base_url, domain, interval_minutes, enabled, last_run_at";

/// Enabled sources whose interval has elapsed at `now` (or that never ran).
pub async fn due_sources(pool: &PgPool, now: DateTime<Utc>) -> sqlx::Result<Vec<SourceRow>> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {SOURCE_COLS} FROM events.sources
         WHERE enabled
           AND (last_run_at IS NULL
                OR last_run_at + make_interval(mins => interval_minutes) <= $1)
         ORDER BY key"
    )))
    .bind(now)
    .fetch_all(pool)
    .await
}

pub async fn source_by_key(pool: &PgPool, key: &str) -> sqlx::Result<Option<SourceRow>> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {SOURCE_COLS} FROM events.sources WHERE key = $1"
    )))
    .bind(key)
    .fetch_optional(pool)
    .await
}

/// Insert or update a source definition (used by tests and tooling).
pub async fn upsert_source(
    pool: &PgPool,
    key: &str,
    kind: &str,
    base_url: &str,
    interval_minutes: i32,
    enabled: bool,
) -> sqlx::Result<SourceRow> {
    let domain = url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    sqlx::query_as(AssertSqlSafe(format!(
        "INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (key) DO UPDATE SET kind = EXCLUDED.kind, base_url = EXCLUDED.base_url,
             domain = EXCLUDED.domain, interval_minutes = EXCLUDED.interval_minutes,
             enabled = EXCLUDED.enabled
         RETURNING {SOURCE_COLS}"
    )))
    .bind(key)
    .bind(kind)
    .bind(base_url)
    .bind(domain)
    .bind(interval_minutes)
    .bind(enabled)
    .fetch_one(pool)
    .await
}

/// Result of [`upsert_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpsertOutcome {
    pub event_id: Uuid,
    /// A new `events.events` row was created.
    pub created: bool,
}

/// Columns bound in the same order by [`bind_event`].
const EVENT_INSERT: &str = "INSERT INTO events.events (
        title, description, venue_name, address, lat, lng, starts_at, ends_at,
        is_free, price_min, price_max, currency, url, image_url, category, tags, dedupe_key)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)";

type PgQuery<'q> = sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments>;
type PgQueryAs<'q, O> = sqlx::query::QueryAs<'q, Postgres, O, sqlx::postgres::PgArguments>;

fn bind_event<'q>(q: PgQuery<'q>, e: &'q NewEvent) -> PgQuery<'q> {
    q.bind(&e.title)
        .bind(&e.description)
        .bind(&e.venue_name)
        .bind(&e.address)
        .bind(e.lat)
        .bind(e.lng)
        .bind(e.starts_at)
        .bind(e.ends_at)
        .bind(e.price.is_free)
        .bind(e.price.min)
        .bind(e.price.max)
        .bind(&e.price.currency)
        .bind(&e.url)
        .bind(&e.image_url)
        .bind(e.category.as_str())
        .bind(&e.tags)
        .bind(&e.dedupe_key)
}

fn bind_event_as<'q, O>(q: PgQueryAs<'q, O>, e: &'q NewEvent) -> PgQueryAs<'q, O> {
    q.bind(&e.title)
        .bind(&e.description)
        .bind(&e.venue_name)
        .bind(&e.address)
        .bind(e.lat)
        .bind(e.lng)
        .bind(e.starts_at)
        .bind(e.ends_at)
        .bind(e.price.is_free)
        .bind(e.price.min)
        .bind(e.price.max)
        .bind(&e.price.currency)
        .bind(&e.url)
        .bind(&e.image_url)
        .bind(e.category.as_str())
        .bind(&e.tags)
        .bind(&e.dedupe_key)
}

/// Merge used when a DIFFERENT source reports an event we already have:
/// existing values win, the newcomer only fills gaps; tags are unioned.
/// (`x` is the existing row, `n` the incoming values.)
const FILL_GAPS: &str = "
    description = COALESCE(x.description, n.description),
    venue_name  = COALESCE(x.venue_name, n.venue_name),
    address     = COALESCE(x.address, n.address),
    lat         = COALESCE(x.lat, n.lat),
    lng         = COALESCE(x.lng, n.lng),
    ends_at     = COALESCE(x.ends_at, n.ends_at),
    is_free     = CASE WHEN x.price_min IS NULL AND x.price_max IS NULL AND NOT x.is_free
                       THEN n.is_free ELSE x.is_free END,
    price_min   = CASE WHEN x.price_min IS NULL AND x.price_max IS NULL AND NOT x.is_free
                       THEN n.price_min ELSE x.price_min END,
    price_max   = CASE WHEN x.price_min IS NULL AND x.price_max IS NULL AND NOT x.is_free
                       THEN n.price_max ELSE x.price_max END,
    currency    = COALESCE(x.currency, n.currency),
    url         = COALESCE(x.url, n.url),
    image_url   = COALESCE(x.image_url, n.image_url),
    tags        = ARRAY(SELECT DISTINCT t FROM unnest(x.tags || n.tags) AS t ORDER BY t),
    updated_at  = now()";

/// Update used when the SAME source re-reports its event: the source is
/// authoritative for its own copy (new values win, NULLs keep old values).
const REFRESH: &str = "
    title       = n.title,
    description = COALESCE(n.description, x.description),
    venue_name  = COALESCE(n.venue_name, x.venue_name),
    address     = COALESCE(n.address, x.address),
    lat         = COALESCE(n.lat, x.lat),
    lng         = COALESCE(n.lng, x.lng),
    starts_at   = n.starts_at,
    ends_at     = COALESCE(n.ends_at, x.ends_at),
    is_free     = CASE WHEN n.price_min IS NULL AND n.price_max IS NULL AND NOT n.is_free
                       THEN x.is_free ELSE n.is_free END,
    price_min   = CASE WHEN n.price_min IS NULL AND n.price_max IS NULL AND NOT n.is_free
                       THEN x.price_min ELSE n.price_min END,
    price_max   = CASE WHEN n.price_min IS NULL AND n.price_max IS NULL AND NOT n.is_free
                       THEN x.price_max ELSE n.price_max END,
    currency    = COALESCE(n.currency, x.currency),
    url         = COALESCE(n.url, x.url),
    image_url   = COALESCE(n.image_url, x.image_url),
    category    = n.category,
    tags        = ARRAY(SELECT DISTINCT t FROM unnest(x.tags || n.tags) AS t ORDER BY t),
    dedupe_key  = n.dedupe_key,
    updated_at  = now()";

/// `UPDATE events.events x SET <set> FROM (VALUES ...) n(...) WHERE x.id = $18`.
fn update_from_values(set: &str) -> String {
    format!(
        "UPDATE events.events AS x SET {set}
         FROM (VALUES ($1::text, $2::text, $3::text, $4::text, $5::float8, $6::float8,
                       $7::timestamptz, $8::timestamptz, $9::bool, $10::numeric, $11::numeric,
                       $12::text, $13::text, $14::text, $15::text, $16::text[], $17::text))
              AS n(title, description, venue_name, address, lat, lng, starts_at, ends_at,
                   is_free, price_min, price_max, currency, url, image_url, category, tags,
                   dedupe_key)
         WHERE x.id = $18"
    )
}

/// Insert an event or merge it into an existing one, and link the source.
///
/// 1. If this `(source, source_event_id)` is already linked:
///    * sole source of the event → refreshed with the new values (the source
///      owns its copy, so title/date corrections apply);
///    * event shared with other sources → gaps filled only (no flip-flopping
///      between sources' spellings); if the key changed, the source's copy is
///      split off into a new event;
///    * if the new dedupe key belongs to a different event, the link moves
///      there and the old event is deleted if nothing else links to it.
/// 2. Otherwise the event is inserted, or — when an event with the same
///    `dedupe_key` exists (e.g. from another source) — merged into it by
///    filling gaps only.
/// 3. The `events.event_sources` row is upserted (raw payload, last_seen_at).
///
/// Everything happens in one transaction.
pub async fn upsert_event(
    pool: &PgPool,
    source_id: i64,
    event: &NewEvent,
    raw: &RawEvent,
) -> sqlx::Result<UpsertOutcome> {
    let mut tx = pool.begin().await?;
    let outcome = upsert_event_tx(&mut tx, source_id, event, raw).await?;
    tx.commit().await?;
    Ok(outcome)
}

async fn upsert_event_tx(
    tx: &mut Transaction<'_, Postgres>,
    source_id: i64,
    event: &NewEvent,
    raw: &RawEvent,
) -> sqlx::Result<UpsertOutcome> {
    let linked: Option<Uuid> = sqlx::query_scalar(
        "SELECT event_id FROM events.event_sources
         WHERE source_id = $1 AND source_event_id = $2 FOR UPDATE",
    )
    .bind(source_id)
    .bind(&raw.source_event_id)
    .fetch_optional(&mut **tx)
    .await?;

    let holder: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM events.events WHERE dedupe_key = $1 FOR UPDATE")
            .bind(&event.dedupe_key)
            .fetch_optional(&mut **tx)
            .await?;

    let mut orphan_candidate: Option<Uuid> = None;
    let outcome = match (linked, holder) {
        // Already linked from this source, and the (possibly new) key is
        // free or still ours.
        (Some(id), h) if h.is_none() || h == Some(id) => {
            let other_sources: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM events.event_sources
                 WHERE event_id = $1 AND source_id <> $2",
            )
            .bind(id)
            .bind(source_id)
            .fetch_one(&mut **tx)
            .await?;
            if other_sources == 0 {
                // Sole owner: the source is authoritative, refresh in place.
                refresh(tx, id, event).await?;
                UpsertOutcome {
                    event_id: id,
                    created: false,
                }
            } else if h == Some(id) {
                // Shared event: only fill gaps, so sources don't flip-flop.
                fill_gaps(tx, id, event).await?;
                UpsertOutcome {
                    event_id: id,
                    created: false,
                }
            } else {
                // Shared event but this source now describes something else
                // (key changed): split it off into its own event.
                let (new_id,): (Uuid,) = bind_event_as(
                    sqlx::query_as(AssertSqlSafe(format!("{EVENT_INSERT} RETURNING id"))),
                    event,
                )
                .fetch_one(&mut **tx)
                .await?;
                UpsertOutcome {
                    event_id: new_id,
                    created: true,
                }
            }
        }
        // Same source, but its new key collides with another event: move.
        (Some(_), None) => unreachable!("covered by the first arm's guard"),
        (Some(old), Some(other)) => {
            fill_gaps(tx, other, event).await?;
            orphan_candidate = Some(old);
            UpsertOutcome {
                event_id: other,
                created: false,
            }
        }
        // New to this source; an event with this key exists: merge.
        (None, Some(other)) => {
            fill_gaps(tx, other, event).await?;
            UpsertOutcome {
                event_id: other,
                created: false,
            }
        }
        // Brand new.
        (None, None) => {
            let (id,): (Uuid,) = bind_event_as(
                sqlx::query_as(AssertSqlSafe(format!("{EVENT_INSERT} RETURNING id"))),
                event,
            )
            .fetch_one(&mut **tx)
            .await?;
            UpsertOutcome {
                event_id: id,
                created: true,
            }
        }
    };

    sqlx::query(
        "INSERT INTO events.event_sources
            (event_id, source_id, source_event_id, source_url, raw)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (source_id, source_event_id) DO UPDATE SET
            event_id = EXCLUDED.event_id,
            source_url = EXCLUDED.source_url,
            raw = EXCLUDED.raw,
            last_seen_at = now()",
    )
    .bind(outcome.event_id)
    .bind(source_id)
    .bind(&raw.source_event_id)
    .bind(&raw.source_url)
    .bind(&raw.payload)
    .execute(&mut **tx)
    .await?;

    if let Some(old) = orphan_candidate {
        sqlx::query(
            "DELETE FROM events.events e WHERE e.id = $1
               AND NOT EXISTS (SELECT 1 FROM events.event_sources s WHERE s.event_id = e.id)",
        )
        .bind(old)
        .execute(&mut **tx)
        .await?;
    }
    Ok(outcome)
}

async fn refresh(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    event: &NewEvent,
) -> sqlx::Result<()> {
    let sql = update_from_values(REFRESH);
    bind_event(sqlx::query(AssertSqlSafe(sql)), event)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn fill_gaps(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    event: &NewEvent,
) -> sqlx::Result<()> {
    let sql = update_from_values(FILL_GAPS);
    bind_event(sqlx::query(AssertSqlSafe(sql)), event)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// A stored event (subset used by tests and, later, the read API).
#[derive(Debug, Clone, FromRow)]
pub struct EventRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub venue_name: Option<String>,
    pub address: Option<String>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub is_free: bool,
    pub price_min: Option<Decimal>,
    pub price_max: Option<Decimal>,
    pub currency: Option<String>,
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub category: String,
    pub tags: Vec<String>,
    pub dedupe_key: String,
}

pub async fn get_event(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<EventRow>> {
    sqlx::query_as(
        "SELECT id, title, description, venue_name, address, lat, lng, starts_at, ends_at,
                is_free, price_min, price_max, currency, url, image_url, category, tags,
                dedupe_key
         FROM events.events WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// A completed source run to record.
#[derive(Debug, Clone)]
pub struct NewRun {
    pub source_id: i64,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub events_found: i32,
    pub errors: i32,
    pub error_summary: Option<String>,
    pub ok: bool,
}

#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct RunRow {
    pub id: i64,
    pub source_id: i64,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub events_found: i32,
    pub errors: i32,
    pub error_summary: Option<String>,
    pub ok: bool,
}

/// Record a run and bump the source's `last_run_at` to the run start.
pub async fn record_run(pool: &PgPool, run: &NewRun) -> sqlx::Result<i64> {
    let mut tx = pool.begin().await?;
    let duration_ms = (run.finished_at - run.started_at).num_milliseconds().max(0);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO events.source_runs
            (source_id, started_at, finished_at, duration_ms, events_found, errors, error_summary, ok)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(run.source_id)
    .bind(run.started_at)
    .bind(run.finished_at)
    .bind(duration_ms)
    .bind(run.events_found)
    .bind(run.errors)
    .bind(&run.error_summary)
    .bind(run.ok)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE events.sources SET last_run_at = $2 WHERE id = $1")
        .bind(run.source_id)
        .bind(run.started_at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

/// Most recent runs of a source, newest first.
pub async fn recent_runs(pool: &PgPool, source_id: i64, limit: i64) -> sqlx::Result<Vec<RunRow>> {
    sqlx::query_as(
        "SELECT id, source_id, started_at, finished_at, duration_ms, events_found, errors,
                error_summary, ok
         FROM events.source_runs WHERE source_id = $1
         ORDER BY started_at DESC, id DESC LIMIT $2",
    )
    .bind(source_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct HealthIssueRow {
    pub id: i64,
    pub source_id: i64,
    pub github_issue_number: i64,
    pub reason: String,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

pub async fn open_health_issue(
    pool: &PgPool,
    source_id: i64,
) -> sqlx::Result<Option<HealthIssueRow>> {
    sqlx::query_as(
        "SELECT id, source_id, github_issue_number, reason, opened_at, closed_at
         FROM events.health_issues WHERE source_id = $1 AND closed_at IS NULL",
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
}

/// Record an open issue; a no-op if one is already open for the source.
pub async fn insert_health_issue(
    pool: &PgPool,
    source_id: i64,
    number: i64,
    reason: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO events.health_issues (source_id, github_issue_number, reason)
         VALUES ($1, $2, $3)
         ON CONFLICT (source_id) WHERE closed_at IS NULL DO NOTHING",
    )
    .bind(source_id)
    .bind(number)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn close_health_issues(pool: &PgPool, source_id: i64) -> sqlx::Result<u64> {
    Ok(sqlx::query(
        "UPDATE events.health_issues SET closed_at = now()
         WHERE source_id = $1 AND closed_at IS NULL",
    )
    .bind(source_id)
    .execute(pool)
    .await?
    .rows_affected())
}
