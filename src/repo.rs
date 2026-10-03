//! Database access. All SQL is schema-qualified with `events.`.
//!
//! Runtime-checked queries only (`sqlx::query*`), no compile-time macros, so
//! builds never need a live database.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::{AssertSqlSafe, FromRow, PgPool, Postgres, Transaction};
use std::collections::HashSet;
use uuid::Uuid;

use crate::enrich::input::PageText;
use crate::listing::{
    EARTH_RADIUS_KM, EventFilter, EventOrder, EventQuery, Near, Pick, PickFilter, When,
};
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
    /// Shared implementation to build instead of dispatching on `key`.
    pub platform: Option<String>,
    /// The platform's per-venue settings.
    pub config: Option<serde_json::Value>,
    /// The source's normal state can be zero upcoming events (e.g. a calendar
    /// that only lists its next meetup), so the health checker's zero-events
    /// rule does not apply to it.
    pub may_be_empty: bool,
}

const SOURCE_COLS: &str = "id, key, kind, base_url, domain, interval_minutes, enabled, last_run_at, platform, config, may_be_empty";

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
        is_free, price_min, price_max, currency, url, image_url, category, tags, dedupe_key,
        all_day)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)";

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
        .bind(e.all_day)
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
        .bind(e.all_day)
}

/// Merge used when a DIFFERENT source reports an event we already have
/// (`x` is the existing row, `n` the incoming values). Existing values win
/// and the newcomer only fills gaps, except where the incoming source has
/// precedence: `$20` (a venue site) wins dates, description and image; `$21`
/// (an API) wins price and URL. Tags are unioned. The `ends_at` guards keep
/// `events_ends_after_start` true when fuzzy-merged start dates differ.
const MERGE: &str = "
    description = CASE WHEN $20::bool THEN COALESCE(n.description, x.description)
                       ELSE COALESCE(x.description, n.description) END,
    venue_name  = COALESCE(x.venue_name, n.venue_name),
    address     = COALESCE(x.address, n.address),
    lat         = COALESCE(x.lat, n.lat),
    lng         = COALESCE(x.lng, n.lng),
    starts_at   = CASE WHEN $20::bool THEN n.starts_at ELSE x.starts_at END,
    all_day     = CASE WHEN $20::bool THEN n.all_day ELSE x.all_day END,
    ends_at     = CASE WHEN $20::bool
                       THEN (CASE WHEN n.ends_at IS NOT NULL THEN n.ends_at
                                  WHEN x.ends_at >= n.starts_at THEN x.ends_at END)
                       ELSE COALESCE(x.ends_at,
                                     CASE WHEN n.ends_at >= x.starts_at THEN n.ends_at END) END,
    is_free     = CASE WHEN $21::bool AND (n.is_free OR n.price_min IS NOT NULL
                                           OR n.price_max IS NOT NULL) THEN n.is_free
                       WHEN x.price_min IS NULL AND x.price_max IS NULL AND NOT x.is_free
                       THEN n.is_free ELSE x.is_free END,
    price_min   = CASE WHEN $21::bool AND (n.is_free OR n.price_min IS NOT NULL
                                           OR n.price_max IS NOT NULL) THEN n.price_min
                       WHEN x.price_min IS NULL AND x.price_max IS NULL AND NOT x.is_free
                       THEN n.price_min ELSE x.price_min END,
    price_max   = CASE WHEN $21::bool AND (n.is_free OR n.price_min IS NOT NULL
                                           OR n.price_max IS NOT NULL) THEN n.price_max
                       WHEN x.price_min IS NULL AND x.price_max IS NULL AND NOT x.is_free
                       THEN n.price_max ELSE x.price_max END,
    currency    = CASE WHEN $21::bool AND (n.is_free OR n.price_min IS NOT NULL
                                           OR n.price_max IS NOT NULL)
                       THEN COALESCE(n.currency, x.currency)
                       ELSE COALESCE(x.currency, n.currency) END,
    url         = CASE WHEN $21::bool THEN COALESCE(n.url, x.url) ELSE COALESCE(x.url, n.url) END,
    image_url   = CASE WHEN $20::bool THEN COALESCE(n.image_url, x.image_url)
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
    all_day     = n.all_day,
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

/// `UPDATE events.events x SET <set> FROM (VALUES ...) n(...) WHERE x.id = $19`.
fn update_from_values(set: &str) -> String {
    format!(
        "UPDATE events.events AS x SET {set}
         FROM (VALUES ($1::text, $2::text, $3::text, $4::text, $5::float8, $6::float8,
                       $7::timestamptz, $8::timestamptz, $9::bool, $10::numeric, $11::numeric,
                       $12::text, $13::text, $14::text, $15::text, $16::text[], $17::text,
                       $18::bool))
              AS n(title, description, venue_name, address, lat, lng, starts_at, ends_at,
                   is_free, price_min, price_max, currency, url, image_url, category, tags,
                   dedupe_key, all_day)
         WHERE x.id = $19"
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
/// The venue-name resolver (`crate::venues::Resolver`) over
/// `events.venue_aliases`.
async fn venue_resolver<'e, E>(db: E) -> sqlx::Result<crate::venues::Resolver>
where
    E: sqlx::PgExecutor<'e>,
{
    let aliases: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, venue_name FROM events.venue_aliases ORDER BY name")
            .fetch_all(db)
            .await?;
    Ok(crate::venues::Resolver::new(
        aliases.iter().map(|(n, v)| (n.as_str(), v.as_deref())),
    ))
}

/// The key (`crate::venues::Resolver`) of the venue a listing's venue name
/// belongs to, if any.
async fn venue_key_tx(
    tx: &mut Transaction<'_, Postgres>,
    venue: Option<&str>,
) -> sqlx::Result<Option<String>> {
    if venue.is_none_or(|v| v.trim().is_empty()) {
        return Ok(None);
    }
    Ok(match venue_resolver(&mut **tx).await?.resolve(venue) {
        crate::venues::Resolved::Venue(key) => Some(key),
        crate::venues::Resolved::None => None,
    })
}

/// Coordinates for a venue from `events.venues`, matched on the normalised
/// venue name (`normalise::normalise_venue_for_key`, the dedupe key's venue
/// part) through `events.venue_aliases`. The table is small (one row per
/// venue), so it is read whole.
async fn venue_coords_tx(
    tx: &mut Transaction<'_, Postgres>,
    venue: Option<&str>,
) -> sqlx::Result<Option<(f64, f64)>> {
    let Some(key) = venue_key_tx(tx, venue).await? else {
        return Ok(None);
    };
    let rows: Vec<(String, f64, f64)> = sqlx::query_as(
        "SELECT name, lat, lng FROM events.venues WHERE lat IS NOT NULL ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .find(|(name, _, _)| crate::normalise::normalise_venue_for_key(Some(name)) == key)
        .map(|(_, lat, lng)| (lat, lng)))
}

/// What [`sync_venues`] changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VenueSync {
    /// Venues created from events' venue names.
    pub created: usize,
    /// Venues whose slug, address, postcode, coordinates, borough or hours
    /// were filled or refreshed.
    pub updated: usize,
    /// Events whose `venue_id` changed.
    pub linked: u64,
    /// Events without coordinates that got their venue's.
    pub located: u64,
}

#[derive(FromRow)]
struct VenueRow {
    id: i64,
    name: String,
    slug: Option<String>,
    address: Option<String>,
    postcode: Option<String>,
    lat: Option<f64>,
    lng: Option<f64>,
    borough: Option<String>,
    opening_hours: Option<serde_json::Value>,
}

/// Venues as first-class objects (#204; see the `first_class_venues`
/// migration): create a venue for every venue name on the events that has
/// none, link every event to its venue (`venue_id`), fill venues' missing
/// slug/address/postcode/coordinates/hours (from their events and
/// `events.venue_hours`), their borough from their coordinates, and events'
/// missing coordinates from their venue. Deterministic, idempotent, one
/// transaction; runs after each ingest run (before the borough sync).
pub async fn sync_venues(pool: &PgPool) -> sqlx::Result<VenueSync> {
    use std::collections::{BTreeMap, HashMap};
    type EventRow = (
        Uuid,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<f64>,
        Option<i64>,
    );

    let mut out = VenueSync::default();
    let mut tx = pool.begin().await?;
    // One sync at a time (the ingest lock already ensures it; this is cheap).
    sqlx::query("LOCK TABLE events.venues IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let resolver = venue_resolver(&mut *tx).await?;
    let mut venues: Vec<VenueRow> = sqlx::query_as(
        "SELECT id, name, slug, address, postcode, lat, lng, borough, opening_hours
         FROM events.venues ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let events: Vec<EventRow> = sqlx::query_as(
        "SELECT id, venue_name, address, lat, lng, venue_id FROM events.events ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let hours: Vec<(String, serde_json::Value, String)> = sqlx::query_as(
        "SELECT name, opening_hours, hours_source FROM events.venue_hours ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;

    // Venue key -> index in `venues` (the oldest row wins a shared key).
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (i, v) in venues.iter().enumerate() {
        by_key
            .entry(crate::normalise::normalise_venue_for_key(Some(&v.name)))
            .or_insert(i);
    }
    // Events grouped by venue key (sorted, so creation order is stable).
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, e) in events.iter().enumerate() {
        if let crate::venues::Resolved::Venue(key) = resolver.resolve(e.1.as_deref()) {
            groups.entry(key).or_default().push(i);
        }
    }
    let group_names = |idx: &[usize]| {
        crate::venues::most_common(idx.iter().filter_map(|&i| events[i].1.as_deref()))
            .map(str::to_string)
    };
    let group_address = |idx: &[usize]| {
        crate::venues::most_common(idx.iter().filter_map(|&i| events[i].2.as_deref()))
            .map(str::to_string)
    };
    let group_point = |idx: &[usize]| {
        crate::venues::most_common_point(idx.iter().filter_map(|&i| events[i].3.zip(events[i].4)))
    };

    // 1. Create the missing venues.
    for (key, idx) in &groups {
        if by_key.contains_key(key) {
            continue;
        }
        let Some(name) = group_names(idx) else {
            continue;
        };
        let point = group_point(idx);
        let row: VenueRow = sqlx::query_as(
            "INSERT INTO events.venues (name, address, lat, lng, coords_source)
             VALUES ($1, $2, $3, $4, CASE WHEN $3::float8 IS NOT NULL THEN 'listing' END)
             RETURNING id, name, slug, address, postcode, lat, lng, borough, opening_hours",
        )
        .bind(&name)
        .bind(group_address(idx))
        .bind(point.map(|p| p.0))
        .bind(point.map(|p| p.1))
        .fetch_one(&mut *tx)
        .await?;
        by_key.insert(key.clone(), venues.len());
        venues.push(row);
        out.created += 1;
    }

    // 2. Fill each venue's gaps.
    let mut hours_by_key: HashMap<String, (&serde_json::Value, &str)> = HashMap::new();
    for (name, h, src) in &hours {
        if let crate::venues::Resolved::Venue(key) = resolver.resolve(Some(name)) {
            hours_by_key.entry(key).or_insert((h, src.as_str()));
        }
    }
    let mut slugs: std::collections::HashSet<String> =
        venues.iter().filter_map(|v| v.slug.clone()).collect();
    let key_of: HashMap<i64, String> = by_key
        .iter()
        .map(|(k, &i)| (venues[i].id, k.clone()))
        .collect();
    for v in &mut venues {
        let key = key_of.get(&v.id).cloned();
        let idx: &[usize] = key
            .as_ref()
            .and_then(|k| groups.get(k))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let slug = match &v.slug {
            Some(_) => None,
            None => {
                let s = crate::venues::unique_slug(&crate::venues::slugify(&v.name), |s| {
                    slugs.contains(s)
                });
                slugs.insert(s.clone());
                Some(s)
            }
        };
        let address = v.address.is_none().then(|| group_address(idx)).flatten();
        let postcode = match (&v.postcode, address.as_deref().or(v.address.as_deref())) {
            (None, Some(a)) => crate::venues::postcode(a),
            _ => None,
        };
        let point = v.lat.is_none().then(|| group_point(idx)).flatten();
        let (lat, lng) = match point {
            Some(p) => (Some(p.0), Some(p.1)),
            None => (v.lat, v.lng),
        };
        let borough = crate::borough::of(lat, lng).map(str::to_string);
        let borough_changed = borough != v.borough;
        let hours = match (
            &v.opening_hours,
            key.as_ref().and_then(|k| hours_by_key.get(k)),
        ) {
            (None, Some(h)) => Some(*h),
            _ => None,
        };
        if slug.is_none()
            && address.is_none()
            && postcode.is_none()
            && point.is_none()
            && !borough_changed
            && hours.is_none()
        {
            continue;
        }
        sqlx::query(
            "UPDATE events.venues SET
                slug = COALESCE(slug, $2),
                address = COALESCE(address, $3),
                postcode = COALESCE(postcode, $4),
                lat = COALESCE(lat, $5), lng = COALESCE(lng, $6),
                coords_source = CASE WHEN lat IS NULL AND $5::float8 IS NOT NULL
                                     THEN 'listing' ELSE coords_source END,
                borough = $7,
                opening_hours = COALESCE(opening_hours, $8),
                hours_source = CASE WHEN opening_hours IS NULL AND $8::jsonb IS NOT NULL
                                    THEN $9 ELSE hours_source END,
                updated_at = now()
             WHERE id = $1",
        )
        .bind(v.id)
        .bind(&slug)
        .bind(&address)
        .bind(&postcode)
        .bind(point.map(|p| p.0))
        .bind(point.map(|p| p.1))
        .bind(&borough)
        .bind(hours.map(|h| h.0))
        .bind(hours.map(|h| h.1))
        .execute(&mut *tx)
        .await?;
        (v.lat, v.lng) = (lat, lng);
        out.updated += 1;
    }

    // 3. Link events to their venues, and locate the ones without a point.
    let (mut ids, mut venue_ids) = (Vec::new(), Vec::<Option<i64>>::new());
    let (mut loc_ids, mut lats, mut lngs) = (Vec::new(), Vec::new(), Vec::new());
    for (key, idx) in &groups {
        let Some(v) = by_key.get(key).map(|&i| &venues[i]) else {
            continue;
        };
        for &i in idx {
            let e = &events[i];
            if e.5 != Some(v.id) {
                ids.push(e.0);
                venue_ids.push(Some(v.id));
            }
            if let (None, Some(lat), Some(lng)) = (e.3, v.lat, v.lng) {
                loc_ids.push(e.0);
                lats.push(lat);
                lngs.push(lng);
            }
        }
    }
    let grouped: std::collections::HashSet<usize> = groups.values().flatten().copied().collect();
    for (i, e) in events.iter().enumerate() {
        if e.5.is_some() && !grouped.contains(&i) {
            ids.push(e.0);
            venue_ids.push(None);
        }
    }
    if !ids.is_empty() {
        out.linked = sqlx::query(
            "UPDATE events.events ev SET venue_id = u.v
             FROM unnest($1::uuid[], $2::int8[]) AS u(id, v) WHERE ev.id = u.id",
        )
        .bind(&ids)
        .bind(&venue_ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    if !loc_ids.is_empty() {
        out.located = sqlx::query(
            "UPDATE events.events ev SET lat = u.lat, lng = u.lng
             FROM unnest($1::uuid[], $2::float8[], $3::float8[]) AS u(id, lat, lng)
             WHERE ev.id = u.id AND ev.lat IS NULL",
        )
        .bind(&loc_ids)
        .bind(&lats)
        .bind(&lngs)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(out)
}

/// Venues without coordinates that have events and a postcode not looked
/// up since `checked_before` (`crate::geocode`), oldest first.
pub async fn venues_to_geocode(
    pool: &PgPool,
    checked_before: DateTime<Utc>,
    limit: usize,
) -> sqlx::Result<Vec<(i64, String)>> {
    sqlx::query_as(
        "SELECT v.id, v.postcode FROM events.venues v
         WHERE v.lat IS NULL AND v.postcode IS NOT NULL
           AND (v.geocode_checked_at IS NULL OR v.geocode_checked_at < $1)
           AND EXISTS (SELECT 1 FROM events.events ev WHERE ev.venue_id = v.id)
         ORDER BY v.id LIMIT $2",
    )
    .bind(checked_before)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
}

/// Store a geocoded point for a venue that still has none.
pub async fn set_venue_point(
    pool: &PgPool,
    id: i64,
    lat: f64,
    lng: f64,
    source: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE events.venues SET lat = $2, lng = $3, coords_source = $4,
             geocode_checked_at = NULL, updated_at = now()
         WHERE id = $1 AND lat IS NULL",
    )
    .bind(id)
    .bind(lat)
    .bind(lng)
    .bind(source)
    .execute(pool)
    .await?;
    Ok(())
}

/// Remember a postcode lookup that found nothing.
pub async fn venue_geocode_checked(pool: &PgPool, id: i64, now: DateTime<Utc>) -> sqlx::Result<()> {
    sqlx::query("UPDATE events.venues SET geocode_checked_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}

/// Names of venues without coordinates that have events still to come or
/// still running at `now` (the #204 invariant), by name.
pub async fn venues_without_coords(pool: &PgPool, now: DateTime<Utc>) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT v.name FROM events.venues v
         WHERE v.lat IS NULL
           AND EXISTS (SELECT 1 FROM events.events ev
                       WHERE ev.venue_id = v.id AND COALESCE(ev.ends_at, ev.starts_at) >= $1)
         ORDER BY v.name",
    )
    .bind(now)
    .fetch_all(pool)
    .await
}

/// Set every event's `venue_type` from [`crate::venue_type::classify`]
/// (overrides from `events.venues`, its sources' keys, its venue name).
/// Returns how many events changed. Run after each ingest run.
pub async fn sync_venue_types(pool: &PgPool) -> sqlx::Result<u64> {
    let overrides: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, venue_type FROM events.venues WHERE venue_type IS NOT NULL ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    let overrides =
        crate::venue_type::Overrides::new(overrides.iter().map(|(n, t)| (n.as_str(), t.as_str())));
    let events: Vec<(Uuid, Option<String>, String, Vec<String>)> = sqlx::query_as(
        "SELECT ev.id, ev.venue_name, ev.venue_type,
                ARRAY(SELECT s.key FROM events.event_sources es
                      JOIN events.sources s ON s.id = es.source_id
                      WHERE es.event_id = ev.id ORDER BY s.key)
         FROM events.events ev",
    )
    .fetch_all(pool)
    .await?;
    let (mut ids, mut types) = (Vec::new(), Vec::new());
    for (id, venue, current, keys) in &events {
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let t = crate::venue_type::classify(venue.as_deref(), &keys, &overrides);
        if t != current {
            ids.push(*id);
            types.push(t);
        }
    }
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(sqlx::query(
        "UPDATE events.events ev SET venue_type = u.t
         FROM unnest($1::uuid[], $2::text[]) AS u(id, t) WHERE ev.id = u.id",
    )
    .bind(&ids)
    .bind(&types)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Set every event's `music_tags` from [`crate::music::tags_for`] (its
/// category, source tags and title; empty unless it is `music`). Returns
/// how many events changed. Run after each ingest run.
pub async fn sync_music_tags(pool: &PgPool) -> sqlx::Result<u64> {
    type Row = (Uuid, String, Vec<String>, String, Vec<String>);
    let events: Vec<Row> =
        sqlx::query_as("SELECT id, category, tags, title, music_tags FROM events.events")
            .fetch_all(pool)
            .await?;
    let (mut ids, mut tags) = (Vec::new(), Vec::new());
    for (id, category, source_tags, title, current) in &events {
        let t = crate::music::tags_for(category, source_tags, title);
        if t != *current {
            ids.push(*id);
            // Postgres arrays of arrays must be rectangular: send each
            // event's tags as one comma-joined string, split in SQL.
            tags.push(t.join(","));
        }
    }
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(sqlx::query(
        "UPDATE events.events ev
         SET music_tags = CASE WHEN u.t = '' THEN '{}'::text[] ELSE string_to_array(u.t, ',') END
         FROM unnest($1::uuid[], $2::text[]) AS u(id, t) WHERE ev.id = u.id",
    )
    .bind(&ids)
    .bind(&tags)
    .execute(pool)
    .await?
    .rows_affected())
}

pub async fn upsert_event(
    pool: &PgPool,
    source_id: i64,
    event: &NewEvent,
    raw: &RawEvent,
) -> sqlx::Result<UpsertOutcome> {
    Ok(upsert_listing(pool, source_id, event, raw).await?.0)
}

/// [`upsert_event`], also returning the listing's page text (issue #208)
/// when it is the text behind the event's stored excerpt, for the AI
/// enrichment of the same ingest run. Only its hash is stored
/// (`events.events.page_text_hash`); the text itself never is.
pub async fn upsert_listing(
    pool: &PgPool,
    source_id: i64,
    event: &NewEvent,
    raw: &RawEvent,
) -> sqlx::Result<(UpsertOutcome, Option<PageText>)> {
    let mut tx = pool.begin().await?;
    let out = upsert_event_tx(&mut tx, source_id, event, raw).await?;
    tx.commit().await?;
    Ok(out)
}

async fn upsert_event_tx(
    tx: &mut Transaction<'_, Postgres>,
    source_id: i64,
    event: &NewEvent,
    raw: &RawEvent,
) -> sqlx::Result<(UpsertOutcome, Option<PageText>)> {
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
    if ev.lat.is_none() || ev.lng.is_none() {
        if let Some((lat, lng)) = venue_coords_tx(tx, ev.venue_name.as_deref()).await? {
            ev.lat = Some(lat);
            ev.lng = Some(lng);
        }
    }
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
            set_sessions(tx, id, &ev).await?;
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
    set_hours_tx(tx, outcome.event_id, event, raw, policy).await?;
    set_borough_tx(tx, outcome.event_id).await?;
    let page = set_page_text_hash_tx(tx, outcome.event_id, event, &ev, policy).await?;
    Ok((outcome, page))
}

/// Record the hash of this listing's page text (its full description,
/// before the excerpt cut) when the event's stored excerpt is this
/// listing's, so a merged event follows one listing (the one merge
/// precedence picked for the description) and its hash doesn't flip between
/// sources. `''` = the listing has no text beyond the excerpt. Sources whose
/// terms don't let us keep descriptions provide no page text. Returns the
/// text when it is the event's current page text.
async fn set_page_text_hash_tx(
    tx: &mut Transaction<'_, Postgres>,
    event_id: Uuid,
    event: &NewEvent,
    stored: &NewEvent,
    policy: SourcePolicy,
) -> sqlx::Result<Option<PageText>> {
    let page = if policy.store_description {
        PageText::from_listing(event.description.as_deref(), stored.description.as_deref())
    } else {
        None
    };
    let hash = page.as_ref().map(PageText::hash).unwrap_or_default();
    let matched = sqlx::query(
        "UPDATE events.events SET page_text_hash = $2
         WHERE id = $1 AND description IS NOT DISTINCT FROM $3",
    )
    .bind(event_id)
    .bind(&hash)
    .bind(&stored.description)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    Ok(page.filter(|_| matched))
}

/// The London borough (issue #79, `crate::borough`) of the event this
/// listing landed in, from the row's final coordinates (merging keeps
/// existing ones, so they may not be the listing's).
async fn set_borough_tx(tx: &mut Transaction<'_, Postgres>, event_id: Uuid) -> sqlx::Result<()> {
    let (lat, lng): (Option<f64>, Option<f64>) =
        sqlx::query_as("SELECT lat, lng FROM events.events WHERE id = $1")
            .bind(event_id)
            .fetch_one(&mut **tx)
            .await?;
    sqlx::query(
        "UPDATE events.events SET borough = $2
         WHERE id = $1 AND borough IS DISTINCT FROM $2",
    )
    .bind(event_id)
    .bind(crate::borough::of(lat, lng))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Set every event's `borough` from its coordinates
/// ([`crate::borough::of`]). Returns how many events changed. Runs after
/// each ingest run; the first run after the column was added backfills it.
pub async fn sync_boroughs(pool: &PgPool) -> sqlx::Result<u64> {
    type Row = (Uuid, Option<f64>, Option<f64>, Option<String>);
    let events: Vec<Row> = sqlx::query_as("SELECT id, lat, lng, borough FROM events.events")
        .fetch_all(pool)
        .await?;
    let (mut ids, mut boroughs) = (Vec::new(), Vec::<Option<&str>>::new());
    for (id, lat, lng, current) in &events {
        let b = crate::borough::of(*lat, *lng);
        if b != current.as_deref() {
            ids.push(*id);
            boroughs.push(b);
        }
    }
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(sqlx::query(
        "UPDATE events.events ev SET borough = u.b
         FROM unnest($1::uuid[], $2::text[]) AS u(id, b) WHERE ev.id = u.id",
    )
    .bind(&ids)
    .bind(&boroughs)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Opening hours (issue #168, `crate::hours`) for the event this listing
/// landed in: the payload's structured schema.org hours for the event
/// (#206), else read from the listing's FULL description (before the
/// excerpt and content policy), else inherited from its venue by an
/// exhibition that has none. Hours only apply to all-day events running
/// more than one day (the row's final dates, after merging), and are
/// cleared otherwise, so "doors 7pm" on a talk never becomes a schedule.
/// The listing's own wording (`hours_note`) is kept only when the source's
/// policy lets us keep its description.
///
/// Structured hours on the payload's `location` are the venue's: they fill
/// the matching `events.venues` row's hours when it has none
/// ([`set_venue_hours_tx`]) and serve as the venue fallback here.
async fn set_hours_tx(
    tx: &mut Transaction<'_, Postgres>,
    event_id: Uuid,
    event: &NewEvent,
    raw: &RawEvent,
    policy: SourcePolicy,
) -> sqlx::Result<()> {
    let structured = crate::hours::from_payload(&raw.payload);
    if let Some(h) = &structured.venue {
        set_venue_hours_tx(tx, event.venue_name.as_deref(), h, raw).await?;
    }
    let parsed = match structured.event {
        Some(hours) => Some(crate::hours::ParsedHours { hours, note: None }),
        None => event
            .description
            .as_deref()
            .and_then(crate::hours::from_text),
    };
    let venue = match (&parsed, &structured.venue) {
        (Some(_), _) => None,
        (None, Some(h)) => Some(serde_json::to_value(h).unwrap_or_default()),
        (None, None) => venue_hours_tx(tx, event.venue_name.as_deref()).await?,
    };
    let hours = parsed
        .as_ref()
        .map(|p| serde_json::to_value(&p.hours).unwrap_or_default());
    let note = parsed
        .and_then(|p| p.note)
        .filter(|_| policy.store_description);
    sqlx::query(
        "UPDATE events.events e SET
            opening_hours = CASE WHEN NOT e.all_day OR e.ends_at IS NULL OR e.ends_at <= e.starts_at
                                      OR e.sessions IS NOT NULL
                                 THEN NULL
                                 ELSE COALESCE($2::jsonb, e.opening_hours,
                                               CASE WHEN e.category = 'exhibition' THEN $4::jsonb END)
                            END,
            hours_note    = CASE WHEN NOT e.all_day OR e.ends_at IS NULL OR e.ends_at <= e.starts_at
                                      OR e.sessions IS NOT NULL
                                 THEN NULL
                                 WHEN $2::jsonb IS NOT NULL THEN $3
                                 ELSE e.hours_note
                            END
         WHERE e.id = $1
           AND (e.opening_hours IS NOT NULL OR $2::jsonb IS NOT NULL OR $4::jsonb IS NOT NULL)",
    )
    .bind(event_id)
    .bind(hours)
    .bind(note)
    .bind(venue)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Give the venue a listing names the hours its structured data states
/// (#206), when the venue has none yet (hand-seeded `events.venue_hours`
/// and earlier values win). A venue not created yet gets them on a later
/// run, after `sync_venues` has made its row.
async fn set_venue_hours_tx(
    tx: &mut Transaction<'_, Postgres>,
    venue: Option<&str>,
    hours: &crate::hours::OpeningHours,
    raw: &RawEvent,
) -> sqlx::Result<()> {
    let Some(key) = venue_key_tx(tx, venue).await? else {
        return Ok(());
    };
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, name FROM events.venues WHERE opening_hours IS NULL ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await?;
    let resolver = venue_resolver(&mut **tx).await?;
    let Some((id, _)) = rows.into_iter().find(|(_, name)| {
        resolver.resolve(Some(name)) == crate::venues::Resolved::Venue(key.clone())
    }) else {
        return Ok(());
    };
    let host = raw
        .source_url
        .as_deref()
        .and_then(|u| url::Url::parse(u).ok())
        .and_then(|u| u.host_str().map(str::to_string));
    let source = format!(
        "schema.org structured data{}, {}",
        host.map(|h| format!(" on {h}")).unwrap_or_default(),
        Utc::now().format("%Y-%m-%d")
    );
    sqlx::query(
        "UPDATE events.venues SET opening_hours = $2, hours_source = $3, updated_at = now()
         WHERE id = $1 AND opening_hours IS NULL",
    )
    .bind(id)
    .bind(serde_json::to_value(hours).unwrap_or_default())
    .bind(source)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A venue's usual hours: the venue's own (`events.venues.opening_hours`),
/// else a not-yet-synced `events.venue_hours` row (matched like
/// [`venue_coords_tx`]).
async fn venue_hours_tx(
    tx: &mut Transaction<'_, Postgres>,
    venue: Option<&str>,
) -> sqlx::Result<Option<serde_json::Value>> {
    let Some(key) = venue_key_tx(tx, venue).await? else {
        return Ok(None);
    };
    let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT name, opening_hours FROM (
             SELECT 0 AS pri, id, name, opening_hours FROM events.venues
             WHERE opening_hours IS NOT NULL
             UNION ALL
             SELECT 1, id, name, opening_hours FROM events.venue_hours) h
         ORDER BY pri, id",
    )
    .fetch_all(&mut **tx)
    .await?;
    let resolver = venue_resolver(&mut **tx).await?;
    Ok(rows
        .into_iter()
        .find(|(name, _)| {
            resolver.resolve(Some(name)) == crate::venues::Resolved::Venue(key.clone())
        })
        .map(|(_, h)| h))
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
    set_sessions(tx, id, event).await?;
    set_image_source(tx, id, source_id, event, prior.as_deref()).await
}

/// Store `event`'s sessions (#207) on event `id` (NULL when it has none):
/// they belong with the dates the same statement just wrote.
async fn set_sessions(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    event: &NewEvent,
) -> sqlx::Result<()> {
    let sessions = (event.sessions.len() >= 2).then_some(sqlx::types::Json(&event.sessions));
    sqlx::query("UPDATE events.events SET sessions = $2 WHERE id = $1")
        .bind(id)
        .bind(sessions)
        .execute(&mut **tx)
        .await?;
    Ok(())
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
    // Sessions follow whichever side won the dates.
    if site_wins {
        set_sessions(tx, id, event).await?;
    }
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
    pub all_day: bool,
    /// The sessions of a multi-session event (#207), NULL otherwise.
    pub sessions: Option<sqlx::types::Json<Vec<crate::model::Session>>>,
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
    /// Music subtags (`crate::music`), only for `music` events.
    pub music_tags: Vec<String>,
    pub is_opening: Option<bool>,
    pub whats_cool: Option<String>,
    pub one_liner: Option<String>,
    pub ai_grounding: Option<String>,
    pub ai_model: Option<String>,
    pub ai_enriched_at: Option<DateTime<Utc>>,
    /// Weekly opening hours (`crate::hours` JSON), for all-day runs.
    pub opening_hours: Option<sqlx::types::Json<crate::hours::OpeningHours>>,
    /// The listing's own wording about its hours.
    pub hours_note: Option<String>,
    /// Our page for the event's venue (`/venues/<slug>`, #204), if linked.
    pub venue_slug: Option<String>,
}

const EVENT_COLS: &str = "id, title, description, venue_name, address, lat, lng, starts_at,
    ends_at, all_day, sessions, is_free, price_min, price_max, currency, url, image_url, category, tags,
    dedupe_key, medium_tags, format_tags, good_for, vibe_tags, music_tags, is_opening, whats_cool,
    one_liner, ai_grounding, ai_model, ai_enriched_at, opening_hours, hours_note,
    (SELECT vn.slug FROM events.venues vn WHERE vn.id = venue_id) AS venue_slug";

pub async fn get_event(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<EventRow>> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {EVENT_COLS} FROM events.events WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// A venue (#204) as its page shows it.
#[derive(Debug, Clone, FromRow)]
pub struct Venue {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub address: Option<String>,
    pub postcode: Option<String>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub borough: Option<String>,
    /// The manual override, else the most common type of its events.
    pub venue_type: Option<String>,
    pub website: Option<String>,
    pub opening_hours: Option<sqlx::types::Json<crate::hours::OpeningHours>>,
}

pub async fn venue_by_slug(pool: &PgPool, slug: &str) -> sqlx::Result<Option<Venue>> {
    sqlx::query_as(
        "SELECT v.id, v.name, v.slug, v.address, v.postcode, v.lat, v.lng, v.borough,
                COALESCE(v.venue_type,
                         (SELECT ev.venue_type FROM events.events ev WHERE ev.venue_id = v.id
                          GROUP BY ev.venue_type ORDER BY count(*) DESC, ev.venue_type LIMIT 1))
                    AS venue_type,
                v.website, v.opening_hours
         FROM events.venues v WHERE v.slug = $1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
}

/// A venue's events still to come or running at `now`, soonest first.
pub async fn venue_events(
    pool: &PgPool,
    venue_id: i64,
    now: DateTime<Utc>,
    limit: i64,
) -> sqlx::Result<Vec<EventRow>> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {EVENT_COLS} FROM events.events
         WHERE venue_id = $1 AND COALESCE(ends_at, starts_at) >= $2
         ORDER BY starts_at, id LIMIT $3"
    )))
    .bind(venue_id)
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// An event on a `GET /v1/events` page.
#[derive(Debug, Clone, FromRow)]
pub struct ListedEvent {
    #[sqlx(flatten)]
    pub event: EventRow,
    /// Set when the listing has an area (`near`).
    pub distance_km: Option<f64>,
    /// The sort key as a time (`ending`: effective end, `added`: first
    /// seen), for the next page's cursor; NULL for other sorts.
    pub sort_at: Option<DateTime<Utc>>,
    /// The search rank (`ts_rank`) when sorting by relevance, for the next
    /// page's cursor; NULL for other sorts.
    pub relevance: Option<f64>,
    /// `sort=richest`: the event's day and slot
    /// ([`crate::listing::richest_slot`]); `sort=fullest`: its day and
    /// [`FULLEST_MAX_SCORE`] minus its [`fullness_sql`] score. For the next
    /// page's cursor; NULL for other sorts.
    pub rich_day: Option<NaiveDate>,
    pub rich_slot: Option<i64>,
}

/// Richness score weights (`sort=richest`, #205): a thumbnail we may show,
/// a description (long, or at least short), an AI note, opening hours or a
/// known price.
pub const RICH_WEIGHT_THUMBNAIL: i32 = 3;
pub const RICH_WEIGHT_LONG_DESCRIPTION: i32 = 2;
pub const RICH_WEIGHT_SHORT_DESCRIPTION: i32 = 1;
pub const RICH_WEIGHT_AI_NOTE: i32 = 1;
pub const RICH_WEIGHT_HOURS_OR_PRICE: i32 = 1;
/// Description lengths (characters of the stored, at most 300-character
/// excerpt) for the long and short description weights.
pub const RICH_LONG_DESCRIPTION: i32 = 120;
pub const RICH_SHORT_DESCRIPTION: i32 = 40;
/// The score at which a listing counts as rich: a thumbnail alone is 3 and
/// everything else together 4, so a rich listing always has a picture.
pub const RICH_MIN_SCORE: i32 = 5;

/// Richness score weights of `sort=fullest`, the API's default (#201): an
/// image outweighs everything else together.
pub const FULL_WEIGHT_IMAGE: i32 = 8;
pub const FULL_WEIGHT_DESCRIPTION: i32 = 4;
pub const FULL_WEIGHT_PRICE: i32 = 2;
pub const FULL_WEIGHT_PLACE: i32 = 1;
pub const FULLEST_MAX_SCORE: i32 =
    FULL_WEIGHT_IMAGE + FULL_WEIGHT_DESCRIPTION + FULL_WEIGHT_PRICE + FULL_WEIGHT_PLACE;
const _: () =
    assert!(FULL_WEIGHT_IMAGE > FULL_WEIGHT_DESCRIPTION + FULL_WEIGHT_PRICE + FULL_WEIGHT_PLACE);

/// Whether an event (`ev`) has a thumbnail we may show: `thumbnail_meta`'s
/// test, stored bytes from a source that lets us show images.
const SHOWABLE_THUMBNAIL_SQL: &str = "EXISTS (SELECT 1 FROM events.thumbnails t
                            JOIN events.sources s ON s.id = t.source_id
                            WHERE t.event_id = ev.id AND t.bytes IS NOT NULL AND s.store_image)";

/// An event's (`ev`) richness score for `sort=richest`, from the `RICH_*`
/// weights.
pub fn richness_sql() -> String {
    format!(
        "(CASE WHEN {SHOWABLE_THUMBNAIL_SQL}
               THEN {RICH_WEIGHT_THUMBNAIL} ELSE 0 END
          + CASE WHEN length(COALESCE(ev.description, '')) >= {RICH_LONG_DESCRIPTION}
                   THEN {RICH_WEIGHT_LONG_DESCRIPTION}
                 WHEN length(COALESCE(ev.description, '')) >= {RICH_SHORT_DESCRIPTION}
                   THEN {RICH_WEIGHT_SHORT_DESCRIPTION}
                 ELSE 0 END
          + CASE WHEN ev.whats_cool IS NOT NULL OR ev.one_liner IS NOT NULL
                 THEN {RICH_WEIGHT_AI_NOTE} ELSE 0 END
          + CASE WHEN ev.opening_hours IS NOT NULL OR ev.is_free OR ev.price_min IS NOT NULL
                 THEN {RICH_WEIGHT_HOURS_OR_PRICE} ELSE 0 END)"
    )
}

/// An event's (`ev`) richness score for `sort=fullest`, from the `FULL_*`
/// weights: a thumbnail we may show, a description, a price (or free), a
/// known venue or coordinates.
pub fn fullness_sql() -> String {
    format!(
        "(CASE WHEN {SHOWABLE_THUMBNAIL_SQL} THEN {FULL_WEIGHT_IMAGE} ELSE 0 END
          + CASE WHEN btrim(COALESCE(ev.description, '')) <> ''
                 THEN {FULL_WEIGHT_DESCRIPTION} ELSE 0 END
          + CASE WHEN ev.is_free OR ev.price_min IS NOT NULL
                 THEN {FULL_WEIGHT_PRICE} ELSE 0 END
          + CASE WHEN ev.venue_id IS NOT NULL OR (ev.lat IS NOT NULL AND ev.lng IS NOT NULL)
                 THEN {FULL_WEIGHT_PLACE} ELSE 0 END)"
    )
}

/// The search query of `q=` as a tsquery, from `$10` (the text, for
/// `websearch_to_tsquery`) and `$11` (the prefix/corrected alternatives,
/// `to_tsquery` text built by [`crate::search`]).
const SEARCH_TSQUERY: &str = "(websearch_to_tsquery('pg_catalog.english', events.search_fold($10))
    || to_tsquery('pg_catalog.english', coalesce($11::text, '')))";

/// `$1`..`$11` of every listing query (on `events.events ev`). An event with
/// an end (`ends_at` set), whatever its category, matches when its range
/// overlaps the window, a one-off when it starts inside it. `$4` is
/// `free_only`; `$5` (source keys) matches an event listed by ANY of those
/// sources; `$6` restricts to the given event ids; `$7`/`$8`/`$9` (medium,
/// format, good_for) match an event with ANY of the given tags; `$10`/`$11`
/// are the search ([`SEARCH_TSQUERY`]; a query of stop words only falls back
/// to a substring of the title or venue name).
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
    AND (cardinality($9::text[]) = 0 OR ev.good_for && $9)
    AND ($10::text IS NULL
        OR ev.search @@ (websearch_to_tsquery('pg_catalog.english', events.search_fold($10))
                         || to_tsquery('pg_catalog.english', coalesce($11::text, '')))
        OR (numnode(websearch_to_tsquery('pg_catalog.english', events.search_fold($10))) = 0
            AND strpos(events.search_fold(ev.title || ' ' || coalesce(ev.venue_name, '')),
                       events.search_fold($10)) > 0))
    AND (ev.sessions IS NULL OR EXISTS (
        SELECT 1 FROM jsonb_to_recordset(ev.sessions) AS s(starts_at timestamptz, ends_at timestamptz)
        WHERE ($1::timestamptz IS NULL OR COALESCE(s.ends_at, s.starts_at) >= $1)
          AND ($2::timestamptz IS NULL OR s.starts_at < $2)))";

/// Bind `$1`..`$11` ([`LISTING_FILTER`]) for `f`.
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
        .bind(f.search.as_ref().map(|s| s.text.as_str()))
        .bind(f.search.as_ref().and_then(|s| s.alternatives.as_deref()))
}

/// The event is a multi-session event (#207): its `starts_at`/`ends_at`
/// are only the envelope of its sessions.
const HAS_SESSIONS: &str = "(ev.sessions IS NOT NULL)";

/// `EXISTS` a session `s` of the event (`ev.sessions`, #207) satisfying
/// `cond`. Columns: `s.starts_at`, `s.ends_at` (may be NULL), `s.end_at`
/// (its end, else [`crate::listing::LIVE_GRACE_MINUTES`] after its start:
/// `crate::model::Session::effective_end`) and `s.local` (its London
/// wall-clock start). FALSE when the event has no sessions.
fn session_sql(cond: &str) -> String {
    use crate::listing::LIVE_GRACE_MINUTES;
    format!(
        "EXISTS (SELECT 1 FROM (
            SELECT j.starts_at, j.ends_at,
                   COALESCE(j.ends_at, j.starts_at + interval '{LIVE_GRACE_MINUTES} minutes') AS end_at,
                   j.starts_at AT TIME ZONE 'Europe/London' AS local
            FROM jsonb_to_recordset(ev.sessions) AS j(starts_at timestamptz, ends_at timestamptz)
          ) s WHERE {cond})"
    )
}

/// The event's opening hours apply (`crate::hours`): NULL means unknown,
/// and the date-only behaviour is kept.
const HAS_HOURS: &str = "(ev.opening_hours IS NOT NULL)";

/// `EXISTS` a rule of the event's opening hours (`r`, one element of
/// `ev.opening_hours`: `days` ISO weekdays, `opens`/`closes` `HH:MM`
/// London) satisfying `cond`.
fn hours_rule_sql(cond: &str) -> String {
    format!("EXISTS (SELECT 1 FROM jsonb_array_elements(ev.opening_hours) r WHERE {cond})")
}

/// Rule `r` covers ISO weekday `day` (an integer SQL expression).
fn rule_on_day(day: &str) -> String {
    format!("r->'days' @> to_jsonb(({day})::int)")
}

const RULE_OPENS: &str = "(r->>'opens')::time";
const RULE_CLOSES: &str = "(r->>'closes')::time";

/// The `at=` / `open_now` / `open_at` test ([`EventFilter::live_at`]) and
/// `open_on` ([`EventFilter::open_on`]) as SQL over `events.events ev`
/// (`TRUE` when neither is set).
///
/// `at=`: the event is still on at that instant. In `at=` mode `$1` is the
/// instant minus [`crate::listing::LIVE_LOOKBACK_HOURS`], so the instant is
/// recovered from it (and `$2`, the end of the window, is already applied
/// by [`LISTING_FILTER`]). An all-day event ends at London midnight after
/// its last day (computed in London time, so DST days are 23 or 25 hours);
/// a ranged event at its `ends_at`; an event with a start time but no end
/// [`crate::listing::LIVE_GRACE_MINUTES`] after it starts. An event with
/// opening hours must also be open at the instant, or open later that
/// London day before the window ends (`open_at` is a window ending one
/// second after its instant, so: open at it).
///
/// `open_on`: the event's London dates, clipped to the `$1`/`$2` window,
/// include that weekday (at most the first 7 days need checking), and an
/// event with opening hours is open that weekday.
/// The filters of an [`EventFilter`] that are SQL built from constants
/// rather than [`LISTING_FILTER`]'s placeholders: [`live_sql`] and
/// [`venue_type_sql`], [`borough_sql`], [`music_sql`]. Every listing query applies it after
/// [`LISTING_FILTER`], so counts and listings agree.
fn filter_extra_sql(f: &EventFilter) -> String {
    format!(
        "{} AND {} AND {} AND {}",
        live_sql(f),
        venue_type_sql(f),
        borough_sql(f),
        music_sql(f)
    )
}

/// `music=` ([`EventFilter::music`]): events with ANY of the given music
/// subtags. Built only from [`crate::music::MUSIC_TAGS`] (unknown values
/// are dropped; `crate::listing` rejects them).
fn music_sql(f: &EventFilter) -> String {
    let chosen: Vec<String> = crate::music::MUSIC_TAGS
        .iter()
        .filter(|t| f.music.iter().any(|v| v == *t))
        .map(|t| format!("'{t}'"))
        .collect();
    if chosen.is_empty() {
        "TRUE".into()
    } else {
        format!("ev.music_tags && ARRAY[{}]::text[]", chosen.join(", "))
    }
}

/// `borough=` ([`EventFilter::boroughs`]): events in ANY of the given
/// boroughs. Built only from [`crate::borough::BOROUGHS`] keys (unknown
/// values are dropped; `crate::listing` rejects them).
fn borough_sql(f: &EventFilter) -> String {
    let chosen: Vec<String> = crate::borough::BOROUGHS
        .iter()
        .filter(|(k, _)| f.boroughs.iter().any(|v| v == k))
        .map(|(k, _)| format!("'{k}'"))
        .collect();
    if chosen.is_empty() {
        "TRUE".into()
    } else {
        format!("ev.borough IN ({})", chosen.join(", "))
    }
}

/// `venue_type=` ([`EventFilter::venue_types`]): events at ANY of the
/// given venue types. Built only from [`crate::venue_type::VENUE_TYPES`]
/// (unknown values are dropped; `crate::listing` rejects them).
fn venue_type_sql(f: &EventFilter) -> String {
    let chosen: Vec<String> = crate::venue_type::VENUE_TYPES
        .iter()
        .filter(|t| f.venue_types.iter().any(|v| v == *t))
        .map(|t| format!("'{t}'"))
        .collect();
    if chosen.is_empty() {
        "TRUE".into()
    } else {
        format!("ev.venue_type IN ({})", chosen.join(", "))
    }
}

fn live_sql(f: &EventFilter) -> String {
    use crate::listing::LIVE_LOOKBACK_HOURS;
    let open_on = match f.open_on {
        None => "TRUE".to_string(),
        Some(day) => {
            let day = u8::min(day, 7);
            let by_session = session_sql(&format!(
                "extract(isodow FROM s.local) = {day}
                 AND ($1::timestamptz IS NULL OR COALESCE(s.ends_at, s.starts_at) >= $1)
                 AND ($2::timestamptz IS NULL OR s.starts_at < $2)"
            ));
            format!(
                "(CASE WHEN {HAS_SESSIONS} THEN {by_session} ELSE EXISTS (SELECT 1
                    FROM (SELECT GREATEST({LOCAL_START}::date,
                                          ($1::timestamptz AT TIME ZONE 'Europe/London')::date) AS d0,
                                 LEAST({LOCAL_LAST_DAY},
                                       ($2::timestamptz AT TIME ZONE 'Europe/London')::date - 1) AS d1) w,
                         generate_series(0, LEAST(w.d1 - w.d0, 6)) AS k
                    WHERE extract(isodow FROM w.d0 + k) = {day})
                  AND (NOT {HAS_HOURS} OR {}) END)",
                hours_rule_sql(&rule_on_day(&day.to_string()))
            )
        }
    };
    if f.live_at.is_none() {
        return open_on;
    }
    let now = format!("($1::timestamptz + interval '{LIVE_LOOKBACK_HOURS} hours')");
    let lnow = format!("({now} AT TIME ZONE 'Europe/London')");
    let lend = "($2::timestamptz AT TIME ZONE 'Europe/London')";
    let open = hours_rule_sql(&format!(
        "{} AND {RULE_CLOSES} > {lnow}::time
         AND ({lend} IS NULL AND {RULE_OPENS} <= {lnow}::time
              OR {lend}::date > {lnow}::date
              OR {RULE_OPENS} < {lend}::time)",
        rule_on_day(&format!("extract(isodow FROM {lnow})"))
    ));
    format!(
        "({}
          AND (NOT {HAS_HOURS} OR {open})
          AND {open_on})",
        still_on_sql(&now, "COALESCE($2::timestamptz, 'infinity')")
    )
}

/// The event has not ended at SQL instant `now`: an all-day event ends at
/// London midnight after its last day (computed in London time, so DST
/// days are 23 or 25 hours), a ranged event at its `ends_at`, an event
/// with a start time but no end [`crate::listing::LIVE_GRACE_MINUTES`]
/// after it starts.
///
/// A multi-session event (#207) is on when one of its sessions has not
/// ended at `now` and starts before SQL instant `until` (so a gap between
/// sessions is not "on").
fn still_on_sql(now: &str, until: &str) -> String {
    use crate::listing::LIVE_GRACE_MINUTES;
    let session = session_sql(&format!("s.end_at > {now} AND s.starts_at < {until}"));
    format!(
        "(CASE
            WHEN {HAS_SESSIONS} THEN {session}
            WHEN ev.all_day THEN
                (((COALESCE(ev.ends_at, ev.starts_at) AT TIME ZONE 'Europe/London')::date + 1)::timestamp
                    AT TIME ZONE 'Europe/London') > {now}
            WHEN ev.ends_at IS NOT NULL THEN ev.ends_at > {now}
            ELSE ev.starts_at + interval '{LIVE_GRACE_MINUTES} minutes' > {now}
          END)"
    )
}

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
/// `weekend` clips to the window in `$1`/`$2`. An event with opening hours
/// is judged by them instead of its start time and the `late opening` tag:
/// evening = open after 18:00 on some day, after work = open between 17:30
/// and 20:30 on a weekday, daytime = open before 18:00, weekend = open on a
/// Saturday or Sunday within its clipped dates.
fn when_sql(when: Option<When>) -> String {
    let late = format!("({LOCAL_START}::time = '00:00' AND 'late opening' = ANY(ev.tags))");
    let hours = |cond: &str| hours_rule_sql(cond);
    let by = |with_hours: String, without: String| {
        format!("(CASE WHEN {HAS_HOURS} THEN {with_hours} ELSE {without} END)")
    };
    match when {
        None => "TRUE".into(),
        Some(When::Evening) => by(
            hours(&format!("{RULE_CLOSES} > '18:00'")),
            format!("({LOCAL_START}::time >= '18:00' OR {late})"),
        ),
        Some(When::AfterWork) => by(
            hours(&format!(
                "{RULE_CLOSES} > '17:30' AND {RULE_OPENS} <= '20:30'
                 AND EXISTS (SELECT 1 FROM jsonb_array_elements_text(r->'days') d
                             WHERE d::int <= 5)"
            )),
            format!(
                "((extract(isodow FROM {LOCAL_START}) <= 5
                    AND {LOCAL_START}::time BETWEEN '17:30' AND '20:30') OR {late})"
            ),
        ),
        Some(When::Daytime) => by(
            hours(&format!("{RULE_OPENS} < '18:00'")),
            format!("({LOCAL_START}::time < '18:00')"),
        ),
        // At most the first 7 days of the clipped range need checking.
        Some(When::Weekend) => format!(
            "(CASE WHEN {HAS_SESSIONS} THEN {} ELSE EXISTS (SELECT 1
                FROM (SELECT GREATEST({LOCAL_START}::date,
                                      ($1::timestamptz AT TIME ZONE 'Europe/London')::date) AS d0,
                             LEAST((COALESCE(ev.ends_at, ev.starts_at) AT TIME ZONE 'Europe/London')::date,
                                   ($2::timestamptz AT TIME ZONE 'Europe/London')::date - 1) AS d1) w,
                     generate_series(0, LEAST(w.d1 - w.d0, 6)) AS k
                WHERE extract(isodow FROM w.d0 + k) >= 6
                  AND (NOT {HAS_HOURS} OR {})) END)",
            session_sql(
                "extract(isodow FROM s.local) >= 6
                 AND ($1::timestamptz IS NULL OR COALESCE(s.ends_at, s.starts_at) >= $1)
                 AND ($2::timestamptz IS NULL OR s.starts_at < $2)"
            ),
            hours(&rule_on_day("extract(isodow FROM w.d0 + k)"))
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

/// When an event is over, for "Last chance" (`sort=ending`): its end; for
/// an all-day event the London midnight after its last (or only) day; for
/// a timed one-off without an end, three hours after it starts.
pub const EFFECTIVE_END: &str = "(CASE
        WHEN ev.all_day THEN ((COALESCE(ev.ends_at, ev.starts_at) AT TIME ZONE 'Europe/London')
                              + interval '1 day') AT TIME ZONE 'Europe/London'
        WHEN ev.ends_at IS NOT NULL THEN ev.ends_at
        ELSE ev.starts_at + interval '3 hours'
    END)";

/// The event's first and last London dates.
const LOCAL_FIRST_DAY: &str = "((ev.starts_at AT TIME ZONE 'Europe/London')::date)";
const LOCAL_LAST_DAY: &str =
    "((COALESCE(ev.ends_at, ev.starts_at) AT TIME ZONE 'Europe/London')::date)";

/// Title words that mark an opening (word-bounded, case-insensitive; "PV"
/// only in capitals, so "PVC" or "pv" in a word never match).
const OPENING_TITLE: &str =
    "(ev.title ~* '\\m(private view|opening reception|opening night|preview evening|launch)\\M'
        OR ev.title ~ '\\mPV\\M')";

/// Title words that mark a hands-on session (word-bounded, case-insensitive).
const HANDS_ON_TITLE: &str =
    "(ev.title ~* '\\m(class|classes|course|drop-in|life drawing|masterclass|workshops?)\\M')";

/// A quick pick ([`Pick`]) as SQL over `events.events ev` (`TRUE` for none).
/// Its London date is a typed `NaiveDate`, written as a date literal.
pub fn pick_sql(pick: Option<PickFilter>) -> String {
    let Some(PickFilter { pick, today }) = pick else {
        return "TRUE".into();
    };
    let l = format!("DATE '{}'", today.format("%Y-%m-%d"));
    let first = LOCAL_FIRST_DAY;
    let last = LOCAL_LAST_DAY;
    match pick {
        // With opening hours: open after 18:00 today.
        Pick::Tonight => format!(
            "({first} <= {l} AND {last} >= {l}
              AND (CASE WHEN {HAS_SESSIONS} THEN {}
                   WHEN {HAS_HOURS} THEN {}
                   ELSE (({first} = {l} AND {LOCAL_START}::time >= '17:00')
                         OR ({LOCAL_START}::time = '00:00' AND 'late opening' = ANY(ev.tags)))
                   END))",
            session_sql(&format!("s.local::date = {l} AND s.local::time >= '17:00'")),
            hours_rule_sql(&format!(
                "{} AND {RULE_CLOSES} > '18:00'",
                rule_on_day(&format!("extract(isodow FROM {l})"))
            ))
        ),
        Pick::Openings => format!(
            "({first} BETWEEN {l} AND {l} + 6
              AND (ev.category = 'exhibition' OR ev.is_opening IS TRUE
                   OR 'opening' = ANY(ev.format_tags) OR {OPENING_TITLE}))"
        ),
        Pick::LastChance => format!(
            "({last} > {first} AND {last} BETWEEN {l} AND {l} + 6 AND {EFFECTIVE_END} > now())"
        ),
        Pick::HandsOn => format!(
            "(ev.category = 'workshop' OR 'hands_on' = ANY(ev.format_tags) OR {HANDS_ON_TITLE})"
        ),
        // Started, not ended, and open now by its hours; without hours only
        // a timed event counts (an untimed one may be closed right now).
        Pick::OpenNow => format!(
            "(ev.starts_at <= now() AND {}
              AND (CASE WHEN {HAS_HOURS} THEN {}
                   ELSE NOT ev.all_day AND {LOCAL_START}::time <> '00:00'
                   END))",
            still_on_sql("now()", "now() + interval '1 second'"),
            hours_rule_sql(&format!(
                "{} AND {RULE_OPENS} <= (now() AT TIME ZONE 'Europe/London')::time
                 AND {RULE_CLOSES} > (now() AT TIME ZONE 'Europe/London')::time",
                rule_on_day("extract(isodow FROM now() AT TIME ZONE 'Europe/London')")
            ))
        ),
    }
}

/// How many upcoming events (still running at or after `from`, London
/// midnight today) each home-page quick pick would list; `weekend` is the
/// London dates `[sat, mon)` as instants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromRow)]
pub struct QuickPickCounts {
    pub tonight: i64,
    pub weekend: i64,
    pub free: i64,
    pub openings: i64,
    pub last_chance: i64,
    pub hands_on: i64,
    pub open_now: i64,
    pub talks: i64,
    pub music: i64,
}

/// One query (FILTER aggregates) for every quick pick, with the same
/// predicates as the listings the chips open.
pub async fn quick_pick_counts(
    pool: &PgPool,
    today: NaiveDate,
    from: DateTime<Utc>,
    weekend: (DateTime<Utc>, DateTime<Utc>),
) -> sqlx::Result<QuickPickCounts> {
    let p = |pick: Pick| pick_sql(Some(PickFilter { pick, today }));
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT count(*) FILTER (WHERE {}) AS tonight,
                count(*) FILTER (WHERE COALESCE(ev.ends_at, ev.starts_at) >= $2
                                   AND ev.starts_at < $3
                                   AND (NOT {HAS_SESSIONS} OR {})) AS weekend,
                count(*) FILTER (WHERE ev.is_free) AS free,
                count(*) FILTER (WHERE {}) AS openings,
                count(*) FILTER (WHERE {}) AS last_chance,
                count(*) FILTER (WHERE {}) AS hands_on,
                count(*) FILTER (WHERE {}) AS open_now,
                count(*) FILTER (WHERE ev.category = 'talk') AS talks,
                count(*) FILTER (WHERE ev.category = 'music') AS music
         FROM events.events ev
         WHERE COALESCE(ev.ends_at, ev.starts_at) >= $1
           AND (NOT {HAS_SESSIONS} OR {})",
        p(Pick::Tonight),
        session_sql("COALESCE(s.ends_at, s.starts_at) >= $2 AND s.starts_at < $3"),
        p(Pick::Openings),
        p(Pick::LastChance),
        p(Pick::HandsOn),
        p(Pick::OpenNow),
        session_sql("COALESCE(s.ends_at, s.starts_at) >= $1"),
    )))
    .bind(from)
    .bind(weekend.0)
    .bind(weekend.1)
    .fetch_one(pool)
    .await
}

/// When Muse & Mingle first saw the event (`sort=added`).
const FIRST_SEEN: &str = "COALESCE((SELECT min(es.first_seen_at) FROM events.event_sources es
        WHERE es.event_id = ev.id), ev.created_at)";

/// One page of events plus one more row (the caller's "has next page" probe):
/// `query.limit + 1` rows at most.
///
/// Placeholders: `$1`..`$11` [`LISTING_FILTER`], `$12` price_max, `$13`..`$20`
/// the area (NULL without `near`), `$21`/`$22` the cursor, `$23` the limit,
/// `$24` the sort's parameter (`now` for `ending`, the seed for `surprise`,
/// today for `richest`, the window's first day for `fullest`), `$25` the
/// `richest`/`fullest` cursor's slot, `$26` the `fullest` cursor's start.
pub async fn list_events(pool: &PgPool, query: &EventQuery) -> sqlx::Result<Vec<ListedEvent>> {
    let f = &query.filter;
    let near = query.near;
    let b = near.map(|n| n.bounding_box());
    let filter = format!(
        "{LISTING_FILTER} AND {} AND {} AND {} AND {}
         AND ($13::float8 IS NULL OR (lat BETWEEN $16 AND $17 AND lng BETWEEN $18 AND $19))",
        price_max_sql("$12"),
        when_sql(f.when),
        pick_sql(f.pick),
        filter_extra_sql(f)
    );
    // (sort key expression, extra condition, cursor condition, order)
    let (key, extra, after, order) = match &query.order {
        EventOrder::ByStart { .. } => (
            "NULL::timestamptz".to_string(),
            "TRUE",
            "($21::timestamptz IS NULL OR (starts_at, id) > ($21, $22::uuid))",
            "starts_at, id",
        ),
        EventOrder::ByDistance { .. } => (
            "NULL::timestamptz".to_string(),
            "TRUE",
            "($21::float8 IS NULL OR (distance_km, id) > ($21, $22::uuid))",
            "distance_km, id",
        ),
        EventOrder::ByEnd { .. } => (
            EFFECTIVE_END.to_string(),
            "sort_at > $24::timestamptz",
            "($21::timestamptz IS NULL OR (sort_at, id) > ($21, $22::uuid))",
            "sort_at, id",
        ),
        EventOrder::ByAdded { .. } => (
            FIRST_SEEN.to_string(),
            "TRUE",
            "($21::timestamptz IS NULL OR (sort_at, id) < ($21, $22::uuid))",
            "sort_at DESC, id DESC",
        ),
        EventOrder::Shuffled { .. } => (
            "NULL::timestamptz".to_string(),
            "TRUE",
            // $21 is unused (NULL); the key is recomputed from the last id.
            "($22::uuid IS NULL OR (shuffle, id) > (md5($22::text || $24::text), $22))",
            "shuffle, id",
        ),
        EventOrder::ByRelevance { .. } => (
            "NULL::timestamptz".to_string(),
            "TRUE",
            "($21::float8 IS NULL OR relevance < $21 OR (relevance = $21 AND id > $22::uuid))",
            "relevance DESC, id",
        ),
        EventOrder::Richest { .. } => (
            "NULL::timestamptz".to_string(),
            "TRUE",
            "($21::date IS NULL OR (rich_day, rich_slot, id) > ($21, $25::int8, $22::uuid))",
            "rich_day, rich_slot, id",
        ),
        EventOrder::Fullest { .. } => (
            "NULL::timestamptz".to_string(),
            "TRUE",
            "($21::date IS NULL
              OR (rich_day, rich_slot, starts_at, id) > ($21, $25::int8, $26::timestamptz, $22::uuid))",
            "rich_day, rich_slot, starts_at, id",
        ),
    };
    // Other sorts bind `$25`/`$26` as NULL; they must still appear in the SQL.
    let extra = match &query.order {
        EventOrder::Fullest { .. } => extra.to_string(),
        EventOrder::Richest { .. } => format!("{extra} AND $26::timestamptz IS NULL"),
        _ => format!("{extra} AND $25::int8 IS NULL AND $26::timestamptz IS NULL"),
    };
    // `richest`/`fullest`: each event's London day (the day `$24` if
    // already running; a multi-session event's next session day).
    let rich_day = format!(
        "COALESCE((SELECT min((j.starts_at AT TIME ZONE 'Europe/London')::date)
                   FROM jsonb_to_recordset(ev.sessions) AS j(starts_at timestamptz)
                   WHERE (j.starts_at AT TIME ZONE 'Europe/London')::date >= $24::date),
                  GREATEST({LOCAL_FIRST_DAY}, $24::date)) AS rich_day"
    );
    // `richest` also needs whether it is rich; the slot is numbered over the
    // whole filtered set, before the cursor, so pages don't shift.
    let (rich_cols, slot) = match &query.order {
        EventOrder::Richest { .. } => {
            let (n, m) = (crate::listing::RICH_RUN, crate::listing::RICH_RUN + 1);
            (
                format!("{rich_day}, {} >= {RICH_MIN_SCORE} AS rich", richness_sql()),
                format!(
                    "CASE WHEN rich THEN rn + (rn - 1) / {n} ELSE rn * {m} END AS rich_slot
                     FROM (SELECT *, row_number() OVER (PARTITION BY rich_day, rich
                                                        ORDER BY starts_at, id) AS rn"
                ),
            )
        }
        EventOrder::Fullest { .. } => (
            format!(
                "{rich_day}, NULL::bool AS rich,
                 ({FULLEST_MAX_SCORE} - {})::int8 AS fullest_slot",
                fullness_sql()
            ),
            "fullest_slot AS rich_slot FROM (SELECT *".to_string(),
        ),
        _ => (
            "NULL::date AS rich_day, NULL::bool AS rich".to_string(),
            "NULL::int8 AS rich_slot FROM (SELECT *".to_string(),
        ),
    };
    let shuffle = match &query.order {
        EventOrder::Shuffled { .. } => "md5(ev.id::text || $24::text)",
        _ => "NULL::text",
    };
    let relevance = match &query.order {
        EventOrder::ByRelevance { .. } => format!("ts_rank(ev.search, {SEARCH_TSQUERY})::float8"),
        _ => "NULL::float8".to_string(),
    };
    let sql = format!(
        "SELECT * FROM (
             SELECT *, {slot} FROM (
                 SELECT {EVENT_COLS},
                        CASE WHEN $13::float8 IS NULL THEN NULL::float8 ELSE {} END AS distance_km,
                        {key} AS sort_at, {shuffle} AS shuffle, {relevance} AS relevance,
                        {rich_cols}
                 FROM events.events ev
                 WHERE {filter}
             ) e0
             WHERE ($20::float8 IS NULL OR distance_km <= $20)) e1
         ) e
         WHERE {extra} AND {after}
         ORDER BY {order} LIMIT $23",
        distance_km_sql(13, 14, 15)
    );
    let q = bind_filter(sqlx::query_as(AssertSqlSafe(sql)), f)
        .bind(f.price_max)
        .bind(near.map(|n| n.lat))
        .bind(near.map(|n| n.lng))
        .bind(EARTH_RADIUS_KM)
        .bind(b.map(|b| b.min_lat))
        .bind(b.map(|b| b.max_lat))
        .bind(b.map(|b| b.min_lng))
        .bind(b.map(|b| b.max_lng))
        .bind(near.map(|n| n.radius_km));
    let q = match &query.order {
        EventOrder::ByStart { after } | EventOrder::ByAdded { after } => q
            .bind(after.map(|a| a.0))
            .bind(after.map(|a| a.1))
            .bind(query.limit + 1)
            .bind(None::<String>),
        EventOrder::ByDistance { after, .. } => q
            .bind(after.map(|a| a.0))
            .bind(after.map(|a| a.1))
            .bind(query.limit + 1)
            .bind(None::<String>),
        EventOrder::ByEnd { now, after } => q
            .bind(after.map(|a| a.0))
            .bind(after.map(|a| a.1))
            .bind(query.limit + 1)
            .bind(*now),
        EventOrder::Shuffled { seed, after } => q
            .bind(None::<String>)
            .bind(*after)
            .bind(query.limit + 1)
            .bind(seed.format("%Y-%m-%d").to_string()),
        EventOrder::ByRelevance { after } => q
            .bind(after.map(|a| a.0))
            .bind(after.map(|a| a.1))
            .bind(query.limit + 1)
            .bind(None::<String>),
        EventOrder::Richest { today, after } => {
            return q
                .bind(after.map(|a| a.0))
                .bind(after.map(|a| a.2))
                .bind(query.limit + 1)
                .bind(today.format("%Y-%m-%d").to_string())
                .bind(after.map(|a| a.1))
                .bind(None::<DateTime<Utc>>)
                .fetch_all(pool)
                .await;
        }
        EventOrder::Fullest { from_day, after } => {
            return q
                .bind(after.map(|a| a.0))
                .bind(after.map(|a| a.3))
                .bind(query.limit + 1)
                .bind(from_day.format("%Y-%m-%d").to_string())
                .bind(after.map(|a| a.1))
                .bind(after.map(|a| a.2))
                .fetch_all(pool)
                .await;
        }
    };
    q.bind(None::<i64>)
        .bind(None::<DateTime<Utc>>)
        .fetch_all(pool)
        .await
}

/// Which tag column a facet counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facet {
    Medium,
    Format,
    GoodFor,
    VenueType,
    /// Events without a known borough count as [`crate::borough::UNKNOWN`].
    Borough,
    /// Music subtags ([`crate::music::MUSIC_TAGS`]).
    Music,
}

impl Facet {
    pub const ALL: [Facet; 6] = [
        Facet::Medium,
        Facet::Format,
        Facet::GoodFor,
        Facet::VenueType,
        Facet::Borough,
        Facet::Music,
    ];

    /// The facet's values of an event (`ev`) as an SQL array.
    pub fn column(self) -> &'static str {
        match self {
            Facet::Medium => "ev.medium_tags",
            Facet::Format => "ev.format_tags",
            Facet::GoodFor => "ev.good_for",
            Facet::VenueType => "ARRAY[ev.venue_type]",
            Facet::Borough => "ARRAY[COALESCE(ev.borough, 'unknown')]",
            Facet::Music => "ev.music_tags",
        }
    }

    /// The API name (`medium`, `format`, `good_for`, `venue_type`,
    /// `borough`, `music`).
    pub fn name(self) -> &'static str {
        match self {
            Facet::Medium => "medium",
            Facet::Format => "format",
            Facet::GoodFor => "good_for",
            Facet::VenueType => "venue_type",
            Facet::Borough => "borough",
            Facet::Music => "music",
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
        Facet::VenueType => f.venue_types.clear(),
        Facet::Borough => f.boroughs.clear(),
        Facet::Music => f.music.clear(),
    }
    let near = query.near;
    let b = near.map(|n| n.bounding_box());
    let col = facet.column();
    let filter = format!(
        "{LISTING_FILTER} AND {} AND {} AND {} AND {}",
        price_max_sql("$20"),
        when_sql(f.when),
        pick_sql(f.pick),
        filter_extra_sql(&f)
    );
    bind_filter(
        sqlx::query_as(AssertSqlSafe(format!(
            "SELECT t, count(*) FROM events.events ev CROSS JOIN LATERAL unnest({col}) AS t
             WHERE {filter}
               AND ($12::float8 IS NULL OR (
                   lat BETWEEN $15 AND $16 AND lng BETWEEN $17 AND $18
                   AND {} <= $19))
             GROUP BY t ORDER BY count(*) DESC, t",
            distance_km_sql(12, 13, 14)
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
/// only applied per-count, below, via `$12`/`$13`) but the other filters
/// (dates, category, sources, ids, tags) still apply to the base rows.
pub async fn listing_counts(
    pool: &PgPool,
    filter: &EventFilter,
    near: Option<&Near>,
) -> sqlx::Result<ListingCounts> {
    let categories: Vec<&str> = filter.categories.iter().map(|c| c.as_str()).collect();
    let sources: Vec<&str> = filter.sources.iter().map(String::as_str).collect();
    let price = price_filter_sql("$12", "$13");
    let when = when_sql(filter.when);
    let live = filter_extra_sql(filter);
    let when_count = |w: When| format!("count(*) FILTER (WHERE {} AND {price})", when_sql(Some(w)));
    let price_count = |p: &str| format!("count(*) FILTER (WHERE {p} AND {when})");
    let b = near.map(Near::bounding_box);
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {} AS evening, {} AS after_work, {} AS weekend, {} AS daytime,
                {} AS free, {} AS max_10, {} AS max_20, {} AS unknown
         FROM events.events ev
         WHERE {LISTING_FILTER} AND {live} AND {}
           AND ($14::float8 IS NULL OR (lat BETWEEN $16 AND $17 AND lng BETWEEN $18 AND $19
                AND {} <= $21))",
        when_count(When::Evening),
        when_count(When::AfterWork),
        when_count(When::Weekend),
        when_count(When::Daytime),
        price_count(&price_filter_sql("TRUE", "NULL")),
        price_count(&price_filter_sql("FALSE", "10")),
        price_count(&price_filter_sql("FALSE", "20")),
        price_count(PRICE_UNKNOWN),
        pick_sql(filter.pick),
        distance_km_sql(14, 15, 20)
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
    .bind(filter.search.as_ref().map(|s| s.text.as_str()))
    .bind(
        filter
            .search
            .as_ref()
            .and_then(|s| s.alternatives.as_deref()),
    )
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

/// Events a search word is checked against for typos: still on (or ended
/// less than a day ago).
const SEARCH_UPCOMING: &str = "COALESCE(ev.ends_at, ev.starts_at) >= now() - interval '1 day'";

/// Which of `words` (folded `[a-z0-9]+`, see [`crate::search`]) match no
/// upcoming event, even as a prefix. Stop words are never reported.
pub async fn search_unmatched_words(pool: &PgPool, words: &[String]) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT w FROM unnest($1::text[]) AS w
         WHERE numnode(to_tsquery('pg_catalog.english', w)) > 0
           AND NOT EXISTS (SELECT 1 FROM events.events ev
                           WHERE {SEARCH_UPCOMING}
                             AND ev.search @@ to_tsquery('pg_catalog.english', w || ':*'))"
    )))
    .bind(words)
    .fetch_all(pool)
    .await
}

/// The distinct folded words (3+ letters) of upcoming events' titles and
/// venue names: what a mistyped search word is corrected to.
pub async fn search_lexicon(pool: &PgPool) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT DISTINCT w FROM events.events ev,
             regexp_split_to_table(
                 events.search_fold(ev.title || ' ' || coalesce(ev.venue_name, '')),
                 '[^a-z0-9]+') AS w
         WHERE {SEARCH_UPCOMING} AND length(w) >= 3 AND w !~ '^[0-9]+$'
         ORDER BY w"
    )))
    .fetch_all(pool)
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
             CROSS JOIN (SELECT $12::real[]::extensions.vector AS v) q
             WHERE {LISTING_FILTER} AND {}
             ORDER BY em.embedding OPERATOR(extensions.<=>) q.v, ev.id
             LIMIT $13",
            filter_extra_sql(filter)
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
    /// The latest scraper QA check that ran to a conclusion.
    pub qa_checked_at: Option<DateTime<Utc>>,
    pub qa_status: Option<String>,
    /// Wrong fields + missed events it found.
    pub qa_problems: Option<i32>,
    /// QA rules hit by the latest run.
    pub qa_rule_flags: i64,
}

pub async fn source_statuses(pool: &PgPool) -> sqlx::Result<Vec<SourceStatusRow>> {
    sqlx::query_as(
        "SELECT s.key, s.display_name, s.kind, s.interval_minutes, s.enabled,
                r.started_at AS run_started_at, r.events_found AS run_events_found,
                r.errors AS run_errors, r.duration_ms AS run_duration_ms, r.ok AS run_ok,
                h.github_issue_number AS open_issue_number, s.skip_reason, s.skipped_at,
                q.checked_at AS qa_checked_at, q.status AS qa_status,
                q.wrong_fields + q.missed_events AS qa_problems,
                (SELECT count(*) FROM events.qa_findings f WHERE f.run_id = r.id) AS qa_rule_flags
         FROM events.sources s
         LEFT JOIN LATERAL (
             SELECT id, started_at, events_found, errors, duration_ms, ok
             FROM events.source_runs WHERE source_id = s.id
             ORDER BY started_at DESC, id DESC LIMIT 1
         ) r ON TRUE
         LEFT JOIN LATERAL (
             SELECT checked_at, status, wrong_fields, missed_events
             FROM events.qa_checks
             WHERE source_id = s.id AND status = ANY($1)
             ORDER BY checked_at DESC, id DESC LIMIT 1
         ) q ON TRUE
         LEFT JOIN events.health_issues h ON h.source_id = s.id AND h.closed_at IS NULL
         ORDER BY s.key",
    )
    .bind(crate::qa::store::COMPLETED)
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

/// Take one of `per_day` form-issue slots for the London date `day`; false
/// when they are all taken. Atomic, so concurrent requests can't overshoot.
pub async fn reserve_form_issue(pool: &PgPool, day: NaiveDate, per_day: i32) -> sqlx::Result<bool> {
    let filed: Option<i32> = sqlx::query_scalar(
        "INSERT INTO events.form_issue_quota (day, filed) VALUES ($1, 1)
         ON CONFLICT (day) DO UPDATE SET filed = events.form_issue_quota.filed + 1
             WHERE events.form_issue_quota.filed < $2
         RETURNING filed",
    )
    .bind(day)
    .bind(per_day)
    .fetch_optional(pool)
    .await?;
    Ok(filed.is_some())
}

/// Site suggestions and contact requests waiting for a GitHub issue.
pub async fn held_form_counts(pool: &PgPool) -> sqlx::Result<(i64, i64)> {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM events.site_suggestions WHERE status = 'pending'),
                (SELECT count(*) FROM events.contact_requests WHERE status = 'pending_issue')",
    )
    .fetch_one(pool)
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
/// storing images and no thumbnail for that exact image URL yet. A failed
/// image URL is retried `retry_base` after its first failure, then after
/// twice as long each time, and not at all after `max_failures`. Soonest
/// first.
pub async fn thumbnail_jobs(
    pool: &PgPool,
    now: DateTime<Utc>,
    retry_base: chrono::Duration,
    max_failures: i32,
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
                OR (t.bytes IS NULL AND t.failures < $3
                    AND t.fetched_at + make_interval(secs => $2::float8 * power(2, t.failures - 1))
                        <= $1))
         ORDER BY e.starts_at, e.id
         LIMIT $4",
    )
    .bind(now)
    .bind(retry_base.num_seconds() as f64)
    .bind(max_failures)
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

/// Store the thumbnail (`Ok`) or the failure (`Err(message)`) for a job,
/// counting consecutive failures of the same image URL.
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
             width, height, content_hash, etag, last_modified, error, failures, fetched_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now())
         ON CONFLICT (event_id) DO UPDATE SET
             source_id = EXCLUDED.source_id, source_image_url = EXCLUDED.source_image_url,
             bytes = EXCLUDED.bytes, content_type = EXCLUDED.content_type,
             width = EXCLUDED.width, height = EXCLUDED.height,
             content_hash = EXCLUDED.content_hash, etag = EXCLUDED.etag,
             last_modified = EXCLUDED.last_modified, error = EXCLUDED.error,
             failures = CASE
                 WHEN EXCLUDED.error IS NULL THEN 0
                 WHEN events.thumbnails.bytes IS NULL
                      AND events.thumbnails.source_image_url = EXCLUDED.source_image_url
                     THEN events.thumbnails.failures + 1
                 ELSE 1 END,
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
    .bind(i32::from(error.is_some()))
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
