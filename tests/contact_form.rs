//! The public contact form (`GET/POST /contact`): spam protection, issue
//! filing through a wiremock GitHub, dedupe, the GitHub-down pending path
//! and what may (not) reach the issue.

mod common;

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use common::TestDb;
use serde_json::{Value, json};
use sqlx::PgPool;
use thaleia::api::ApiSettings;
use thaleia::config::SuggestionConfig;
use thaleia::contact;
use thaleia::github::{GitHubIssueFiler, IssueFiler};
use thaleia::suggestions::Suggestions;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO: &str = "alexsiri7/thaleia";
const SALT: &str = "test-salt";

fn app(pool: &PgPool, filer: Option<Box<dyn IssueFiler>>) -> Router {
    thaleia::api::router(
        pool.clone(),
        Suggestions::new(
            SuggestionConfig {
                ip_salt: Some(SALT.into()),
                ..Default::default()
            },
            filer,
        )
        .unwrap(),
        ApiSettings {
            github_repo: REPO.into(),
            cors_origins: Vec::new(),
        },
    )
    .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 7], 4000))))
}

fn filer(server: &MockServer) -> Option<Box<dyn IssueFiler>> {
    Some(Box::new(
        GitHubIssueFiler::new(&server.uri(), REPO, "token").unwrap(),
    ))
}

/// A form body with a token old enough to pass the fill-time check.
fn body(t: &str, url: &str, details: &str, email: &str, honeypot: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("request_type", t)
        .append_pair("url", url)
        .append_pair("details", details)
        .append_pair("reply_email", email)
        .append_pair("website", honeypot)
        .append_pair(
            "token",
            &contact::form_token(SALT, Utc::now() - Duration::seconds(10)),
        )
        .finish()
}

async fn post(app: &Router, body: String) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::post("/contact")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn rows(pool: &PgPool) -> Vec<(String, String, Option<String>, Option<i64>, String)> {
    sqlx::query_as(
        "SELECT request_type, domain, reply_email, github_issue_number, status
         FROM events.contact_requests ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn mock_create(server: &MockServer, number: i64) {
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "number": number, "title": "x"
        })))
        .mount(server)
        .await;
}

async fn requests_to(server: &MockServer, suffix: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with(suffix))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

#[tokio::test]
async fn form_renders_with_honeypot_and_token() {
    let Some(db) = TestDb::create("contact_form_renders").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let resp = app(&pool, None)
        .oneshot(Request::get("/contact").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let csp = resp.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("form-action 'self'"), "{csp}");
    let html = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    for needle in [
        "<form class=\"contact\" method=\"post\" action=\"/contact\">",
        "value=\"remove_listings\"",
        "value=\"correct_event\"",
        "value=\"other\"",
        "name=\"url\"",
        "name=\"details\"",
        "Your email (optional)",
        "We only use it to reply to you",
        "name=\"website\"",
        "name=\"token\"",
    ] {
        assert!(html.contains(needle), "missing {needle}");
    }
    assert!(!html.contains("<script>"));
    assert!(!html.contains("github.com"));
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn honeypot_and_too_fast_submissions_are_dropped_silently() {
    let Some(db) = TestDb::create("contact_honeypot").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let server = MockServer::start().await;
    mock_create(&server, 1).await;
    let app = app(&pool, filer(&server));

    let (status, html) = post(
        &app,
        body("other", "example.org", "hi", "", "http://spam.example"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Thanks, we&#39;ve got it") || html.contains("Thanks, we've got it"));

    // Token issued just now: too fast.
    let fast = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("request_type", "other")
        .append_pair("url", "example.org")
        .append_pair("details", "hi")
        .append_pair("token", &contact::form_token(SALT, Utc::now()))
        .finish();
    let (status, _) = post(&app, fast).await;
    assert_eq!(status, StatusCode::OK);
    // No token at all.
    let (status, _) = post(&app, "request_type=other&url=example.org&details=hi".into()).await;
    assert_eq!(status, StatusCode::OK);

    assert!(rows(&pool).await.is_empty());
    assert!(requests_to(&server, "/issues").await.is_empty());
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn files_an_inert_issue_without_the_email_and_dedupes_by_comment() {
    let Some(db) = TestDb::create("contact_files_issue").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let server = MockServer::start().await;
    mock_create(&server, 77).await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues/77/comments")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
        .mount(&server)
        .await;
    let app = app(&pool, filer(&server));

    let evil =
        "<script>alert(1)</script> **bold** ```\n# heading\n[x](javascript:alert(1)) @someone";
    let (status, html) = post(
        &app,
        body(
            "remove_listings",
            "https://www.example-venue.org/whats-on",
            evil,
            "owner@example-venue.org",
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(!html.contains("github.com"));
    assert!(!html.contains("#77"));

    let created = requests_to(&server, "/issues").await;
    assert_eq!(created.len(), 1);
    let issue = &created[0];
    assert_eq!(
        issue["title"],
        "Venue request: example-venue.org (remove listings)"
    );
    assert_eq!(issue["labels"], json!(["venue-request"]));
    let b = issue["body"].as_str().unwrap();
    assert!(!b.contains("owner@example-venue.org"), "email leaked: {b}");
    assert!(!b.contains("```\n# heading"), "fence broken: {b}");
    let script = b.find("<script>").unwrap();
    let open = b[..script].rfind("```text\n").unwrap();
    assert!(!b[open + 8..script].contains("```"), "{b}");
    assert!(b[script..].contains("\n```"), "{b}");
    assert!(b.contains("contact request #1"));
    assert_eq!(
        rows(&pool).await,
        [(
            "remove_listings".to_string(),
            "example-venue.org".to_string(),
            Some("owner@example-venue.org".to_string()),
            Some(77),
            "filed".to_string()
        )]
    );

    // Same domain and type within 7 days: a comment on #77, no new issue.
    let (status, _) = post(
        &app,
        body(
            "remove_listings",
            "example-venue.org",
            "Again, please.",
            "",
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(requests_to(&server, "/issues").await.len(), 1);
    let comments = requests_to(&server, "/issues/77/comments").await;
    assert_eq!(comments.len(), 1);
    assert!(
        comments[0]["body"]
            .as_str()
            .unwrap()
            .contains("Again, please.")
    );
    assert_eq!(rows(&pool).await[1].3, Some(77));
    assert_eq!(rows(&pool).await[1].4, "commented");

    // Different type for the same domain: its own issue.
    let (status, _) = post(
        &app,
        body("correct_event", "example-venue.org", "Wrong date.", "", ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(requests_to(&server, "/issues").await.len(), 2);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn invalid_input_rerenders_and_rate_limit_applies() {
    let Some(db) = TestDb::create("contact_rate_limit").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app(&pool, None);
    let (status, html) = post(&app, body("other", "example.org", "", "", "")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        html.contains("tell us a little about the request"),
        "{html}"
    );
    assert!(html.contains("value=\"example.org\""));

    for i in 0..contact::PER_HOUR {
        let (status, _) = post(&app, body("other", &format!("site{i}.org"), "hi", "", "")).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, html) = post(&app, body("other", "site9.org", "hi", "", "")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{html}");
    assert_eq!(rows(&pool).await.len(), contact::PER_HOUR as usize);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn github_down_leaves_pending_and_the_ingest_run_files_it() {
    let Some(db) = TestDb::create("contact_github_down").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let down = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&down)
        .await;
    let (status, html) = post(
        &app(&pool, filer(&down)),
        body("other", "https://example.org", "Test", "", ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("got it"));
    // No filer at all behaves the same.
    let (status, _) = post(
        &app(&pool, None),
        body("correct_event", "https://example.org", "Test 2", "", ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let r = rows(&pool).await;
    assert!(
        r.iter().all(|r| r.4 == "pending_issue" && r.3.is_none()),
        "{r:?}"
    );

    // Too recent for the ingest run (grace period)...
    let up = MockServer::start().await;
    mock_create(&up, 90).await;
    let gh = GitHubIssueFiler::new(&up.uri(), REPO, "token").unwrap();
    assert_eq!(
        contact::file_pending(&pool, &gh, Utc::now()).await.unwrap(),
        0
    );
    // ...then filed.
    let later = Utc::now() + contact::RETRY_GRACE + Duration::minutes(1);
    assert_eq!(contact::file_pending(&pool, &gh, later).await.unwrap(), 2);
    let r = rows(&pool).await;
    assert!(r.iter().all(|r| r.4 == "filed" && r.3 == Some(90)), "{r:?}");
    assert_eq!(contact::file_pending(&pool, &gh, later).await.unwrap(), 0);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn oversized_bodies_are_rejected() {
    let Some(db) = TestDb::create("contact_body_limit").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let huge = body("other", "example.org", &"x".repeat(64 * 1024), "", "");
    let (status, _) = post(&app(&pool, None), huge).await;
    assert!(
        status == StatusCode::PAYLOAD_TOO_LARGE || status == StatusCode::BAD_REQUEST,
        "{status}"
    );
    assert!(rows(&pool).await.is_empty());
    pool.close().await;
    db.drop_db().await;
}
