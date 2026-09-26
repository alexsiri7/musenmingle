//! Database access. All SQL is schema-qualified with `events.`.
//!
//! Runtime-checked queries only (`sqlx::query*`), no compile-time macros, so
//! builds never need a live database.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::{AssertSqlSafe, FromRow, PgPool, Postgres, Transaction};
use std::collections::HashSet;
use uuid::Uuid;

use crate::listing::{EARTH_RADIUS_KM, EventFilter, EventOrder, EventQuery, Near, When};
use crate::matching::{self, MatchInput, TitleScore};
use crate::model::{NewEvent, OverrideAction, RawEvent, SourceKind};

#[derive(Debug, Clone, FromRow)]
pub struct SourceRow {
    pub id: i64,
    pub key: String,
    pub kind: SourceKind,
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
    kind: SourceKind,
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

/// What we may keep from a source (`events.sources` content policy columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromRow)]
pub struct SourcePolicy {
    pub store_description: bool,
    pub store_image: bool,
}

impl Default for SourcePolicy {
    fn default() -> Self {
        Self {
            store_description: true,
            store_image: true,
        }
    }
}

/// What `events.event_sources.raw` holds for sources whose terms restrict
/// reuse (see [`SourcePolicy::restricted`]).
pub fn redacted_raw() -> serde_json::Value {
    serde_json::json!({ "redacted": "content policy" })
}

impl SourcePolicy {
    /// The source's terms restrict reuse: we keep facts + link only, and
    /// not its raw payload either.
    pub fn restricted(&self) -> bool {
        !self.store_description || !self.store_image
    }

    /// Drop what the source's terms don't let us keep and cut the
    /// description to a short excerpt (we link out for the full text).
    pub fn apply(&self, ev: &mut NewEvent) {
        ev.description = if self.store_description {
            ev.description
                .as_deref()
                .map(crate::normalise::excerpt)
                .filter(|d| !d.is_empty())
        } else {
            None
        };
        if !self.store_image {
            ev.image_url = None;
        }
    }
}

async fn source_policy_tx(
    tx: &mut Transaction<'_, Postgres>,
    source_id: i64,
) -> sqlx::Result<SourcePolicy> {
    Ok(
        sqlx::query_as("SELECT store_description, store_image FROM events.sources WHERE id = $1")
            .bind(source_id)
            .fetch_optional(&mut **tx)
            .await?
            .unwrap_or_default(),
    )
}

/// Set a source's display name and content policy (tests and tooling; in
/// production these come from migrations).
pub async fn set_source_policy(
    pool: &PgPool,
    key: &str,
    display_name: Option<&str>,
    policy: SourcePolicy,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE events.sources SET display_name = $2, store_description = $3, store_image = $4
         WHERE key = $1",
    )
    .bind(key)
    .bind(display_name)
    .bind(policy.store_description)
    .bind(policy.store_image)
    .execute(pool)
    .await?;
    Ok(())
}

/// A source's human-readable name: `display_name`, else the key title-cased
/// ("design-museum" → "Design Museum").
pub fn display_name(key: &str, display_name: Option<&str>) -> String {
    if let Some(d) = display_name.map(str::trim).filter(|d| !d.is_empty()) {
        return d.to_string();
    }
    key.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        .filter(|(action, _)| *action == OverrideAction::NeverMerge)
        .map(|(_, id)| *id)
        .collect();
    let force = overrides
        .iter()
        .find(|(action, id)| *action == OverrideAction::ForceMerge && !forbidden.contains(id))
        .map(|(_, id)| *id);

    let mut ev = event.clone();
    let policy = source_policy_tx(tx, source_id).await?;
    policy.apply(&mut ev);
    // A restricted source's raw payload (full text, image URLs) is not kept.
    let payload = if policy.restricted() {
        redacted_raw()
    } else {
        raw.payload.clone()
    };
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
        refresh(tx, l, source_id, &ev).await?;
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
            set_image_source(tx, id, source_id, &ev, None).await?;
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
    .bind(&payload)
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
) -> sqlx::Result<Vec<(OverrideAction, Uuid)>> {
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
    source_id: i64,
    event: &NewEvent,
) -> sqlx::Result<()> {
    let prior = prior_image_url(tx, id).await?;
    let sql = update_from_values(REFRESH);
    bind_event(sqlx::query(AssertSqlSafe(sql)), event)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    set_image_source(tx, id, source_id, event, prior.as_deref()).await
}

async fn prior_image_url(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> sqlx::Result<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT image_url FROM events.events WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .flatten(),
    )
}

/// Record `source_id` as the provenance of event `id`'s image when the
/// stored `image_url` is now the one this source sent and it is new (or had
/// no recorded provenance), so identical URLs from two sources never
/// flip-flop the credit.
async fn set_image_source(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    source_id: i64,
    event: &NewEvent,
    prior_image_url: Option<&str>,
) -> sqlx::Result<()> {
    let Some(incoming) = event.image_url.as_deref() else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE events.events SET image_source_id = $2
         WHERE id = $1 AND image_url = $3
           AND (image_url IS DISTINCT FROM $4 OR image_source_id IS NULL)",
    )
    .bind(id)
    .bind(source_id)
    .bind(incoming)
    .bind(prior_image_url)
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
    let (kind, other_same_kind): (SourceKind, bool) = sqlx::query_as(
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
    let (site_wins, api_wins) = match kind {
        SourceKind::Scraper => (!other_same_kind, false),
        SourceKind::Api => (false, !other_same_kind),
        SourceKind::Aggregator => (false, false),
    };
    let prior = prior_image_url(tx, id).await?;
    let sql = update_from_values(MERGE);
    bind_event(sqlx::query(AssertSqlSafe(sql)), event)
        .bind(id)
        .bind(site_wins)
        .bind(api_wins)
        .execute(&mut **tx)
        .await?;
    set_image_source(tx, id, source_id, event, prior.as_deref()).await
}

/// A stored event.
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
    /// Materialised AI enrichment (`crate::enrich`); medium tags fall back to
    /// the sources' defaults.
    pub medium_tags: Vec<String>,
    pub format_tags: Vec<String>,
    pub good_for: Vec<String>,
    pub vibe_tags: Vec<String>,
    pub is_opening: Option<bool>,
    pub whats_cool: Option<String>,
    pub one_liner: Option<String>,
    pub ai_grounding: Option<String>,
    pub ai_model: Option<String>,
    pub ai_enriched_at: Option<DateTime<Utc>>,
}

const EVENT_COLS: &str = "id, title, description, venue_name, address, lat, lng, starts_at,
    ends_at, is_free, price_min, price_max, currency, url, image_url, category, tags, dedupe_key,
    medium_tags, format_tags, good_for, vibe_tags, is_opening, whats_cool, one_liner,
    ai_grounding, ai_model, ai_enriched_at";

pub async fn get_event(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<EventRow>> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {EVENT_COLS} FROM events.events WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// An event on a `GET /v1/events` page.
#[derive(Debug, Clone, FromRow)]
pub struct ListedEvent {
    #[sqlx(flatten)]
    pub event: EventRow,
    /// Set when ordering by distance.
    pub distance_km: Option<f64>,
}

/// `$1`..`$9` of every listing query (on `events.events ev`). An event with
/// an end (`ends_at` set), whatever its category, matches when its range
/// overlaps the window, a one-off when it starts inside it. `$4` is
/// `free_only`; `$5` (source keys) matches an event listed by ANY of those
/// sources; `$6` restricts to the given event ids; `$7`/`$8`/`$9` (medium,
/// format, good_for) match an event with ANY of the given tags.
const LISTING_FILTER: &str = "($1::timestamptz IS NULL OR COALESCE(ev.ends_at, ev.starts_at) >= $1)
    AND ($2::timestamptz IS NULL OR ev.starts_at < $2)
    AND (cardinality($3::text[]) = 0 OR ev.category = ANY($3))
    AND (NOT $4 OR ev.is_free)
    AND (cardinality($5::text[]) = 0 OR EXISTS (
        SELECT 1 FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
        WHERE es.event_id = ev.id AND s.key = ANY($5)))
    AND (cardinality($6::uuid[]) = 0 OR ev.id = ANY($6))
    AND (cardinality($7::text[]) = 0 OR ev.medium_tags && $7)
    AND (cardinality($8::text[]) = 0 OR ev.format_tags && $8)
    AND (cardinality($9::text[]) = 0 OR ev.good_for && $9)";

/// Bind `$1`..`$9` ([`LISTING_FILTER`]) for `f`.
fn bind_filter<'q, O>(q: PgQueryAs<'q, O>, f: &'q EventFilter) -> PgQueryAs<'q, O> {
    let categories: Vec<&'static str> = f.categories.iter().map(|c| c.as_str()).collect();
    q.bind(f.from)
        .bind(f.until)
        .bind(categories)
        .bind(f.free_only)
        .bind(&f.sources)
        .bind(&f.ids)
        .bind(&f.mediums)
        .bind(&f.formats)
        .bind(&f.good_for)
}

/// Haversine distance from (`$10`, `$11`) with Earth radius `$12`.
const DISTANCE_KM: &str = "2 * $12::float8 * asin(least(1, sqrt(
        power(sin(radians(lat - $10) / 2), 2)
        + cos(radians($10)) * cos(radians(lat))
          * power(sin(radians(lng - $11) / 2), 2))))";

/// The event's start in London wall-clock time.
const LOCAL_START: &str = "(ev.starts_at AT TIME ZONE 'Europe/London')";

/// Neither free nor priced.
const PRICE_UNKNOWN: &str = "(NOT ev.is_free AND ev.price_min IS NULL)";

/// `free=true` (`free`, a boolean) and `price_max=` (`max`, a numeric or
/// NULL for no limit) as SQL over `events.events ev`.
fn price_filter_sql(free: &str, max: &str) -> String {
    format!(
        "((NOT {free} OR ev.is_free) AND ({max}::numeric IS NULL OR ev.is_free
            OR (ev.price_min <= {max} AND COALESCE(ev.currency, 'GBP') = 'GBP')))"
    )
}

/// `price_max=` (`max`, a numeric or NULL for no limit) as SQL over
/// `events.events ev`; free events always pass. `free_only` itself is
/// already enforced by [`LISTING_FILTER`]'s `$4`.
fn price_max_sql(max: &str) -> String {
    format!(
        "({max}::numeric IS NULL OR ev.is_free
            OR (ev.price_min <= {max} AND COALESCE(ev.currency, 'GBP') = 'GBP'))"
    )
}

/// The [`When`] bucket as SQL over `events.events ev` (`TRUE` for none);
/// `weekend` clips to the window in `$1`/`$2`.
fn when_sql(when: Option<When>) -> String {
    let late = format!("({LOCAL_START}::time = '00:00' AND 'late opening' = ANY(ev.tags))");
    match when {
        None => "TRUE".into(),
        Some(When::Evening) => format!("({LOCAL_START}::time >= '18:00' OR {late})"),
        Some(When::AfterWork) => format!(
            "((extract(isodow FROM {LOCAL_START}) <= 5
                AND {LOCAL_START}::time BETWEEN '17:30' AND '20:30') OR {late})"
        ),
        Some(When::Daytime) => format!("({LOCAL_START}::time < '18:00')"),
        // At most the first 7 days of the clipped range need checking.
        Some(When::Weekend) => format!(
            "EXISTS (SELECT 1
                FROM (SELECT GREATEST({LOCAL_START}::date,
                                      ($1::timestamptz AT TIME ZONE 'Europe/London')::date) AS d0,
                             LEAST((COALESCE(ev.ends_at, ev.starts_at) AT TIME ZONE 'Europe/London')::date,
                                   ($2::timestamptz AT TIME ZONE 'Europe/London')::date - 1) AS d1) r,
                     generate_series(0, LEAST(r.d1 - r.d0, 6)) AS k
                WHERE extract(isodow FROM r.d0 + k) >= 6)"
        ),
    }
}

/// Haversine distance in km from the event to the point in placeholders
/// `$lat`/`$lng`, on a sphere of radius `$earth_radius`.
fn distance_km_sql(lat: usize, lng: usize, earth_radius: usize) -> String {
    format!(
        "2 * ${earth_radius}::float8 * asin(least(1, sqrt(
            power(sin(radians(lat - ${lat}) / 2), 2)
            + cos(radians(${lat})) * cos(radians(lat))
              * power(sin(radians(lng - ${lng}) / 2), 2))))"
    )
}

/// One page of events plus one more row (the caller's "has next page" probe):
/// `query.limit + 1` rows at most.
pub async fn list_events(pool: &PgPool, query: &EventQuery) -> sqlx::Result<Vec<ListedEvent>> {
    let f = &query.filter;
    let filter = format!(
        "{LISTING_FILTER} AND {} AND {}",
        price_max_sql("$10"),
        when_sql(f.when)
    );
    let fetch = query.limit + 1;
    match &query.order {
        EventOrder::ByStart { after } => {
            bind_filter(
                sqlx::query_as(AssertSqlSafe(format!(
                    "SELECT {EVENT_COLS}, NULL::float8 AS distance_km FROM events.events ev
                     WHERE {filter}
                       AND ($11::timestamptz IS NULL OR (starts_at, id) > ($11, $12::uuid))
                     ORDER BY starts_at, id LIMIT $13"
                ))),
                f,
            )
            .bind(f.price_max)
            .bind(after.map(|a| a.0))
            .bind(after.map(|a| a.1))
            .bind(fetch)
            .fetch_all(pool)
            .await
        }
        EventOrder::ByDistance { near, after } => {
            let b = near.bounding_box();
            bind_filter(
                sqlx::query_as(AssertSqlSafe(format!(
                    "SELECT * FROM (
                         SELECT {EVENT_COLS}, {} AS distance_km
                         FROM events.events ev
                         WHERE {filter}
                           AND lat BETWEEN $14 AND $15 AND lng BETWEEN $16 AND $17
                     ) e
                     WHERE distance_km <= $18
                       AND ($19::float8 IS NULL OR (distance_km, id) > ($19, $20::uuid))
                     ORDER BY distance_km, id LIMIT $21",
                    distance_km_sql(11, 12, 13)
                ))),
                f,
            )
            .bind(f.price_max)
            .bind(near.lat)
            .bind(near.lng)
            .bind(EARTH_RADIUS_KM)
            .bind(b.min_lat)
            .bind(b.max_lat)
            .bind(b.min_lng)
            .bind(b.max_lng)
            .bind(near.radius_km)
            .bind(after.map(|a| a.0))
            .bind(after.map(|a| a.1))
            .bind(fetch)
            .fetch_all(pool)
            .await
        }
    }
}

/// Which tag column a facet counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facet {
    Medium,
    Format,
    GoodFor,
}

impl Facet {
    pub const ALL: [Facet; 3] = [Facet::Medium, Facet::Format, Facet::GoodFor];

    pub fn column(self) -> &'static str {
        match self {
            Facet::Medium => "medium_tags",
            Facet::Format => "format_tags",
            Facet::GoodFor => "good_for",
        }
    }

    /// The API name (`medium`, `format`, `good_for`).
    pub fn name(self) -> &'static str {
        match self {
            Facet::Medium => "medium",
            Facet::Format => "format",
            Facet::GoodFor => "good_for",
        }
    }
}

/// For each tag of `facet`, how many events match `query`'s filters (and
/// area) with that facet's own selection ignored, so every count says what
/// choosing that tag would show. Tags with no events are omitted.
pub async fn facet_counts(
    pool: &PgPool,
    query: &EventQuery,
    facet: Facet,
) -> sqlx::Result<Vec<(String, i64)>> {
    let mut f = query.filter.clone();
    match facet {
        Facet::Medium => f.mediums.clear(),
        Facet::Format => f.formats.clear(),
        Facet::GoodFor => f.good_for.clear(),
    }
    let near = match &query.order {
        EventOrder::ByDistance { near, .. } => Some(*near),
        EventOrder::ByStart { .. } => None,
    };
    let b = near.map(|n| n.bounding_box());
    let col = facet.column();
    let filter = format!(
        "{LISTING_FILTER} AND {} AND {}",
        price_max_sql("$18"),
        when_sql(f.when)
    );
    bind_filter(
        sqlx::query_as(AssertSqlSafe(format!(
            "SELECT t, count(*) FROM events.events ev CROSS JOIN LATERAL unnest(ev.{col}) AS t
             WHERE {filter}
               AND ($10::float8 IS NULL OR (
                   lat BETWEEN $13 AND $14 AND lng BETWEEN $15 AND $16
                   AND {DISTANCE_KM} <= $17))
             GROUP BY t ORDER BY count(*) DESC, t"
        ))),
        &f,
    )
    .bind(near.map(|n| n.lat))
    .bind(near.map(|n| n.lng))
    .bind(EARTH_RADIUS_KM)
    .bind(b.map(|b| b.min_lat))
    .bind(b.map(|b| b.max_lat))
    .bind(b.map(|b| b.min_lng))
    .bind(b.map(|b| b.max_lng))
    .bind(near.map(|n| n.radius_km))
    .bind(f.price_max)
    .fetch_all(pool)
    .await
}

/// How many events each `when=` and price option would list, given the
/// other active filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromRow)]
pub struct ListingCounts {
    pub evening: i64,
    pub after_work: i64,
    pub weekend: i64,
    pub daytime: i64,
    pub free: i64,
    pub max_10: i64,
    pub max_20: i64,
    /// Neither free nor priced (left out by any `price_max`).
    pub unknown: i64,
}

/// Facet counts for a listing: each `when` count applies every filter in
/// `filter` except `when`, each price count every filter except the price
/// ones (`free_only`, `price_max`). `near` restricts to its radius. `$4` in
/// [`LISTING_FILTER`] is pinned to `false` (the free/price dimensions are
/// only applied per-count, below, via `$10`/`$11`) but the other filters
/// (dates, category, sources, ids, tags) still apply to the base rows.
pub async fn listing_counts(
    pool: &PgPool,
    filter: &EventFilter,
    near: Option<&Near>,
) -> sqlx::Result<ListingCounts> {
    let categories: Vec<&str> = filter.categories.iter().map(|c| c.as_str()).collect();
    let sources: Vec<&str> = filter.sources.iter().map(String::as_str).collect();
    let price = price_filter_sql("$10", "$11");
    let when = when_sql(filter.when);
    let when_count = |w: When| format!("count(*) FILTER (WHERE {} AND {price})", when_sql(Some(w)));
    let price_count = |p: &str| format!("count(*) FILTER (WHERE {p} AND {when})");
    let b = near.map(Near::bounding_box);
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {} AS evening, {} AS after_work, {} AS weekend, {} AS daytime,
                {} AS free, {} AS max_10, {} AS max_20, {} AS unknown
         FROM events.events ev
         WHERE {LISTING_FILTER}
           AND ($12::float8 IS NULL OR (lat BETWEEN $14 AND $15 AND lng BETWEEN $16 AND $17
                AND {} <= $19))",
        when_count(When::Evening),
        when_count(When::AfterWork),
        when_count(When::Weekend),
        when_count(When::Daytime),
        price_count(&price_filter_sql("TRUE", "NULL")),
        price_count(&price_filter_sql("FALSE", "10")),
        price_count(&price_filter_sql("FALSE", "20")),
        price_count(PRICE_UNKNOWN),
        distance_km_sql(12, 13, 18)
    )))
    .bind(filter.from)
    .bind(filter.until)
    .bind(&categories)
    .bind(false)
    .bind(&sources)
    .bind(&filter.ids)
    .bind(&filter.mediums)
    .bind(&filter.formats)
    .bind(&filter.good_for)
    .bind(filter.free_only)
    .bind(filter.price_max)
    .bind(near.map(|n| n.lat))
    .bind(near.map(|n| n.lng))
    .bind(b.map(|b| b.min_lat))
    .bind(b.map(|b| b.max_lat))
    .bind(b.map(|b| b.min_lng))
    .bind(b.map(|b| b.max_lng))
    .bind(EARTH_RADIUS_KM)
    .bind(near.map(|n| n.radius_km))
    .fetch_one(pool)
    .await
}

/// Up to `k` events matching `filter`, nearest to `query_vec` by cosine
/// distance (id, similarity), for hybrid search. Empty when embeddings are
/// off (`events.event_embeddings` missing). `query_vec` must come from the
/// same model as the stored embeddings (`crate::enrich::embed`).
pub async fn semantic_candidates(
    pool: &PgPool,
    query_vec: &[f32],
    filter: &EventFilter,
    k: i64,
) -> sqlx::Result<Vec<(Uuid, f64)>> {
    if !crate::enrich::store::embeddings_available(pool).await? {
        return Ok(Vec::new());
    }
    bind_filter(
        sqlx::query_as(AssertSqlSafe(format!(
            "SELECT ev.id, 1 - (em.embedding OPERATOR(extensions.<=>) q.v) AS similarity
             FROM events.event_embeddings em
             JOIN events.events ev ON ev.id = em.event_id
             CROSS JOIN (SELECT $10::real[]::extensions.vector AS v) q
             WHERE {LISTING_FILTER}
             ORDER BY em.embedding OPERATOR(extensions.<=>) q.v, ev.id
             LIMIT $11"
        ))),
        filter,
    )
    .bind(query_vec)
    .bind(k)
    .fetch_all(pool)
    .await
}

/// Where an event was found (one row per listing in `events.event_sources`).
#[derive(Debug, Clone, FromRow)]
pub struct EventSourceLink {
    pub event_id: Uuid,
    pub source: String,
    pub display_name: Option<String>,
    pub kind: SourceKind,
    pub source_url: Option<String>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

/// Source links of the given events, oldest listing first per event.
pub async fn event_source_links(
    pool: &PgPool,
    event_ids: &[Uuid],
) -> sqlx::Result<Vec<EventSourceLink>> {
    sqlx::query_as(
        "SELECT es.event_id, s.key AS source, s.display_name, s.kind, es.source_url,
                es.first_seen_at, es.last_seen_at
         FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
         WHERE es.event_id = ANY($1)
         ORDER BY es.event_id, es.first_seen_at, s.key, es.source_event_id",
    )
    .bind(event_ids)
    .fetch_all(pool)
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

/// Record a run, bump the source's `last_run_at` to the run start and clear
/// any recorded skip.
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
    sqlx::query(
        "UPDATE events.sources SET last_run_at = $2, skip_reason = NULL, skipped_at = NULL
         WHERE id = $1",
    )
    .bind(run.source_id)
    .bind(run.started_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Record that a due source could not be built; `last_run_at` is left alone
/// so it stays due.
pub async fn record_skip(
    pool: &PgPool,
    source_id: i64,
    reason: &str,
    at: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE events.sources SET skip_reason = $2, skipped_at = $3 WHERE id = $1")
        .bind(source_id)
        .bind(reason)
        .bind(at)
        .execute(pool)
        .await?;
    Ok(())
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

/// A source with its latest run and open health issue, for `GET /v1/sources`.
#[derive(Debug, Clone, FromRow)]
pub struct SourceStatusRow {
    pub key: String,
    pub display_name: Option<String>,
    pub kind: SourceKind,
    pub interval_minutes: i32,
    pub enabled: bool,
    pub run_started_at: Option<DateTime<Utc>>,
    pub run_events_found: Option<i32>,
    pub run_errors: Option<i32>,
    pub run_duration_ms: Option<i64>,
    pub run_ok: Option<bool>,
    pub open_issue_number: Option<i64>,
    pub skip_reason: Option<String>,
    pub skipped_at: Option<DateTime<Utc>>,
}

pub async fn source_statuses(pool: &PgPool) -> sqlx::Result<Vec<SourceStatusRow>> {
    sqlx::query_as(
        "SELECT s.key, s.display_name, s.kind, s.interval_minutes, s.enabled,
                r.started_at AS run_started_at, r.events_found AS run_events_found,
                r.errors AS run_errors, r.duration_ms AS run_duration_ms, r.ok AS run_ok,
                h.github_issue_number AS open_issue_number, s.skip_reason, s.skipped_at
         FROM events.sources s
         LEFT JOIN LATERAL (
             SELECT started_at, events_found, errors, duration_ms, ok
             FROM events.source_runs WHERE source_id = s.id
             ORDER BY started_at DESC, id DESC LIMIT 1
         ) r ON TRUE
         LEFT JOIN events.health_issues h ON h.source_id = s.id AND h.closed_at IS NULL
         ORDER BY s.key",
    )
    .fetch_all(pool)
    .await
}

/// A site suggestion about to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSuggestion {
    pub url: String,
    pub domain: String,
    pub note: Option<String>,
    pub submitter_ip_hash: String,
}

#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct SuggestionRow {
    pub id: i64,
    pub url: String,
    pub domain: String,
    pub note: Option<String>,
}

/// Serialise submissions from one client until the transaction ends, so
/// concurrent requests cannot all pass the rate-limit check.
pub async fn lock_suggestion_submitter(
    tx: &mut Transaction<'_, Postgres>,
    ip_hash: &str,
) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(ip_hash)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// When `limit` submissions from `ip_hash` already fall inside the last
/// `window`, the seconds until the oldest of those leaves it; else `None`.
pub async fn suggestion_retry_after(
    tx: &mut Transaction<'_, Postgres>,
    ip_hash: &str,
    window: std::time::Duration,
    limit: u32,
) -> sqlx::Result<Option<f64>> {
    sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM created_at + make_interval(secs => $2) - now())::float8
           FROM events.site_suggestions
          WHERE submitter_ip_hash = $1 AND created_at > now() - make_interval(secs => $2)
          ORDER BY created_at DESC
         OFFSET $3 - 1 LIMIT 1",
    )
    .bind(ip_hash)
    .bind(window.as_secs_f64())
    .bind(i64::from(limit))
    .fetch_optional(&mut **tx)
    .await
}

/// `(key, domain)` of every source, enabled or not.
pub async fn source_domains(
    tx: &mut Transaction<'_, Postgres>,
) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as("SELECT key, domain FROM events.sources ORDER BY key")
        .fetch_all(&mut **tx)
        .await
}

/// Insert a `pending` suggestion; `None` if the domain already has a
/// pending or accepted one.
pub async fn insert_pending_suggestion(
    tx: &mut Transaction<'_, Postgres>,
    s: &NewSuggestion,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "INSERT INTO events.site_suggestions (url, domain, note, submitter_ip_hash, status)
         VALUES ($1, $2, $3, $4, 'pending')
         ON CONFLICT (domain) WHERE status IN ('pending', 'accepted') DO NOTHING
         RETURNING id",
    )
    .bind(&s.url)
    .bind(&s.domain)
    .bind(&s.note)
    .bind(&s.submitter_ip_hash)
    .fetch_optional(&mut **tx)
    .await
}

pub async fn insert_duplicate_suggestion(
    tx: &mut Transaction<'_, Postgres>,
    s: &NewSuggestion,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO events.site_suggestions (url, domain, note, submitter_ip_hash, status)
         VALUES ($1, $2, $3, $4, 'duplicate')",
    )
    .bind(&s.url)
    .bind(&s.domain)
    .bind(&s.note)
    .bind(&s.submitter_ip_hash)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A site we decided not to scrape (`events.refused_sources`).
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct RefusedSourceRow {
    pub domain: String,
    pub name: String,
    pub url: String,
    pub reason_code: String,
    pub reason_text: String,
    pub checked_on: NaiveDate,
    pub issue_url: Option<String>,
}

const REFUSED_COLS: &str = "domain, name, url, reason_code, reason_text, checked_on, issue_url";

/// Every refused site, most recently checked first.
pub async fn refused_sources(pool: &PgPool) -> sqlx::Result<Vec<RefusedSourceRow>> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {REFUSED_COLS} FROM events.refused_sources ORDER BY checked_on DESC, name"
    )))
    .fetch_all(pool)
    .await
}

/// Refused sites, inside a suggestion transaction.
pub async fn refused_source_list(
    tx: &mut Transaction<'_, Postgres>,
) -> sqlx::Result<Vec<RefusedSourceRow>> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {REFUSED_COLS} FROM events.refused_sources ORDER BY domain"
    )))
    .fetch_all(&mut **tx)
    .await
}

/// Record a suggestion for a refused site (counts toward the rate limit;
/// never filed).
pub async fn insert_refused_suggestion(
    tx: &mut Transaction<'_, Postgres>,
    s: &NewSuggestion,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO events.site_suggestions (url, domain, note, submitter_ip_hash, status)
         VALUES ($1, $2, $3, $4, 'refused')",
    )
    .bind(&s.url)
    .bind(&s.domain)
    .bind(&s.note)
    .bind(&s.submitter_ip_hash)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Record the GitHub issue filed for a pending suggestion.
pub async fn mark_suggestion_filed(pool: &PgPool, id: i64, issue: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE events.site_suggestions SET status = 'accepted', github_issue_number = $2
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(issue)
    .execute(pool)
    .await?;
    Ok(())
}

/// Pending suggestions created before `before`, oldest first.
pub async fn pending_suggestions(
    pool: &PgPool,
    before: DateTime<Utc>,
) -> sqlx::Result<Vec<SuggestionRow>> {
    sqlx::query_as(
        "SELECT id, url, domain, note FROM events.site_suggestions
          WHERE status = 'pending' AND created_at < $1
          ORDER BY created_at",
    )
    .bind(before)
    .fetch_all(pool)
    .await
}

// ------------------------------------------------------------ content policy

/// What [`enforce_content_policy`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PolicyReport {
    pub descriptions_cleared: u64,
    pub descriptions_trimmed: u64,
    pub images_cleared: u64,
    pub thumbnails_deleted: u64,
    pub raw_redacted: u64,
}

/// Bring stored rows in line with the current content policy (idempotent,
/// cheap when nothing changed; run by every ingest tick):
///
/// * images whose provenance source may not be stored are cleared;
/// * descriptions of events whose linked sources ALL forbid storing them are
///   cleared (a merged event may hold another source's text);
/// * descriptions longer than the excerpt rule are trimmed with
///   [`crate::normalise::excerpt`];
/// * thumbnails whose event lost its image or whose source may no longer be
///   stored are deleted;
/// * raw payloads of restricted sources are replaced by [`redacted_raw`].
pub async fn enforce_content_policy(pool: &PgPool) -> sqlx::Result<PolicyReport> {
    let images_cleared = sqlx::query(
        "UPDATE events.events e SET image_url = NULL, image_source_id = NULL
         FROM events.sources s
         WHERE s.id = e.image_source_id AND NOT s.store_image",
    )
    .execute(pool)
    .await?
    .rows_affected();
    let mut report = PolicyReport {
        images_cleared,
        ..PolicyReport::default()
    };
    report.descriptions_cleared = sqlx::query(
        "UPDATE events.events e SET description = NULL
         WHERE e.description IS NOT NULL
           AND EXISTS (SELECT 1 FROM events.event_sources es WHERE es.event_id = e.id)
           AND NOT EXISTS (SELECT 1 FROM events.event_sources es
                           JOIN events.sources s ON s.id = es.source_id
                           WHERE es.event_id = e.id AND s.store_description)",
    )
    .execute(pool)
    .await?
    .rows_affected();
    let long: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, description FROM events.events WHERE char_length(description) > $1",
    )
    .bind(i32::try_from(crate::normalise::EXCERPT_MAX_CHARS).unwrap_or(i32::MAX))
    .fetch_all(pool)
    .await?;
    for (id, d) in long {
        report.descriptions_trimmed += sqlx::query(
            "UPDATE events.events SET description = $2 WHERE id = $1 AND description = $3",
        )
        .bind(id)
        .bind(crate::normalise::excerpt(&d))
        .bind(&d)
        .execute(pool)
        .await?
        .rows_affected();
    }
    report.raw_redacted = sqlx::query(
        "UPDATE events.event_sources es SET raw = $1
         FROM events.sources s
         WHERE s.id = es.source_id AND (NOT s.store_description OR NOT s.store_image)
           AND es.raw <> $1",
    )
    .bind(redacted_raw())
    .execute(pool)
    .await?
    .rows_affected();
    report.thumbnails_deleted = sqlx::query(
        "DELETE FROM events.thumbnails t
         USING events.events e
         WHERE t.event_id = e.id
           AND (e.image_url IS NULL
                OR EXISTS (SELECT 1 FROM events.sources s
                           WHERE s.id = t.source_id AND NOT s.store_image))",
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(report)
}

// ---------------------------------------------------------------- thumbnails

/// An event whose image needs a (new) thumbnail.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct ThumbnailJob {
    pub event_id: Uuid,
    pub image_url: String,
    pub source_id: i64,
}

/// Current or upcoming events with an image from a source that allows
/// storing images and no thumbnail for that exact image URL yet (failed
/// attempts are retried after `retry_failed_before`). Soonest first.
pub async fn thumbnail_jobs(
    pool: &PgPool,
    now: DateTime<Utc>,
    retry_failed_before: DateTime<Utc>,
    limit: i64,
) -> sqlx::Result<Vec<ThumbnailJob>> {
    sqlx::query_as(
        "SELECT e.id AS event_id, e.image_url, e.image_source_id AS source_id
         FROM events.events e
         JOIN events.sources s ON s.id = e.image_source_id AND s.store_image
         LEFT JOIN events.thumbnails t ON t.event_id = e.id
         WHERE e.image_url IS NOT NULL
           AND COALESCE(e.ends_at, e.starts_at) >= $1 - interval '1 day'
           AND (t.event_id IS NULL
                OR t.source_image_url <> e.image_url
                OR (t.bytes IS NULL AND t.fetched_at < $2))
         ORDER BY e.starts_at, e.id
         LIMIT $3",
    )
    .bind(now)
    .bind(retry_failed_before)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// A thumbnail made by the thumbnailer.
#[derive(Debug, Clone)]
pub struct NewThumbnail {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub width: i32,
    pub height: i32,
    pub content_hash: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Store the thumbnail (`Ok`) or the failure (`Err(message)`) for a job.
pub async fn save_thumbnail(
    pool: &PgPool,
    job: &ThumbnailJob,
    result: Result<&NewThumbnail, &str>,
) -> sqlx::Result<()> {
    let (t, error) = match result {
        Ok(t) => (Some(t), None),
        Err(e) => (None, Some(e)),
    };
    sqlx::query(
        "INSERT INTO events.thumbnails (event_id, source_id, source_image_url, bytes, content_type,
             width, height, content_hash, etag, last_modified, error, fetched_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now())
         ON CONFLICT (event_id) DO UPDATE SET
             source_id = EXCLUDED.source_id, source_image_url = EXCLUDED.source_image_url,
             bytes = EXCLUDED.bytes, content_type = EXCLUDED.content_type,
             width = EXCLUDED.width, height = EXCLUDED.height,
             content_hash = EXCLUDED.content_hash, etag = EXCLUDED.etag,
             last_modified = EXCLUDED.last_modified, error = EXCLUDED.error,
             fetched_at = EXCLUDED.fetched_at",
    )
    .bind(job.event_id)
    .bind(job.source_id)
    .bind(&job.image_url)
    .bind(t.map(|t| &t.bytes))
    .bind(t.map(|t| &t.content_type))
    .bind(t.map(|t| t.width))
    .bind(t.map(|t| t.height))
    .bind(t.map(|t| &t.content_hash))
    .bind(t.and_then(|t| t.etag.as_ref()))
    .bind(t.and_then(|t| t.last_modified.as_ref()))
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

/// A stored thumbnail's bytes, for `GET /thumbs/...`.
#[derive(Debug, Clone, FromRow)]
pub struct ThumbnailBytes {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub content_hash: String,
}

pub async fn get_thumbnail(pool: &PgPool, event_id: Uuid) -> sqlx::Result<Option<ThumbnailBytes>> {
    sqlx::query_as(
        "SELECT t.bytes, t.content_type, t.content_hash FROM events.thumbnails t
         LEFT JOIN events.sources s ON s.id = t.source_id
         WHERE t.event_id = $1 AND t.bytes IS NOT NULL AND COALESCE(s.store_image, FALSE)",
    )
    .bind(event_id)
    .fetch_optional(pool)
    .await
}

/// What pages and JSON need to show a thumbnail and its credit (no bytes).
#[derive(Debug, Clone, FromRow)]
pub struct ThumbnailMeta {
    pub event_id: Uuid,
    pub content_hash: String,
    pub width: i32,
    pub height: i32,
    pub credit_key: String,
    pub credit_display_name: Option<String>,
    /// The listing's page on that source, else the source's site.
    pub credit_url: String,
}

pub async fn thumbnail_meta(pool: &PgPool, event_ids: &[Uuid]) -> sqlx::Result<Vec<ThumbnailMeta>> {
    sqlx::query_as(
        "SELECT t.event_id, t.content_hash, t.width, t.height,
                s.key AS credit_key, s.display_name AS credit_display_name,
                COALESCE((SELECT es.source_url FROM events.event_sources es
                          WHERE es.event_id = t.event_id AND es.source_id = t.source_id
                            AND es.source_url IS NOT NULL
                          ORDER BY es.first_seen_at LIMIT 1), s.base_url) AS credit_url
         FROM events.thumbnails t JOIN events.sources s ON s.id = t.source_id
         WHERE t.event_id = ANY($1) AND t.bytes IS NOT NULL AND s.store_image",
    )
    .bind(event_ids)
    .fetch_all(pool)
    .await
}

/// `(key, display name)` of every source, for filter chips.
pub async fn source_names(pool: &PgPool) -> sqlx::Result<Vec<(String, String)>> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT key, display_name FROM events.sources ORDER BY key")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(k, d)| {
            let name = display_name(&k, d.as_deref());
            (k, name)
        })
        .collect())
}
