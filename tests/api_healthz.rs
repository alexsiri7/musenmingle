//! `GET /healthz` against a real database.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestDb;
use tower::ServiceExt;

#[tokio::test]
async fn healthz_ok_with_database() {
    let Some(db) = TestDb::create("healthz_ok_with_database").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let suggestions = musenmingle::suggestions::Suggestions::new(
        musenmingle::config::SuggestionConfig {
            ip_salt: Some("salt".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let settings = musenmingle::api::ApiSettings {
        github_repo: "alexsiri7/musenmingle".into(),
        cors_origins: Vec::new(),
    };
    let resp = musenmingle::api::router(pool.clone(), suggestions, settings)
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["db"], "ok");
    pool.close().await;
    db.drop_db().await;
}
