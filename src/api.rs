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
use crate::repo::{self, EventRow, RefusedSourceRow, SourceStatusRow};
use crate::suggestions::{Outcome, Suggestions};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub suggestions: Arc<Suggestions>,
    pub github_repo: Arc<str>,
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
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(settings.cors_origins))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/events", get(list_events))
        .route("/v1/events/{id}", get(get_event))
        .route("/v1/sources", get(list_sources))
        .route("/v1/suggestions", post(suggest))
        .merge(crate::web::routes())
        .layer(cors)
        .with_state(AppState {
            pool,
            suggestions: Arc::new(suggestions),
            github_repo: settings.github_repo.into(),
        })
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
    pub(crate) is_free: bool,
    pub(crate) price_min: Option<Decimal>,
    pub(crate) price_max: Option<Decimal>,
    pub(crate) currency: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) image_url: Option<String>,
    pub(crate) category: String,
    pub(crate) tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) distance_km: Option<f64>,
    pub(crate) sources: Vec<SourceLinkJson>,
}

#[derive(Serialize)]
pub(crate) struct SourceLinkJson {
    pub(crate) source: String,
    pub(crate) url: Option<String>,
    pub(crate) first_seen_at: DateTime<Utc>,
    pub(crate) last_seen_at: DateTime<Utc>,
}

impl EventJson {
    fn new(e: EventRow, distance_km: Option<f64>, sources: Vec<SourceLinkJson>) -> Self {
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
            is_free: e.is_free,
            price_min: e.price_min,
            price_max: e.price_max,
            currency: e.currency,
            url: e.url,
            image_url: e.image_url,
            category: e.category,
            tags: e.tags,
            distance_km,
            sources,
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
                source: l.source,
                url: l.source_url,
                first_seen_at: l.first_seen_at,
                last_seen_at: l.last_seen_at,
            });
    }
    Ok(by_event)
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
        .and_then(|last| match (&query.order, last.distance_km) {
            (EventOrder::ByStart { .. }, _) => {
                Some(Cursor::Start(last.event.starts_at, last.event.id))
            }
            (EventOrder::ByDistance { .. }, Some(d)) => Some(Cursor::Distance(d, last.event.id)),
            (EventOrder::ByDistance { .. }, None) => None,
        })
        .map(|c| c.encode());
    let ids: Vec<Uuid> = rows.iter().map(|r| r.event.id).collect();
    let mut links = source_links(pool, &ids).await?;
    let events = rows
        .into_iter()
        .map(|r| {
            let sources = links.remove(&r.event.id).unwrap_or_default();
            EventJson::new(r.event, r.distance_km, sources)
        })
        .collect();
    Ok((events, next_cursor))
}

async fn list_events(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let query = listing::parse_query(raw.as_deref().unwrap_or("")).map_err(ApiError::BadRequest)?;
    let (events, next_cursor) = event_page(&state.pool, &query).await?;
    Ok(Json(
        json!({ "events": events, "next_cursor": next_cursor }),
    ))
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
    Ok(Some(EventJson::new(event, None, sources)))
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
    kind: &'static str,
    interval_minutes: i32,
    enabled: bool,
    last_run: Option<LastRunJson>,
    skip: Option<SkipJson>,
    status: HealthStatus,
    issue_url: Option<String>,
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

    #[tokio::test]
    async fn healthz_returns_503_when_db_unreachable() {
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
        let resp = router(pool, suggestions, settings)
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
