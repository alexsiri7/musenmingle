//! HTTP API: `GET /healthz`, the read API (`GET /v1/events`,
//! `GET /v1/events/{id}`, `GET /v1/sources`; see `docs/api.md`) and
//! `POST /v1/suggestions`.
//!
//! The human-facing HTML pages (`/`, `/events/{id}`, `/sources`,
//! `POST /suggest`) live in `crate::web` and share the helpers here.
//!
//! Handlers that need the client address extract `ConnectInfo`, so serve
//! the router with `into_make_service_with_connect_info::<SocketAddr>()`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower_http::cors::{AllowOrigin, CorsLayer};
use uuid::Uuid;

use crate::health::{HealthStatus, RunStats};
use crate::listing::{self, Cursor, EventOrder};
use crate::model::SourceKind;
use crate::repo::{
    self, EventRow, ListingCounts, RefusedSourceRow, SourceStatusRow, ThumbnailMeta,
};
use crate::suggestions::{Outcome, Suggestions};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub suggestions: Arc<Suggestions>,
    pub github_repo: Arc<str>,
    /// Home-page quick-pick counts, cached for 5 minutes.
    pub(crate) quick_picks: Arc<crate::web::QuickPickCache>,
    /// The London map tiles (`/tiles/london.pmtiles`), when present.
    pub tiles: Option<Arc<crate::web::map::TileFile>>,
    /// Public-transport times (`/v1/transit`); `None` = walking only.
    pub transit: Option<Arc<crate::transit::Transit>>,
}

/// Settings for [`router`] beyond the database and suggestions.
#[derive(Debug, Clone)]
pub struct ApiSettings {
    /// `owner/name`, for links to open `scraper-broken` issues.
    pub github_repo: String,
    /// Browser origins allowed to call the API (see `config::parse_cors_origins`).
    pub cors_origins: Vec<HeaderValue>,
}

pub fn router(pool: PgPool, suggestions: Suggestions, settings: ApiSettings) -> Router {
    router_with_tiles(pool, suggestions, settings, None)
}

/// [`router`] serving the map tiles from the PMTiles file at `tiles` (see
/// `crate::web::map`); without it (or if it is missing) `/map` shows only
/// its list.
pub fn router_with_tiles(
    pool: PgPool,
    suggestions: Suggestions,
    settings: ApiSettings,
    tiles: Option<&std::path::Path>,
) -> Router {
    router_with(pool, suggestions, settings, tiles, None)
}

/// [`router_with_tiles`] plus the public-transport service behind
/// `GET /v1/transit` (without it, the endpoint answers "unavailable").
pub fn router_with(
    pool: PgPool,
    suggestions: Suggestions,
    settings: ApiSettings,
    tiles: Option<&std::path::Path>,
    transit: Option<crate::transit::Transit>,
) -> Router {
    let tiles = tiles.and_then(crate::web::map::TileFile::open);
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(settings.cors_origins))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/events", get(list_events))
        .route("/v1/events/{id}", get(get_event))
        .route("/v1/events/{id}/similar", get(get_similar))
        .route("/v1/sources", get(list_sources))
        .route("/v1/transit", get(get_transit))
        .route("/v1/suggestions", post(suggest))
        .merge(crate::web::routes())
        .merge(
            tiles
                .clone()
                .map(crate::web::map::tile_routes)
                .unwrap_or_default(),
        )
        .layer(cors)
        .layer(axum::middleware::map_response(security_headers))
        .with_state(AppState {
            pool,
            suggestions: Arc::new(suggestions),
            github_repo: settings.github_repo.into(),
            quick_picks: Arc::default(),
            tiles,
            transit: transit.map(Arc::new),
        })
}

/// Response headers every response carries (JSON, static assets, errors and
/// HTML alike). A handler that sets one of them itself (the HTML pages set
/// the page CSP) keeps its own value.
pub const SECURITY_HEADERS: [(&str, &str); 5] = [
    ("strict-transport-security", "max-age=31536000"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "strict-origin-when-cross-origin"),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
    ),
    // Non-HTML responses load nothing and are never framed.
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'; base-uri 'none'",
    ),
];

async fn security_headers(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        if !h.contains_key(name) {
            h.insert(name, HeaderValue::from_static(value));
        }
    }
    resp
}

/// 200 when the database answers `SELECT 1` within 3 s, 503 otherwise.
async fn healthz(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let db = tokio::time::timeout(
        Duration::from_secs(3),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.pool),
    )
    .await;
    match db {
        Ok(Ok(1)) => (
            StatusCode::OK,
            Json(json!({ "status": "ok", "db": "ok", "version": crate::VERSION })),
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "unavailable", "db": "error", "version": crate::VERSION })),
        ),
    }
}

#[derive(Deserialize)]
struct SuggestionRequest {
    url: String,
    note: Option<String>,
}

/// Responses: 201 `accepted`, 200 `already_suggested`, 409 `already_covered`,
/// 409 `refused`, 400 `invalid`, 429 `rate_limited` (with `Retry-After`).
async fn suggest(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<SuggestionRequest>, JsonRejection>,
) -> Response {
    let req = match body {
        Ok(Json(req)) => req,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "status": "invalid", "error": e.body_text() })),
            )
                .into_response();
        }
    };
    let outcome = submit_suggestion(&state, peer, &headers, &req.url, req.note.as_deref()).await;
    match outcome {
        Ok(Outcome::Accepted { domain, issue }) => (
            StatusCode::CREATED,
            Json(json!({ "status": "accepted", "domain": domain, "github_issue": issue })),
        )
            .into_response(),
        Ok(Outcome::AlreadySuggested { domain }) => (
            StatusCode::OK,
            Json(json!({ "status": "already_suggested", "domain": domain })),
        )
            .into_response(),
        Ok(Outcome::AlreadyCovered { source }) => (
            StatusCode::CONFLICT,
            Json(json!({ "status": "already_covered", "source": source })),
        )
            .into_response(),
        Ok(Outcome::Refused(r)) => (
            StatusCode::CONFLICT,
            Json(json!({
                "status": "refused",
                "message": crate::suggestions::refused_message(&r),
                "refused": RefusedJson::from(*r),
            })),
        )
            .into_response(),
        Ok(Outcome::RateLimited { retry_after_secs }) => (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, retry_after_secs.to_string())],
            Json(json!({ "status": "rate_limited", "retry_after_secs": retry_after_secs })),
        )
            .into_response(),
        Ok(Outcome::Invalid(e)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "status": "invalid", "error": e.to_string() })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "suggestion failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error" })),
            )
                .into_response()
        }
    }
}

/// Validate, dedupe, rate-limit and file a suggestion from `peer` (shared by
/// the JSON and HTML handlers).
pub(crate) async fn submit_suggestion(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
    url: &str,
    note: Option<&str>,
) -> anyhow::Result<Outcome> {
    let forwarded_for = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok());
    let client = state.suggestions.client_ip(peer.ip(), forwarded_for);
    state
        .suggestions
        .submit(&state.pool, url, note, client)
        .await
}

/// `GET /v1/transit?from=lat,lng&event=<id>`: walking and (when it's
/// quicker) public-transport time from `from` to the event's venue. `from`
/// is rounded to ~200 m before it is used and is never logged or stored.
/// Always 200 for a known event with valid parameters; `status` says why
/// `transit` is null (`short_walk`, `not_faster`, `unavailable`,
/// `outside_area`, `no_location`).
async fn get_transit(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Response {
    use crate::transit::{LatLng, Outcome, distance_km, round_origin, walk_minutes};
    let bad = |msg: &str| (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response();
    let mut from = None;
    let mut event = None;
    for (k, v) in url::form_urlencoded::parse(raw.as_deref().unwrap_or("").as_bytes()) {
        match k.as_ref() {
            "from" => from = Some(v.into_owned()),
            "event" => event = Some(v.into_owned()),
            _ => {}
        }
    }
    let Some(from) = from.as_deref().and_then(LatLng::parse) else {
        return bad("from must be lat,lng");
    };
    let Some(id) = event.as_deref().and_then(|e| Uuid::parse_str(e).ok()) else {
        return bad("event must be an event id");
    };
    let e = match repo::get_event(&state.pool, id).await {
        Ok(Some(e)) => e,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(err) => return ApiError::Internal(err).into_response(),
    };
    let (Some(lat), Some(lng)) = (e.lat, e.lng) else {
        return transit_json(Value::Null, Value::Null, "no_location");
    };
    let to = LatLng { lat, lng };
    let (origin, _) = round_origin(from);
    let km = distance_km(origin, to);
    let walk = json!({ "minutes": walk_minutes(km), "km": (km * 10.0).round() / 10.0 });
    let Some(transit) = &state.transit else {
        return transit_json(walk, Value::Null, "unavailable");
    };
    let forwarded_for = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok());
    let client = state.suggestions.client_ip(peer.ip(), forwarded_for);
    let outcome = transit
        .lookup(origin, to, e.venue_name.as_deref(), client, Utc::now())
        .await;
    match outcome {
        Outcome::Journey {
            journey,
            provider,
            links,
        } => transit_json(
            walk,
            json!({
                "minutes": journey.minutes,
                "summary": journey.summary,
                "modes": journey.modes,
                "provider": provider,
                "links": links,
            }),
            "ok",
        ),
        Outcome::ShortWalk => transit_json(walk, Value::Null, "short_walk"),
        Outcome::NotFaster => transit_json(walk, Value::Null, "not_faster"),
        Outcome::OutsideArea => transit_json(walk, Value::Null, "outside_area"),
        Outcome::Unavailable => transit_json(walk, Value::Null, "unavailable"),
    }
}

fn transit_json(walk: Value, transit: Value, status: &str) -> Response {
    (
        [(header::CACHE_CONTROL, "private, max-age=300")],
        Json(json!({ "status": status, "walk": walk, "transit": transit })),
    )
        .into_response()
}

/// Read-API failures, rendered as `{"error": "..."}`.
enum ApiError {
    BadRequest(String),
    NotFound,
    Internal(sqlx::Error),
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found".into()),
            ApiError::Internal(e) => {
                tracing::error!(error = %e, "read API query failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

#[derive(Serialize)]
pub(crate) struct EventJson {
    pub(crate) id: Uuid,
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    pub(crate) venue_name: Option<String>,
    pub(crate) address: Option<String>,
    pub(crate) lat: Option<f64>,
    pub(crate) lng: Option<f64>,
    pub(crate) starts_at: DateTime<Utc>,
    pub(crate) ends_at: Option<DateTime<Utc>>,
    /// The source gave dates but no time of day: `starts_at`/`ends_at` are
    /// London midnight of the first/last day (inclusive).
    pub(crate) all_day: bool,
    pub(crate) is_free: bool,
    pub(crate) price_min: Option<Decimal>,
    pub(crate) price_max: Option<Decimal>,
    pub(crate) currency: Option<String>,
    pub(crate) url: Option<String>,
    /// Our own small copy of the event's image (`/thumbs/...`); the
    /// source's image URL is never exposed, so nobody hotlinks it.
    pub(crate) thumbnail_url: Option<String>,
    pub(crate) image_credit: Option<ImageCreditJson>,
    #[serde(skip)]
    pub(crate) thumbnail_size: Option<(i32, i32)>,
    pub(crate) category: String,
    pub(crate) tags: Vec<String>,
    /// Art forms (AI enrichment, else the sources' default tags).
    pub(crate) medium_tags: Vec<String>,
    /// Kind of occasion (AI enrichment).
    pub(crate) format_tags: Vec<String>,
    /// Who it suits (AI enrichment).
    pub(crate) good_for: Vec<String>,
    pub(crate) vibe_tags: Vec<String>,
    /// Music subtags (`crate::music::MUSIC_TAGS`; deterministic, only for
    /// `music` events).
    pub(crate) music_tags: Vec<String>,
    /// Private view / opening / launch (AI enrichment; null = unknown).
    pub(crate) is_opening: Option<bool>,
    /// AI-written notes, labelled as such; never the venue's words.
    pub(crate) ai: Option<AiNoteJson>,
    /// Weekly opening hours in London time (`crate::hours`), for all-day
    /// runs; absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) opening_hours: Option<Vec<HoursRuleJson>>,
    /// The listing's own wording about its hours; absent when none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hours_note: Option<String>,
    #[serde(skip)]
    pub(crate) hours: Option<crate::hours::OpeningHours>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) distance_km: Option<f64>,
    pub(crate) sources: Vec<SourceLinkJson>,
    /// The venue's page on this site is `/venues/<venue_slug>` (#204);
    /// absent when the event has no venue (e.g. an area or no name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) venue_slug: Option<String>,
}

/// One line of an event's weekly opening hours.
#[derive(Serialize)]
pub(crate) struct HoursRuleJson {
    /// `mon`..`sun`.
    pub(crate) days: Vec<&'static str>,
    /// `HH:MM`, London time.
    pub(crate) opens: String,
    /// `HH:MM`, London time, after `opens`.
    pub(crate) closes: String,
}

/// The AI-written part of an event.
#[derive(Serialize)]
pub(crate) struct AiNoteJson {
    /// Always "AI-generated".
    pub(crate) label: &'static str,
    /// "What's cool about this" (<= 220 characters), or null when the
    /// listing gave too little to say something specific.
    pub(crate) whats_cool: Option<String>,
    /// Neutral summary for cards (<= 90 characters).
    pub(crate) one_liner: Option<String>,
    /// `listing` or `listing_plus_general_knowledge` (or `insufficient`).
    pub(crate) grounding: String,
    pub(crate) model: String,
    pub(crate) generated_at: DateTime<Utc>,
}

/// Who the thumbnail's image belongs to, and where to see it in context.
#[derive(Serialize)]
pub(crate) struct ImageCreditJson {
    /// Human-readable source name ("Barbican").
    pub(crate) name: String,
    /// The event's page on that source (else the source's site).
    pub(crate) url: String,
}

#[derive(Serialize)]
pub(crate) struct SourceLinkJson {
    pub(crate) source: String,
    pub(crate) display_name: String,
    #[serde(skip)]
    pub(crate) kind: SourceKind,
    pub(crate) url: Option<String>,
    pub(crate) first_seen_at: DateTime<Utc>,
    pub(crate) last_seen_at: DateTime<Utc>,
}

impl EventJson {
    fn new(
        e: EventRow,
        distance_km: Option<f64>,
        sources: Vec<SourceLinkJson>,
        thumb: Option<ThumbnailMeta>,
    ) -> Self {
        Self {
            id: e.id,
            title: e.title,
            description: e.description,
            venue_name: e.venue_name,
            address: e.address,
            lat: e.lat,
            lng: e.lng,
            starts_at: e.starts_at,
            ends_at: e.ends_at,
            all_day: e.all_day,
            is_free: e.is_free,
            price_min: e.price_min,
            price_max: e.price_max,
            currency: e.currency,
            url: e.url,
            thumbnail_url: thumb
                .as_ref()
                .map(|t| crate::thumbs::thumb_path(e.id, &t.content_hash)),
            thumbnail_size: thumb.as_ref().map(|t| (t.width, t.height)),
            image_credit: thumb.map(|t| ImageCreditJson {
                name: repo::display_name(&t.credit_key, t.credit_display_name.as_deref()),
                url: t.credit_url,
            }),
            category: e.category,
            tags: e.tags,
            medium_tags: e.medium_tags,
            format_tags: e.format_tags,
            good_for: e.good_for,
            vibe_tags: e.vibe_tags,
            music_tags: e.music_tags,
            is_opening: e.is_opening,
            ai: match (e.ai_grounding, e.ai_model, e.ai_enriched_at) {
                (Some(grounding), Some(model), Some(generated_at)) => Some(AiNoteJson {
                    label: "AI-generated",
                    whats_cool: e.whats_cool,
                    one_liner: e.one_liner,
                    grounding,
                    model,
                    generated_at,
                }),
                _ => None,
            },
            opening_hours: e.opening_hours.as_ref().map(|h| {
                h.0.0
                    .iter()
                    .map(|r| HoursRuleJson {
                        days: r.days.iter().map(|&d| crate::hours::day_code(d)).collect(),
                        opens: r.opens.format("%H:%M").to_string(),
                        closes: r.closes.format("%H:%M").to_string(),
                    })
                    .collect()
            }),
            hours_note: e.hours_note,
            hours: e.opening_hours.map(|h| h.0),
            distance_km,
            sources,
            venue_slug: e.venue_slug,
        }
    }
}

async fn source_links(
    pool: &PgPool,
    ids: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, Vec<SourceLinkJson>>> {
    let mut by_event: HashMap<Uuid, Vec<SourceLinkJson>> = HashMap::new();
    for l in repo::event_source_links(pool, ids).await? {
        by_event
            .entry(l.event_id)
            .or_default()
            .push(SourceLinkJson {
                display_name: repo::display_name(&l.source, l.display_name.as_deref()),
                source: l.source,
                kind: l.kind,
                url: l.source_url,
                first_seen_at: l.first_seen_at,
                last_seen_at: l.last_seen_at,
            });
    }
    Ok(by_event)
}

async fn thumbnails(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, ThumbnailMeta>> {
    Ok(repo::thumbnail_meta(pool, ids)
        .await?
        .into_iter()
        .map(|t| (t.event_id, t))
        .collect())
}

/// One page of events for `query` and the cursor of the next page (shared
/// by `GET /v1/events` and the HTML home page).
pub(crate) async fn event_page(
    pool: &PgPool,
    query: &listing::EventQuery,
) -> sqlx::Result<(Vec<EventJson>, Option<String>)> {
    let mut rows = repo::list_events(pool, query).await?;
    let has_more = rows.len() as i64 > query.limit;
    rows.truncate(query.limit as usize);
    let next_cursor = rows
        .last()
        .filter(|_| has_more)
        .and_then(|last| {
            let id = last.event.id;
            match &query.order {
                EventOrder::ByStart { .. } => Some(Cursor::Start(last.event.starts_at, id)),
                EventOrder::ByDistance { .. } => last.distance_km.map(|d| Cursor::Distance(d, id)),
                EventOrder::ByEnd { .. } => last.sort_at.map(|t| Cursor::End(t, id)),
                EventOrder::ByAdded { .. } => last.sort_at.map(|t| Cursor::Added(t, id)),
                EventOrder::Shuffled { seed, .. } => Some(Cursor::Shuffle(*seed, id)),
                EventOrder::ByRelevance { .. } => last.relevance.map(|r| Cursor::Relevance(r, id)),
                EventOrder::Richest { today, .. } => last
                    .rich_day
                    .zip(last.rich_slot)
                    .map(|(day, slot)| Cursor::Richest(*today, day, slot, id)),
            }
        })
        .map(|c| c.encode());
    let ids: Vec<Uuid> = rows.iter().map(|r| r.event.id).collect();
    let mut links = source_links(pool, &ids).await?;
    let mut thumbs = thumbnails(pool, &ids).await?;
    let events = rows
        .into_iter()
        .map(|r| {
            let sources = links.remove(&r.event.id).unwrap_or_default();
            let thumb = thumbs.remove(&r.event.id);
            EventJson::new(r.event, r.distance_km, sources, thumb)
        })
        .collect();
    Ok((events, next_cursor))
}

/// How many events each `when=` and price option would list (see
/// `repo::listing_counts`).
#[derive(Debug, Serialize)]
pub(crate) struct CountsJson {
    pub(crate) when: WhenCountsJson,
    pub(crate) price: PriceCountsJson,
}

#[derive(Debug, Serialize)]
pub(crate) struct WhenCountsJson {
    pub(crate) evening: i64,
    pub(crate) after_work: i64,
    pub(crate) weekend: i64,
    pub(crate) daytime: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct PriceCountsJson {
    pub(crate) free: i64,
    pub(crate) max_10: i64,
    pub(crate) max_20: i64,
    pub(crate) unknown: i64,
}

impl From<ListingCounts> for CountsJson {
    fn from(c: ListingCounts) -> Self {
        Self {
            when: WhenCountsJson {
                evening: c.evening,
                after_work: c.after_work,
                weekend: c.weekend,
                daytime: c.daytime,
            },
            price: PriceCountsJson {
                free: c.free,
                max_10: c.max_10,
                max_20: c.max_20,
                unknown: c.unknown,
            },
        }
    }
}

/// Facet counts for `query` (shared by `GET /v1/events` and the home page).
pub(crate) async fn event_counts(
    pool: &PgPool,
    query: &listing::EventQuery,
) -> sqlx::Result<CountsJson> {
    Ok(
        repo::listing_counts(pool, &query.filter, query.near.as_ref())
            .await?
            .into(),
    )
}

/// Correct typos in `query`'s search, if any (see [`crate::search`]).
pub(crate) async fn resolve_search(
    pool: &PgPool,
    query: &mut listing::EventQuery,
) -> sqlx::Result<()> {
    if let Some(search) = query.filter.search.take() {
        query.filter.search = Some(crate::search::resolve(pool, search).await?);
    }
    Ok(())
}

async fn list_events(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let mut query =
        listing::parse_query(raw.as_deref().unwrap_or("")).map_err(ApiError::BadRequest)?;
    resolve_search(&state.pool, &mut query).await?;
    let (events, next_cursor) = event_page(&state.pool, &query).await?;
    let counts = event_counts(&state.pool, &query).await?;
    let mut body = json!({
        "events": events,
        "next_cursor": next_cursor,
        "counts": counts,
        "sort": query.order.sort().as_str(),
    });
    if let Some(wanted) = query.fell_back_from {
        body["sort_fallback"] = json!({
            "requested": wanted.as_str(),
            "reason": match wanted {
                listing::Sort::Relevance => "relevance needs q=<search>",
                _ => "nearest needs near=<lat>,<lng>",
            },
        });
    }
    if let Some(search) = &query.filter.search {
        body["search"] = json!({ "q": search.text, "corrected": search.corrected });
    }
    if query.facets {
        body["facets"] = facets(&state.pool, &query).await?;
    }
    Ok(Json(body))
}

/// `{"medium": {"photography": 12, ...}, "format": {...}, "good_for": {...}}`
/// for `query` (each facet ignoring its own selection).
pub(crate) async fn facets(pool: &PgPool, query: &listing::EventQuery) -> sqlx::Result<Value> {
    let mut out = serde_json::Map::new();
    for facet in repo::Facet::ALL {
        let counts: serde_json::Map<String, Value> = repo::facet_counts(pool, query, facet)
            .await?
            .into_iter()
            .map(|(t, n)| (t, json!(n)))
            .collect();
        out.insert(facet.name().into(), Value::Object(counts));
    }
    Ok(Value::Object(out))
}

/// Events most like `id` ("More like this"), still running today or later.
#[derive(Serialize)]
pub(crate) struct SimilarJson {
    pub(crate) id: Uuid,
    pub(crate) title: String,
    pub(crate) venue_name: Option<String>,
    pub(crate) starts_at: DateTime<Utc>,
    pub(crate) ends_at: Option<DateTime<Utc>>,
    pub(crate) all_day: bool,
    pub(crate) category: String,
    pub(crate) similarity: f64,
    /// Tags it shares with the event (why it is similar), as tag values.
    pub(crate) shared_tags: Vec<String>,
}

/// Up to 6 similar upcoming events (empty when embeddings are off).
pub(crate) async fn similar_events(
    pool: &PgPool,
    event: &EventJson,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<SimilarJson>> {
    let since = crate::enrich::london_midnight(now);
    let mine: Vec<&String> = event
        .medium_tags
        .iter()
        .chain(&event.format_tags)
        .chain(&event.good_for)
        .chain(&event.vibe_tags)
        .collect();
    Ok(
        crate::enrich::store::more_like_this(pool, event.id, since, 6)
            .await?
            .into_iter()
            .map(|s| {
                let shared_tags = s
                    .medium_tags
                    .iter()
                    .chain(&s.format_tags)
                    .chain(&s.good_for)
                    .chain(&s.vibe_tags)
                    .filter(|t| mine.contains(t))
                    .cloned()
                    .collect();
                SimilarJson {
                    id: s.id,
                    title: s.title,
                    venue_name: s.venue_name,
                    starts_at: s.starts_at,
                    ends_at: s.ends_at,
                    all_day: s.all_day,
                    category: s.category,
                    similarity: s.similarity,
                    shared_tags,
                }
            })
            .collect(),
    )
}

async fn get_similar(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
    let event = event_by_id(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let similar = similar_events(&state.pool, &event, Utc::now()).await?;
    Ok(Json(json!({ "similar": similar })))
}

/// One event with its source links, or `None` if the id is unknown.
pub(crate) async fn event_by_id(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<EventJson>> {
    let Some(event) = repo::get_event(pool, id).await? else {
        return Ok(None);
    };
    let sources = source_links(pool, &[id])
        .await?
        .remove(&id)
        .unwrap_or_default();
    let thumb = thumbnails(pool, &[id]).await?.remove(&id);
    Ok(Some(EventJson::new(event, None, sources, thumb)))
}

/// A venue's upcoming events (at most `limit`), soonest first, for its page.
pub(crate) async fn venue_events(
    pool: &PgPool,
    venue_id: i64,
    now: DateTime<Utc>,
    limit: i64,
) -> sqlx::Result<Vec<EventJson>> {
    let rows = repo::venue_events(pool, venue_id, now, limit).await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut links = source_links(pool, &ids).await?;
    let mut thumbs = thumbnails(pool, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let sources = links.remove(&r.id).unwrap_or_default();
            let thumb = thumbs.remove(&r.id);
            EventJson::new(r, None, sources, thumb)
        })
        .collect())
}

/// Ids that are not UUIDs are unknown ids too: 404, not 400.
async fn get_event(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<EventJson>, ApiError> {
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
    let event = event_by_id(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(event))
}

#[derive(Serialize)]
struct SourceJson {
    key: String,
    display_name: String,
    kind: &'static str,
    interval_minutes: i32,
    enabled: bool,
    last_run: Option<LastRunJson>,
    skip: Option<SkipJson>,
    status: HealthStatus,
    issue_url: Option<String>,
    qa: QaJson,
}

/// Scraper QA (never links to an issue).
#[derive(Serialize)]
struct QaJson {
    rule_flags: i64,
    last_check: Option<QaCheckJson>,
}

#[derive(Serialize)]
struct QaCheckJson {
    checked_at: DateTime<Utc>,
    status: String,
    problems: i32,
}

#[derive(Serialize)]
struct LastRunJson {
    started_at: DateTime<Utc>,
    events_found: i32,
    errors: i32,
    duration_ms: i64,
    ok: bool,
}

#[derive(Serialize)]
struct SkipJson {
    at: DateTime<Utc>,
    reason: String,
}

impl SourceJson {
    fn new(r: SourceStatusRow, github_repo: &str) -> Self {
        let last_run = match (
            r.run_started_at,
            r.run_events_found,
            r.run_errors,
            r.run_duration_ms,
            r.run_ok,
        ) {
            (Some(started_at), Some(events_found), Some(errors), Some(duration_ms), Some(ok)) => {
                Some(LastRunJson {
                    started_at,
                    events_found,
                    errors,
                    duration_ms,
                    ok,
                })
            }
            _ => None,
        };
        let skip = match (r.skip_reason, r.skipped_at) {
            (Some(reason), Some(at)) => Some(SkipJson { at, reason }),
            _ => None,
        };
        let status = HealthStatus::of(
            skip.is_some(),
            r.open_issue_number.is_some(),
            last_run.as_ref().map(|l| RunStats {
                events_found: l.events_found,
                errors: l.errors,
                ok: l.ok,
            }),
        );
        Self {
            display_name: repo::display_name(&r.key, r.display_name.as_deref()),
            key: r.key,
            kind: r.kind.as_str(),
            interval_minutes: r.interval_minutes,
            enabled: r.enabled,
            last_run,
            skip,
            status,
            issue_url: r
                .open_issue_number
                .map(|n| format!("https://github.com/{github_repo}/issues/{n}")),
            qa: QaJson {
                rule_flags: r.qa_rule_flags,
                last_check: match (r.qa_checked_at, r.qa_status) {
                    (Some(checked_at), Some(status)) => Some(QaCheckJson {
                        checked_at,
                        status,
                        problems: r.qa_problems.unwrap_or(0),
                    }),
                    _ => None,
                },
            },
        }
    }
}

async fn list_sources(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let refused: Vec<RefusedJson> = repo::refused_sources(&state.pool)
        .await?
        .into_iter()
        .map(RefusedJson::from)
        .collect();
    Ok(Json(
        json!({ "sources": source_values(&state).await?, "refused": refused }),
    ))
}

/// A site we decided not to scrape (`refused` in `GET /v1/sources`).
#[derive(Serialize)]
pub(crate) struct RefusedJson {
    pub(crate) name: String,
    pub(crate) domain: String,
    pub(crate) url: String,
    pub(crate) reason_code: String,
    pub(crate) reason_text: String,
    pub(crate) checked_on: NaiveDate,
    pub(crate) issue_url: Option<String>,
}

impl From<RefusedSourceRow> for RefusedJson {
    fn from(r: RefusedSourceRow) -> Self {
        Self {
            name: r.name,
            domain: r.domain,
            url: r.url,
            reason_code: r.reason_code,
            reason_text: r.reason_text,
            checked_on: r.checked_on,
            issue_url: r.issue_url,
        }
    }
}

/// Every source exactly as `GET /v1/sources` serialises it (the HTML page
/// renders these, so new fields and statuses show up without changes).
pub(crate) async fn source_values(state: &AppState) -> sqlx::Result<Vec<Value>> {
    let sources: Vec<SourceJson> = repo::source_statuses(&state.pool)
        .await?
        .into_iter()
        .map(|r| SourceJson::new(r, &state.github_repo))
        .collect();
    Ok(sources
        .iter()
        .map(|s| serde_json::to_value(s).expect("SourceJson serialises"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    /// A router whose database is never reachable (a lazy pool).
    fn offline_router() -> Router {
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(300))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .unwrap();
        let suggestions = Suggestions::new(
            crate::config::SuggestionConfig {
                ip_salt: Some("salt".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let settings = ApiSettings {
            github_repo: "owner/repo".into(),
            cors_origins: Vec::new(),
        };
        router(pool, suggestions, settings)
    }

    #[tokio::test]
    async fn healthz_returns_503_when_db_unreachable() {
        let resp = offline_router()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn every_response_carries_the_security_headers() {
        for uri in [
            "/healthz",
            "/static/app.js",
            "/favicon.svg",
            "/no-such-page",
        ] {
            let resp = offline_router()
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            for (name, value) in SECURITY_HEADERS {
                assert_eq!(resp.headers().get(name).unwrap(), value, "{uri} {name}");
            }
        }
        // HTML pages keep their own CSP.
        let resp = offline_router()
            .oneshot(Request::get("/saved").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_SECURITY_POLICY).unwrap(),
            crate::web::CSP
        );
        assert!(
            resp.headers()
                .contains_key(header::STRICT_TRANSPORT_SECURITY)
        );
    }

    #[tokio::test]
    async fn cross_site_form_posts_are_refused() {
        use axum::extract::connect_info::MockConnectInfo;
        let peer = SocketAddr::from(([203, 0, 113, 7], 4000));
        for uri in ["/suggest", "/contact"] {
            let resp = offline_router()
                .layer(MockConnectInfo(peer))
                .oneshot(
                    Request::post(uri)
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .header("sec-fetch-site", "cross-site")
                        .body(Body::from("url=https%3A%2F%2Fexample.org%2F"))
                        .unwrap(),
                )
                .await
                .unwrap();
            // Refused before the database is touched (it is unreachable here).
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
        }
    }
}
