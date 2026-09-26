//! Database access. All SQL is schema-qualified with `events.`.
//!
//! Runtime-checked queries only (`sqlx::query*`), no compile-time macros, so
//! builds never need a live database.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{AssertSqlSafe, FromRow, PgPool, Postgres, Transaction};
use std::collections::HashSet;
use uuid::Uuid;

use crate::matching::{self, MatchInput, TitleScore};
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

/// Merge used when a DIFFERENT source reports an event we already have
/// (`x` is the existing row, `n` the incoming values). Existing values win
/// and the newcomer only fills gaps, except where the incoming source has
/// precedence: `$19` (a venue site) wins dates, description and image; `$20`
/// (an API) wins price and URL. Tags are unioned. The `ends_at` guards keep
/// `events_ends_after_start` true when fuzzy-merged start dates differ.
const MERGE: &str = "
    description = CASE WHEN $19::bool THEN COALESCE(n.description, x.description)
                       ELSE COALESCE(x.description, n.description) END,
    venue_name  = COALESCE(x.venue_name, n.venue_name),
    address     = COALESCE(x.address, n.address),
    lat         = COALESCE(x.lat, n.lat),
    lng         = COALESCE(x.lng, n.lng),
    starts_at   = CASE WHEN $19::bool THEN n.starts_at ELSE x.starts_at END,
    ends_at     = CASE WHEN $19::bool
                       THEN (CASE WHEN n.ends_at IS NOT NULL THEN n.ends_at
                                  WHEN x.ends_at >= n.starts_at THEN x.ends_at END)
                       ELSE COALESCE(x.ends_at,
                                     CASE WHEN n.ends_at >= x.starts_at THEN n.ends_at END) END,
    is_free     = CASE WHEN $20::bool AND (n.is_free OR n.price_min IS NOT NULL
                                           OR n.price_max IS NOT NULL) THEN n.is_free
                       WHEN x.price_min IS NULL AND x.price_max IS NULL AND NOT x.is_free
                       THEN n.is_free ELSE x.is_free END,
    price_min   = CASE WHEN $20::bool AND (n.is_free OR n.price_min IS NOT NULL
                                           OR n.price_max IS NOT NULL) THEN n.price_min
                       WHEN x.price_min IS NULL AND x.price_max IS NULL AND NOT x.is_free
                       THEN n.price_min ELSE x.price_min END,
    price_max   = CASE WHEN $20::bool AND (n.is_free OR n.price_min IS NOT NULL
                                           OR n.price_max IS NOT NULL) THEN n.price_max
                       WHEN x.price_min IS NULL AND x.price_max IS NULL AND NOT x.is_free
                       THEN n.price_max ELSE x.price_max END,
    currency    = CASE WHEN $20::bool AND (n.is_free OR n.price_min IS NOT NULL
                                           OR n.price_max IS NOT NULL)
                       THEN COALESCE(n.currency, x.currency)
                       ELSE COALESCE(x.currency, n.currency) END,
    url         = CASE WHEN $20::bool THEN COALESCE(n.url, x.url) ELSE COALESCE(x.url, n.url) END,
    image_url   = CASE WHEN $19::bool THEN COALESCE(n.image_url, x.image_url)
                       ELSE COALESCE(x.image_url, n.image_url) END,
    tags        = ARRAY(SELECT DISTINCT t FROM unnest(x.tags || n.tags) AS t ORDER BY t),
    updated_at  = now()";

/// Update used when the SAME source re-reports its event: the source is
/// authoritative for its own copy (new values win, NULLs keep old values).
/// An old `ends_at` left by a departed source is dropped if it would now end
/// before the start.
const REFRESH: &str = "
    title       = n.title,
    description = COALESCE(n.description, x.description),
    venue_name  = COALESCE(n.venue_name, x.venue_name),
    address     = COALESCE(n.address, x.address),
    lat         = COALESCE(n.lat, x.lat),
    lng         = COALESCE(n.lng, x.lng),
    starts_at   = n.starts_at,
    ends_at     = COALESCE(n.ends_at, CASE WHEN x.ends_at >= n.starts_at THEN x.ends_at END),
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
/// The target event is the first of:
///
/// 1. a `force_merge` override partner's event;
/// 2. the event this `(source, source_event_id)` is already linked to, when
///    this source is its sole owner and the dedupe key is free or still its
///    own: refreshed with the new values (the source owns its copy, so
///    title/date corrections apply);
/// 3. the linked event, when shared with other sources and still holding
///    the key: merged (no flip-flopping between sources' spellings);
/// 4. the event holding the same `dedupe_key` (e.g. from another source):
///    merged;
/// 5. otherwise a fuzzy match (`crate::matching`) among events of nearby
///    dates, excluding events holding a different listing of this same
///    source: the linked event if it still matches, else the best match
///    (highest title dice, then oldest), merged;
/// 6. otherwise a new event (a listing that no longer matches its shared
///    event is split off).
///
/// Merges apply field precedence: a venue-site (`scraper`) source wins
/// dates, description and image, an `api` source wins price and URL, each
/// only while no other linked source has the same kind; everything else
/// keeps the existing value and fills gaps. The title is first-come.
///
/// `never_merge` overrides exclude the partner's event from 2–5; on an exact
/// key collision the listing's key is suffixed with its identity so the two
/// can coexist. Overrides (`events.merge_overrides`) take effect the next
/// time either listing is upserted.
///
/// The `events.event_sources` row is then upserted (raw payload,
/// last_seen_at), and an event the listing moved away from is deleted if
/// nothing else links to it. Everything happens in one transaction.
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

    let overrides = load_overrides(tx, source_id, &raw.source_event_id).await?;
    let forbidden: HashSet<Uuid> = overrides
        .iter()
        .filter(|(action, _)| action == "never_merge")
        .map(|(_, id)| *id)
        .collect();
    let force = overrides
        .iter()
        .find(|(action, id)| action == "force_merge" && !forbidden.contains(id))
        .map(|(_, id)| *id);

    let mut ev = event.clone();
    let mut holder = key_holder(tx, &ev.dedupe_key).await?;
    if holder.is_some_and(|h| forbidden.contains(&h)) {
        // The dedupe key index is UNIQUE, so a never-merge partner holding
        // our key can only be kept apart under a listing-specific key.
        ev.dedupe_key = format!("{}|{}:{}", event.dedupe_key, source_id, raw.source_event_id);
        holder = key_holder(tx, &ev.dedupe_key).await?;
    }

    let linked_ok = linked.filter(|l| !forbidden.contains(l));
    let shared = match linked_ok {
        Some(l) => {
            let other_sources: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM events.event_sources
                 WHERE event_id = $1 AND source_id <> $2",
            )
            .bind(l)
            .bind(source_id)
            .fetch_one(&mut **tx)
            .await?;
            other_sources > 0
        }
        None => false,
    };

    let outcome = if let Some(target) = force {
        tracing::info!(
            event_id = %target,
            source_id,
            source_event_id = %raw.source_event_id,
            "force merge"
        );
        merge_into(tx, target, source_id, &ev).await?;
        UpsertOutcome {
            event_id: target,
            created: false,
        }
    } else if let Some(l) = linked_ok.filter(|l| !shared && holder.is_none_or(|h| h == *l)) {
        refresh(tx, l, &ev).await?;
        UpsertOutcome {
            event_id: l,
            created: false,
        }
    } else if let Some(target) = linked_ok.filter(|l| holder == Some(*l)).or(holder) {
        merge_into(tx, target, source_id, &ev).await?;
        UpsertOutcome {
            event_id: target,
            created: false,
        }
    } else {
        let incoming = MatchInput::from(&ev);
        let mut matches: Vec<(CandidateRow, TitleScore)> =
            fuzzy_candidates(tx, &ev, source_id, &raw.source_event_id)
                .await?
                .into_iter()
                .filter(|c| !forbidden.contains(&c.id))
                .filter_map(|c| {
                    let score = matching::match_score(&incoming, &MatchInput::from(&c))?;
                    Some((c, score))
                })
                .collect();
        matches.sort_by(|(a, sa), (b, sb)| {
            sb.dice
                .total_cmp(&sa.dice)
                .then(a.created_at.cmp(&b.created_at))
                .then(a.id.cmp(&b.id))
        });
        let stay = linked_ok.filter(|l| matches.iter().any(|(c, _)| c.id == *l));
        if let Some(l) = stay {
            tracing::debug!(event_id = %l, source_id, "fuzzy match keeps listing in place");
            merge_into(tx, l, source_id, &ev).await?;
            UpsertOutcome {
                event_id: l,
                created: false,
            }
        } else if let Some((c, score)) = matches.first() {
            tracing::info!(
                event_id = %c.id,
                source_id,
                source_event_id = %raw.source_event_id,
                incoming_title = %ev.title,
                existing_title = %c.title,
                jaccard = score.jaccard,
                dice = score.dice,
                "fuzzy merge"
            );
            merge_into(tx, c.id, source_id, &ev).await?;
            UpsertOutcome {
                event_id: c.id,
                created: false,
            }
        } else {
            let (id,): (Uuid,) = bind_event_as(
                sqlx::query_as(AssertSqlSafe(format!("{EVENT_INSERT} RETURNING id"))),
                &ev,
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

    if let Some(old) = linked.filter(|old| *old != outcome.event_id) {
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

async fn key_holder(
    tx: &mut Transaction<'_, Postgres>,
    dedupe_key: &str,
) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar("SELECT id FROM events.events WHERE dedupe_key = $1 FOR UPDATE")
        .bind(dedupe_key)
        .fetch_optional(&mut **tx)
        .await
}

/// `(action, partner's event id)` for every override naming this listing
/// whose partner listing has been ingested, oldest override first.
async fn load_overrides(
    tx: &mut Transaction<'_, Postgres>,
    source_id: i64,
    source_event_id: &str,
) -> sqlx::Result<Vec<(String, Uuid)>> {
    sqlx::query_as(
        "SELECT o.action, es.event_id
         FROM events.merge_overrides o
         JOIN events.event_sources es
           ON (o.source_id_a = $1 AND o.source_event_id_a = $2
               AND es.source_id = o.source_id_b AND es.source_event_id = o.source_event_id_b)
           OR (o.source_id_b = $1 AND o.source_event_id_b = $2
               AND es.source_id = o.source_id_a AND es.source_event_id = o.source_event_id_a)
         ORDER BY o.id",
    )
    .bind(source_id)
    .bind(source_event_id)
    .fetch_all(&mut **tx)
    .await
}

/// An existing event considered for a fuzzy merge.
#[derive(Debug, Clone, FromRow)]
struct CandidateRow {
    id: Uuid,
    title: String,
    venue_name: Option<String>,
    lat: Option<f64>,
    lng: Option<f64>,
    starts_at: DateTime<Utc>,
    ends_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl<'a> From<&'a CandidateRow> for MatchInput<'a> {
    fn from(c: &'a CandidateRow) -> Self {
        MatchInput {
            title: &c.title,
            venue_name: c.venue_name.as_deref(),
            lat: c.lat,
            lng: c.lng,
            starts_at: c.starts_at,
            ends_at: c.ends_at,
        }
    }
}

/// Events whose date range comes within two days of `ev`'s (slack for
/// London-day rounding; `matching` does the exact check), excluding events
/// that carry a different listing of this source: one source's distinct
/// listings are never fuzzy-joined.
async fn fuzzy_candidates(
    tx: &mut Transaction<'_, Postgres>,
    ev: &NewEvent,
    source_id: i64,
    source_event_id: &str,
) -> sqlx::Result<Vec<CandidateRow>> {
    let slack = chrono::Duration::days(2);
    sqlx::query_as(
        "SELECT e.id, e.title, e.venue_name, e.lat, e.lng, e.starts_at, e.ends_at, e.created_at
         FROM events.events e
         WHERE e.starts_at < $2 AND COALESCE(e.ends_at, e.starts_at) > $1
           AND NOT EXISTS (SELECT 1 FROM events.event_sources s
                           WHERE s.event_id = e.id AND s.source_id = $3
                             AND s.source_event_id <> $4)",
    )
    .bind(ev.starts_at - slack)
    .bind(ev.ends_at.unwrap_or(ev.starts_at) + slack)
    .bind(source_id)
    .bind(source_event_id)
    .fetch_all(&mut **tx)
    .await
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

/// Merge `event` from `source_id` into event `id` with [`MERGE`], deciding
/// the precedence flags from the source kinds linked to `id`. A source only
/// takes precedence while it is the sole linked source of its kind, so two
/// venue sites (or two APIs) never overwrite each other on alternate runs.
async fn merge_into(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    source_id: i64,
    event: &NewEvent,
) -> sqlx::Result<()> {
    let (kind, other_same_kind): (String, bool) = sqlx::query_as(
        "SELECT s.kind, EXISTS (SELECT 1 FROM events.event_sources es
                                JOIN events.sources o ON o.id = es.source_id
                                WHERE es.event_id = $2 AND es.source_id <> $1
                                  AND o.kind = s.kind)
         FROM events.sources s WHERE s.id = $1",
    )
    .bind(source_id)
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    let site_wins = kind == "scraper" && !other_same_kind;
    let api_wins = kind == "api" && !other_same_kind;
    let sql = update_from_values(MERGE);
    bind_event(sqlx::query(AssertSqlSafe(sql)), event)
        .bind(id)
        .bind(site_wins)
        .bind(api_wins)
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
