//! The legacy-host redirect (thaleia.interstellarai.net → canonical host).

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::get;
use musenmingle::host_redirect::{self, DEFAULT_LEGACY_HOSTS, HostRedirect};
use tower::ServiceExt;

const CANONICAL: &str = "musenmingle.interstellarai.net";

fn app(cfg: HostRedirect) -> Router {
    let inner = Router::new()
        .route("/", get(|| async { "home" }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/x", get(|| async { "x" }).post(|| async { "posted" }));
    host_redirect::apply(inner, cfg)
}

async fn send(app: &Router, method: Method, host: &str, uri: &str) -> (StatusCode, Option<String>) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, host)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let loc = resp
        .headers()
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_string());
    (resp.status(), loc)
}

#[tokio::test]
async fn legacy_host_redirects_permanently_keeping_path_and_query() {
    let app = app(HostRedirect::new(Some(CANONICAL), DEFAULT_LEGACY_HOSTS));
    assert_eq!(
        send(
            &app,
            Method::GET,
            "thaleia.interstellarai.net",
            "/x?q=1&page=2"
        )
        .await,
        (
            StatusCode::MOVED_PERMANENTLY,
            Some("https://musenmingle.interstellarai.net/x?q=1&page=2".into())
        )
    );
    assert_eq!(
        send(&app, Method::GET, "Thaleia.Interstellarai.net:443", "/").await,
        (
            StatusCode::MOVED_PERMANENTLY,
            Some("https://musenmingle.interstellarai.net/".into())
        )
    );
    assert_eq!(
        send(&app, Method::HEAD, "thaleia.interstellarai.net", "/x")
            .await
            .0,
        StatusCode::MOVED_PERMANENTLY
    );
    // Non-safe methods keep their method and body.
    assert_eq!(
        send(&app, Method::POST, "thaleia.interstellarai.net", "/x").await,
        (
            StatusCode::PERMANENT_REDIRECT,
            Some("https://musenmingle.interstellarai.net/x".into())
        )
    );
}

#[tokio::test]
async fn other_hosts_and_healthz_are_served() {
    let app = app(HostRedirect::new(Some(CANONICAL), DEFAULT_LEGACY_HOSTS));
    for host in [
        CANONICAL,
        "musenmingle-api-production.up.railway.app",
        "localhost:8080",
        "healthcheck.railway.app",
    ] {
        assert_eq!(
            send(&app, Method::GET, host, "/x").await,
            (StatusCode::OK, None),
            "{host}"
        );
    }
    assert_eq!(
        send(&app, Method::GET, "thaleia.interstellarai.net", "/healthz").await,
        (StatusCode::OK, None)
    );
}

#[tokio::test]
async fn inert_without_a_canonical_host() {
    let app = app(HostRedirect::new(None, DEFAULT_LEGACY_HOSTS));
    assert_eq!(
        send(&app, Method::GET, "thaleia.interstellarai.net", "/x").await,
        (StatusCode::OK, None)
    );
}
