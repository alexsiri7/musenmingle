//! HTTP API: `GET /healthz` and `POST /v1/suggestions`; the read API is a
//! later phase.
//!
//! Handlers that need the client address extract `ConnectInfo`, so serve
//! the router with `into_make_service_with_connect_info::<SocketAddr>()`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::suggestions::{Outcome, Suggestions};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub suggestions: Arc<Suggestions>,
}

pub fn router(pool: PgPool, suggestions: Suggestions) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/suggestions", post(suggest))
        .with_state(AppState {
            pool,
            suggestions: Arc::new(suggestions),
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
/// 400 `invalid`, 429 `rate_limited` (with `Retry-After`).
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
    let forwarded_for = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok());
    let client = state.suggestions.client_ip(peer.ip(), forwarded_for);
    let outcome = state
        .suggestions
        .submit(&state.pool, &req.url, req.note.as_deref(), client)
        .await;
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
        let resp = router(pool, suggestions)
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
