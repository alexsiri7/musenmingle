//! `POST /v1/suggestions` against a real database and a wiremock GitHub:
//! filing, domain dedupe (both directions), rate limiting, validation, and
//! the GitHub-down → retry-at-next-ingest-run path.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use common::TestDb;
use serde_json::{Value, json};
use sqlx::PgPool;
use thaleia::config::{RateLimitConfig, SuggestionConfig};
use thaleia::fetch::FetchContext;
use thaleia::github::{GitHubIssueFiler, IssueFiler};
use thaleia::health::{HealthChecker, HealthConfig};
use thaleia::runner::Runner;
use thaleia::suggestions::{MAX_NOTE_CHARS, RETRY_GRACE, Suggestions, ip_hash};
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO: &str = "alexsiri7/thaleia";

fn filer(server: &MockServer) -> Box<dyn IssueFiler> {
    Box::new(GitHubIssueFiler::new(&server.uri(), REPO, "test-token").unwrap())
}

fn app(pool: &PgPool, config: SuggestionConfig, filer: Option<Box<dyn IssueFiler>>) -> Router {
    let config = SuggestionConfig {
        ip_salt: Some("test-salt".into()),
        ..config
    };
    thaleia::api::router(pool.clone(), Suggestions::new(config, filer).unwrap())
        .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 1], 4000))))
}

async fn post(
    app: &Router,
    body: Value,
    forwarded_for: Option<&str>,
) -> (StatusCode, Option<u64>, Value) {
    let mut req = Request::post("/v1/suggestions").header(header::CONTENT_TYPE, "application/json");
    if let Some(xff) = forwarded_for {
        req = req.header("x-forwarded-for", xff);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().parse().unwrap());
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, retry_after, serde_json::from_slice(&bytes).unwrap())
}

async fn rows(pool: &PgPool) -> Vec<(String, String, Option<i64>)> {
    sqlx::query_as(
        "SELECT domain, status, github_issue_number FROM events.site_suggestions ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn mock_create(server: &MockServer, response: ResponseTemplate, times: u64) {
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues")))
        .respond_with(response)
        .expect(times)
        .mount(server)
        .await;
}

async fn mock_list(server: &MockServer, issues: Value) {
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO}/issues")))
        .and(query_param("labels", "new-scraper"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issues))
        .mount(server)
        .await;
}

#[tokio::test]
async fn accepted_suggestion_files_one_issue_and_dedupes_by_domain() {
    let Some(db) =
        TestDb::create("accepted_suggestion_files_one_issue_and_dedupes_by_domain").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let github = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues")))
        .and(body_partial_json(json!({
            "title": "New scraper: example-gallery.org.uk",
            "labels": ["new-scraper"],
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "number": 42, "title": "New scraper: example-gallery.org.uk"
        })))
        .expect(1)
        .mount(&github)
        .await;
    let app = app(&pool, SuggestionConfig::default(), Some(filer(&github)));

    let (status, _, body) = post(
        &app,
        json!({ "url": "https://www.example-gallery.org.uk/whats-on", "note": "tiny gallery" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body,
        json!({ "status": "accepted", "domain": "example-gallery.org.uk", "github_issue": 42 })
    );

    // Same registrable domain written differently: no second issue.
    let (status, _, body) = post(
        &app,
        json!({ "url": "http://events.EXAMPLE-gallery.org.uk" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "status": "already_suggested", "domain": "example-gallery.org.uk" })
    );

    let created = &github.received_requests().await.unwrap()[0];
    let issue: Value = serde_json::from_slice(&created.body).unwrap();
    let issue_body = issue["body"].as_str().unwrap();
    assert!(issue_body.contains("<https://www.example-gallery.org.uk/whats-on>"));
    assert!(issue_body.contains("tiny gallery"));
    assert!(issue_body.contains("### Checklist"));

    let note: Option<String> =
        sqlx::query_scalar("SELECT note FROM events.site_suggestions WHERE status = 'accepted'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(note.as_deref(), Some("tiny gallery"));
    assert_eq!(
        rows(&pool).await,
        vec![
            ("example-gallery.org.uk".into(), "accepted".into(), Some(42)),
            ("example-gallery.org.uk".into(), "duplicate".into(), None),
        ]
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn domains_we_already_scrape_are_already_covered() {
    let Some(db) = TestDb::create("domains_we_already_scrape_are_already_covered").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let github = MockServer::start().await;
    mock_create(&github, ResponseTemplate::new(500), 0).await;
    let app = app(&pool, SuggestionConfig::default(), Some(filer(&github)));

    // Seeded as `www.serpentinegalleries.org` and `app.ticketmaster.com`.
    for (url, source) in [
        (
            "http://serpentinegalleries.org/whats-on/",
            "serpentine-galleries",
        ),
        ("https://www.ticketmaster.com/", "ticketmaster"),
        ("https://designmuseum.org/exhibitions", "design-museum"),
    ] {
        let (status, _, body) = post(&app, json!({ "url": url }), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{url}: {body}");
        assert_eq!(
            body,
            json!({ "status": "already_covered", "source": source })
        );
    }
    let statuses: Vec<String> = rows(&pool).await.into_iter().map(|r| r.1).collect();
    assert_eq!(statuses, ["duplicate"; 3]);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn rate_limit_per_client_hour_and_day() {
    let Some(db) = TestDb::create("rate_limit_per_client_hour_and_day").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let config = SuggestionConfig {
        per_hour: 2,
        per_day: 3,
        trusted_proxies: 1,
        ..Default::default()
    };
    let app = app(&pool, config, None);
    let alice = Some("6.6.6.6, 1.1.1.1");
    let bob = Some("1.1.1.1, 2.2.2.2");

    let (status, _, body) = post(&app, json!({ "url": "https://site1.org" }), alice).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // A duplicate is stored and counts toward the limit too.
    let (status, _, body) = post(&app, json!({ "url": "https://www.site1.org" }), alice).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "already_suggested");
    let (status, retry_after, body) =
        post(&app, json!({ "url": "https://site2.org" }), alice).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["status"], "rate_limited");
    let retry_after = retry_after.expect("Retry-After header");
    assert!((3500..=3600).contains(&retry_after), "{retry_after}");

    // Another client behind the same proxy is unaffected.
    let (status, _, _) = post(&app, json!({ "url": "https://bob.org" }), bob).await;
    assert_eq!(status, StatusCode::CREATED);

    // Two hours later the hourly window is clear, but the daily one is not.
    sqlx::query("UPDATE events.site_suggestions SET created_at = created_at - interval '2 hours'")
        .execute(&pool)
        .await
        .unwrap();
    let (status, _, _) = post(&app, json!({ "url": "https://site3.org" }), alice).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, retry_after, _) = post(&app, json!({ "url": "https://site4.org" }), alice).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let retry_after = retry_after.unwrap();
    assert!(
        (21 * 3600..=22 * 3600).contains(&retry_after),
        "{retry_after}"
    );

    // Rejected-by-rate-limit requests are not stored.
    assert_eq!(rows(&pool).await.len(), 4);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn invalid_submissions_are_rejected_and_not_stored() {
    let Some(db) = TestDb::create("invalid_submissions_are_rejected_and_not_stored").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app(&pool, SuggestionConfig::default(), None);

    for body in [
        json!({ "url": "ftp://example.org/" }),
        json!({ "url": "http://localhost:3000/" }),
        json!({ "url": "http://192.168.1.10/events" }),
        json!({ "url": "http://[fd00::1]/" }),
        json!({ "url": "https://example.org", "note": "x".repeat(501) }),
        json!({ "url": format!("https://example.org/{}", "a".repeat(2048)) }),
        json!({ "note": "no url" }),
    ] {
        let (status, _, resp) = post(&app, body.clone(), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(resp["status"], "invalid", "{body}");
        assert!(resp["error"].is_string());
    }
    assert!(rows(&pool).await.is_empty());
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn note_at_the_length_limit_is_stored() {
    let Some(db) = TestDb::create("note_at_the_length_limit_is_stored").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app(&pool, SuggestionConfig::default(), None);
    let note = "é".repeat(MAX_NOTE_CHARS);

    let (status, _, body) = post(
        &app,
        json!({ "url": "https://example.org", "note": note }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let stored: Option<String> = sqlx::query_scalar("SELECT note FROM events.site_suggestions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, Some(note));
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn submissions_from_one_client_wait_for_its_lock() {
    let Some(db) = TestDb::create("submissions_from_one_client_wait_for_its_lock").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app(&pool, SuggestionConfig::default(), None);
    let mut holder = pool.begin().await.unwrap();
    let client = ip_hash([10, 0, 0, 1].into(), "test-salt");
    thaleia::repo::lock_suggestion_submitter(&mut holder, &client)
        .await
        .unwrap();

    let mut pending = tokio::spawn({
        let app = app.clone();
        async move { post(&app, json!({ "url": "https://example.org" }), None).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(500), &mut pending)
            .await
            .is_err(),
        "submission did not wait for the client's lock"
    );
    holder.commit().await.unwrap();
    let (status, _, body) = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::CREATED, "{body}");
    pool.close().await;
    db.drop_db().await;
}

fn runner(pool: &PgPool, filer: Box<dyn IssueFiler>) -> Runner {
    Runner {
        pool: pool.clone(),
        ctx: FetchContext::new(RateLimitConfig::disabled()).unwrap(),
        factory: Box::new(|_| None),
        health: HealthChecker::new(HealthConfig::default(), Some(filer)),
        source_timeout: Duration::from_secs(1),
    }
}

#[tokio::test]
async fn github_down_leaves_pending_and_next_ingest_run_files_it() {
    let Some(db) = TestDb::create("github_down_leaves_pending_and_next_ingest_run_files_it").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;

    let down = MockServer::start().await;
    mock_create(&down, ResponseTemplate::new(502), 1).await;
    let app = app(&pool, SuggestionConfig::default(), Some(filer(&down)));
    let (status, _, body) = post(
        &app,
        json!({ "url": "https://newplace.london/events" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body,
        json!({ "status": "accepted", "domain": "newplace.london", "github_issue": null })
    );
    assert_eq!(
        rows(&pool).await,
        vec![("newplace.london".into(), "pending".into(), None)]
    );

    // A pending row still counts as suggested.
    let (status, _, _) = post(&app, json!({ "url": "http://www.newplace.london" }), None).await;
    assert_eq!(status, StatusCode::OK);

    let up = MockServer::start().await;
    mock_list(&up, json!([])).await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues")))
        .and(body_partial_json(json!({
            "title": "New scraper: newplace.london",
            "labels": ["new-scraper"],
        })))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({ "number": 7, "title": "New scraper: newplace.london" })),
        )
        .expect(1)
        .mount(&up)
        .await;

    // Within the grace period the row is left to the API request.
    runner(&pool, filer(&up))
        .run_once(Utc::now())
        .await
        .unwrap();
    assert_eq!(rows(&pool).await[0].1, "pending");

    let later = Utc::now() + RETRY_GRACE + chrono::Duration::minutes(1);
    runner(&pool, filer(&up)).run_once(later).await.unwrap();
    assert_eq!(
        rows(&pool).await[0],
        ("newplace.london".into(), "accepted".into(), Some(7))
    );
    // Nothing left to file.
    runner(&pool, filer(&up)).run_once(later).await.unwrap();
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn retry_adopts_an_issue_filed_before_a_crash() {
    let Some(db) = TestDb::create("retry_adopts_an_issue_filed_before_a_crash").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app(&pool, SuggestionConfig::default(), None);
    let (status, _, _) = post(&app, json!({ "url": "https://newplace.london" }), None).await;
    assert_eq!(status, StatusCode::CREATED);

    let github = MockServer::start().await;
    mock_list(
        &github,
        json!([
            { "number": 3, "title": "New scraper: other.org" },
            { "number": 9, "title": "New scraper: newplace.london" },
        ]),
    )
    .await;
    mock_create(&github, ResponseTemplate::new(500), 0).await;

    let later = Utc::now() + RETRY_GRACE + chrono::Duration::minutes(1);
    runner(&pool, filer(&github)).run_once(later).await.unwrap();
    assert_eq!(
        rows(&pool).await,
        vec![("newplace.london".into(), "accepted".into(), Some(9))]
    );
    pool.close().await;
    db.drop_db().await;
}
