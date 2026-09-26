//! SQL for AI enrichment and embeddings (all in schema `events`).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::input::EventFacts;
use super::output::Enrichment;

/// Catalogue prices of one model, USD per million tokens.
#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct ModelPrice {
    pub model: String,
    pub input_usd_per_mtok: Decimal,
    pub output_usd_per_mtok: Decimal,
    pub cache_read_usd_per_mtok: Decimal,
    pub cache_write_usd_per_mtok: Decimal,
    /// Days the provider keeps prompts (0 = zero retention, NULL = unknown).
    pub retention_days: Option<i32>,
}

pub async fn model_price(pool: &PgPool, model: &str) -> sqlx::Result<Option<ModelPrice>> {
    sqlx::query_as(
        "SELECT model, input_usd_per_mtok, output_usd_per_mtok, cache_read_usd_per_mtok,
                cache_write_usd_per_mtok, retention_days
         FROM events.model_prices WHERE model = $1",
    )
    .bind(model)
    .fetch_optional(pool)
    .await
}

/// An upcoming event with the state of its enrichment.
#[derive(Debug, Clone, FromRow)]
pub struct Candidate {
    #[sqlx(flatten)]
    pub facts: EventFacts,
    pub enriched_hash: Option<String>,
    pub enriched_version: Option<i32>,
    pub enriched_output: Option<sqlx::types::Json<Enrichment>>,
    /// Whether AI fields are currently shown on the event.
    pub materialised: bool,
    pub failed_hash: Option<String>,
    pub failed_version: Option<i32>,
}

/// Events still running at or after `since`: those with a stored excerpt
/// first, then soonest first.
pub async fn candidates(pool: &PgPool, since: DateTime<Utc>) -> sqlx::Result<Vec<Candidate>> {
    sqlx::query_as(
        "SELECT e.id, e.title, e.venue_name, e.starts_at, e.ends_at, e.category, e.tags,
                e.is_free, e.price_min, e.price_max, e.currency, e.description,
                ARRAY(SELECT DISTINCT COALESCE(s.display_name, s.key)
                        FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
                       WHERE es.event_id = e.id ORDER BY 1) AS listed_by,
                en.input_hash AS enriched_hash, en.prompt_version AS enriched_version,
                en.output AS enriched_output,
                (e.ai_enriched_at IS NOT NULL) AS materialised,
                f.input_hash AS failed_hash, f.prompt_version AS failed_version
         FROM events.events e
         LEFT JOIN events.enrichments en ON en.event_id = e.id
         LEFT JOIN events.enrichment_failures f ON f.event_id = e.id
         WHERE COALESCE(e.ends_at, e.starts_at) >= $1
         -- Events with an excerpt first: they get the most out of a call,
         -- while facts-only listings often come back \"insufficient\".
         ORDER BY (NULLIF(btrim(e.description), '') IS NULL), e.starts_at, e.id",
    )
    .bind(since)
    .fetch_all(pool)
    .await
}

/// The deterministic default medium tags of an event's sources.
const DEFAULT_TAGS: &str = "ARRAY(SELECT DISTINCT t
        FROM events.event_sources es
        JOIN events.sources s ON s.id = es.source_id
        CROSS JOIN LATERAL unnest(s.default_medium_tags) AS t
        WHERE es.event_id = e.id ORDER BY t)";

/// Give events without AI fields their sources' default medium tags.
pub async fn apply_default_tags(pool: &PgPool) -> sqlx::Result<u64> {
    let sql = format!(
        "UPDATE events.events AS e SET medium_tags = {DEFAULT_TAGS}
         WHERE e.ai_enriched_at IS NULL AND e.medium_tags IS DISTINCT FROM {DEFAULT_TAGS}"
    );
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(pool)
        .await?
        .rows_affected())
}

/// Remove an event's AI fields (its input changed, so they may be wrong);
/// medium tags fall back to the sources' defaults.
pub async fn clear_materialised(pool: &PgPool, event_id: Uuid) -> sqlx::Result<()> {
    let sql = format!(
        "UPDATE events.events AS e SET medium_tags = {DEFAULT_TAGS}, format_tags = '{{}}',
             good_for = '{{}}', vibe_tags = '{{}}', is_opening = NULL, whats_cool = NULL,
             one_liner = NULL, ai_grounding = NULL, ai_model = NULL, ai_enriched_at = NULL
         WHERE e.id = $1"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(event_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// A validated enrichment and its share of the call's cost.
pub struct NewEnrichment<'a> {
    pub event_id: Uuid,
    pub model: &'a str,
    pub prompt_version: i32,
    pub input_hash: &'a str,
    pub output: &'a Enrichment,
    pub tokens_in: i32,
    pub tokens_out: i32,
    pub cost_usd: Decimal,
}

/// Store an enrichment and materialise it on the event, in one transaction
/// (all fields or none).
pub async fn save_enrichment(pool: &PgPool, n: &NewEnrichment<'_>) -> sqlx::Result<()> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;
    sqlx::query(
        "INSERT INTO events.enrichments
             (event_id, model, prompt_version, input_hash, output, tokens_in, tokens_out, cost_usd)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (event_id) DO UPDATE SET model = EXCLUDED.model,
             prompt_version = EXCLUDED.prompt_version, input_hash = EXCLUDED.input_hash,
             output = EXCLUDED.output, tokens_in = EXCLUDED.tokens_in,
             tokens_out = EXCLUDED.tokens_out, cost_usd = EXCLUDED.cost_usd, created_at = now()",
    )
    .bind(n.event_id)
    .bind(n.model)
    .bind(n.prompt_version)
    .bind(n.input_hash)
    .bind(sqlx::types::Json(n.output))
    .bind(n.tokens_in)
    .bind(n.tokens_out)
    .bind(n.cost_usd)
    .execute(&mut *tx)
    .await?;
    let o = n.output;
    let sql = format!(
        "UPDATE events.events AS e SET
             medium_tags = ARRAY(SELECT DISTINCT t FROM unnest($2::text[] || {DEFAULT_TAGS}) AS t ORDER BY t),
             format_tags = $3, good_for = $4, vibe_tags = $5, is_opening = $6,
             whats_cool = $7, one_liner = $8, ai_grounding = $9, ai_model = $10,
             ai_enriched_at = now()
         WHERE e.id = $1"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(n.event_id)
        .bind(&o.medium_tags)
        .bind(&o.format_tags)
        .bind(&o.good_for)
        .bind(&o.vibe_tags)
        .bind(o.is_opening)
        .bind(&o.whats_cool)
        .bind(&o.one_liner)
        .bind(&o.grounding)
        .bind(n.model)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM events.enrichment_failures WHERE event_id = $1")
        .bind(n.event_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// Remember a give-up so the event is not re-sent until its input (or the
/// prompt version) changes.
pub async fn record_failure(
    pool: &PgPool,
    event_id: Uuid,
    input_hash: &str,
    prompt_version: i32,
    error: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO events.enrichment_failures (event_id, input_hash, prompt_version, error)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (event_id) DO UPDATE SET input_hash = EXCLUDED.input_hash,
             prompt_version = EXCLUDED.prompt_version, error = EXCLUDED.error, failed_at = now()",
    )
    .bind(event_id)
    .bind(input_hash)
    .bind(prompt_version)
    .bind(error.chars().take(1000).collect::<String>())
    .execute(pool)
    .await?;
    Ok(())
}

/// One row of the spend ledger.
#[derive(Debug, Clone, Default)]
pub struct CallRecord {
    pub model: String,
    pub prompt_version: i32,
    pub events_requested: i32,
    pub events_ok: i32,
    pub tokens_in: i64,
    pub tokens_cached: i64,
    pub tokens_cache_write: i64,
    pub tokens_out: i64,
    pub cost_usd: Decimal,
    pub provider_cost_usd: Option<Decimal>,
    pub ok: bool,
    pub error: Option<String>,
}

pub async fn record_call(pool: &PgPool, c: &CallRecord, at: DateTime<Utc>) -> sqlx::Result<()> {
    let clamp = |v: i64| i32::try_from(v).unwrap_or(i32::MAX);
    sqlx::query(
        "INSERT INTO events.enrichment_calls
             (called_at, model, prompt_version, events_requested, events_ok, tokens_in,
              tokens_cached, tokens_cache_write, tokens_out, cost_usd, provider_cost_usd, ok, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(at)
    .bind(&c.model)
    .bind(c.prompt_version)
    .bind(c.events_requested)
    .bind(c.events_ok)
    .bind(clamp(c.tokens_in))
    .bind(clamp(c.tokens_cached))
    .bind(clamp(c.tokens_cache_write))
    .bind(clamp(c.tokens_out))
    .bind(c.cost_usd.round_dp(6))
    .bind(c.provider_cost_usd.map(|d| d.round_dp(6)))
    .bind(c.ok)
    .bind(&c.error)
    .execute(pool)
    .await?;
    Ok(())
}

/// Total recorded spend (enrichment + embeddings) since `since`.
pub async fn spent_since(pool: &PgPool, since: DateTime<Utc>) -> sqlx::Result<Decimal> {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(cost_usd), 0) FROM events.enrichment_calls WHERE called_at >= $1",
    )
    .bind(since)
    .fetch_one(pool)
    .await
}

// ------------------------------------------------------------ alert state

#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct AlertState {
    pub active_since: Option<DateTime<Utc>>,
    pub last_notified_at: Option<DateTime<Utc>>,
}

/// Mark `key` active (keeping the first time it was seen) and return it.
pub async fn alert_raise(
    pool: &PgPool,
    key: &str,
    detail: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<AlertState> {
    sqlx::query_as(
        "INSERT INTO events.alert_state (key, active_since, detail, updated_at)
         VALUES ($1, $3, $2, $3)
         ON CONFLICT (key) DO UPDATE SET
             active_since = COALESCE(events.alert_state.active_since, EXCLUDED.active_since),
             detail = EXCLUDED.detail, updated_at = EXCLUDED.updated_at
         RETURNING active_since, last_notified_at",
    )
    .bind(key)
    .bind(detail)
    .bind(now)
    .fetch_one(pool)
    .await
}

pub async fn alert_notified(pool: &PgPool, key: &str, now: DateTime<Utc>) -> sqlx::Result<()> {
    sqlx::query("UPDATE events.alert_state SET last_notified_at = $2 WHERE key = $1")
        .bind(key)
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}

/// Clear `key`; returns when it had become active, if it was.
pub async fn alert_clear(
    pool: &PgPool,
    key: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<DateTime<Utc>>> {
    let prev: Option<Option<DateTime<Utc>>> = sqlx::query_scalar(
        "WITH old AS (
             SELECT active_since FROM events.alert_state
             WHERE key = $1 AND active_since IS NOT NULL FOR UPDATE)
         UPDATE events.alert_state AS a SET active_since = NULL, last_notified_at = NULL,
             updated_at = $2
         FROM old WHERE a.key = $1
         RETURNING old.active_since",
    )
    .bind(key)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(prev.flatten())
}

pub async fn alert_state(pool: &PgPool, key: &str) -> sqlx::Result<Option<AlertState>> {
    sqlx::query_as("SELECT active_since, last_notified_at FROM events.alert_state WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
}

// ------------------------------------------------------------ embeddings

/// Whether `events.event_embeddings` exists (pgvector usable; see its
/// migration).
pub async fn embeddings_available(pool: &PgPool) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT to_regclass('events.event_embeddings') IS NOT NULL")
        .fetch_one(pool)
        .await
}

#[derive(Debug, Clone, FromRow)]
pub struct EmbeddingRow {
    pub event_id: Uuid,
    pub text_hash: String,
    pub embed_version: i32,
    pub model: String,
}

pub async fn embedding_rows(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<Vec<EmbeddingRow>> {
    sqlx::query_as(
        "SELECT event_id, text_hash, embed_version, model FROM events.event_embeddings
         WHERE event_id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
}

pub struct NewEmbedding<'a> {
    pub event_id: Uuid,
    pub model: &'a str,
    pub embed_version: i32,
    pub text_hash: &'a str,
    pub facts_only: bool,
    pub embedding: &'a [f32],
}

pub async fn save_embedding(pool: &PgPool, n: &NewEmbedding<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO events.event_embeddings
             (event_id, model, embed_version, text_hash, facts_only, embedding)
         VALUES ($1, $2, $3, $4, $5, $6::real[]::extensions.vector)
         ON CONFLICT (event_id) DO UPDATE SET model = EXCLUDED.model,
             embed_version = EXCLUDED.embed_version, text_hash = EXCLUDED.text_hash,
             facts_only = EXCLUDED.facts_only, embedding = EXCLUDED.embedding, created_at = now()",
    )
    .bind(n.event_id)
    .bind(n.model)
    .bind(n.embed_version)
    .bind(n.text_hash)
    .bind(n.facts_only)
    .bind(n.embedding)
    .execute(pool)
    .await?;
    Ok(())
}

/// A neighbour for "More like this".
#[derive(Debug, Clone, FromRow)]
pub struct Similar {
    pub id: Uuid,
    pub title: String,
    pub venue_name: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub category: String,
    pub medium_tags: Vec<String>,
    pub format_tags: Vec<String>,
    pub good_for: Vec<String>,
    pub vibe_tags: Vec<String>,
    pub similarity: f64,
}

/// Up to `k` events nearest to `event_id` by cosine distance that are still
/// running at or after `since` (never the event itself). Empty when the
/// event has no embedding or embeddings are off.
pub async fn more_like_this(
    pool: &PgPool,
    event_id: Uuid,
    since: DateTime<Utc>,
    k: i64,
) -> sqlx::Result<Vec<Similar>> {
    if !embeddings_available(pool).await? {
        return Ok(Vec::new());
    }
    // Over-fetch from the index, then drop past events.
    sqlx::query_as(
        "SELECT * FROM (
             SELECT ev.id, ev.title, ev.venue_name, ev.starts_at, ev.ends_at, ev.category,
                    ev.medium_tags, ev.format_tags, ev.good_for, ev.vibe_tags,
                    1 - (em.embedding OPERATOR(extensions.<=>) q.embedding) AS similarity
             FROM events.event_embeddings q
             JOIN events.event_embeddings em ON em.event_id <> q.event_id
             JOIN events.events ev ON ev.id = em.event_id
             WHERE q.event_id = $1 AND COALESCE(ev.ends_at, ev.starts_at) >= $2
             ORDER BY em.embedding OPERATOR(extensions.<=>) q.embedding
             LIMIT $3
         ) s ORDER BY similarity DESC, id",
    )
    .bind(event_id)
    .bind(since)
    .bind(k)
    .fetch_all(pool)
    .await
}
