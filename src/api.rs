//! HTTP API. For now only `GET /healthz`; the read API is a later phase.

use std::time::Duration;

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde_json::{Value, json};
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
}

pub fn router(pool: PgPool) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(AppState { pool })
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
        let resp = router(pool)
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
