//! Health checker + GitHub issue filer against wiremock: create, dedupe
//! (DB and GitHub-side), and close on recovery.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use serde_json::json;
use sqlx::PgPool;
use thaleia::github::{GitHubIssueFiler, IssueFiler};
use thaleia::health::{HealthAction, HealthChecker, HealthConfig};
use thaleia::repo::{self, NewRun, SourceRow};
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO: &str = "alexsiri7/thaleia";
const TITLE: &str = "Scraper broken: serpentine-galleries";

async fn add_run(
    pool: &PgPool,
    src: &SourceRow,
    minutes_ago: i64,
    events: i32,
    errors: i32,
    ok: bool,
) {
    let started_at = Utc::now() - Duration::minutes(minutes_ago);
    repo::record_run(
        pool,
        &NewRun {
            source_id: src.id,
            started_at,
            finished_at: started_at + Duration::seconds(3),
            events_found: events,
            errors,
            error_summary: (errors > 0).then(|| "fetch failed: HTTP 500 | boom".to_string()),
            ok,
        },
    )
    .await
    .unwrap();
}

fn checker(server: &MockServer) -> HealthChecker {
    let filer: Box<dyn IssueFiler> =
        Box::new(GitHubIssueFiler::new(&server.uri(), REPO, "test-token").unwrap());
    HealthChecker::new(HealthConfig::default(), Some(filer))
}

async fn mock_list(server: &MockServer, issues: serde_json::Value, times: u64) {
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO}/issues")))
        .and(query_param("state", "open"))
        .and(query_param("labels", "scraper-broken"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issues))
        .expect(times)
        .mount(server)
        .await;
}

#[tokio::test]
async fn opens_one_issue_dedupes_and_closes_on_recovery() {
    let Some(db) = TestDb::create("opens_one_issue_dedupes_and_closes_on_recovery").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::source_by_key(&pool, "serpentine-galleries")
        .await
        .unwrap()
        .unwrap();

    // History of healthy runs, then a run with zero events.
    for (i, n) in [12, 10, 11].iter().enumerate() {
        add_run(&pool, &src, 100 - i as i64, *n, 0, true).await;
    }
    add_run(&pool, &src, 10, 0, 0, true).await;

    // Phase 1: trip -> list (nothing relevant open; a PR with the same title
    // must be ignored) -> create.
    let gh = MockServer::start().await;
    mock_list(
        &gh,
        json!([
            {"number": 7, "title": "Scraper broken: ticketmaster"},
            {"number": 8, "title": TITLE, "pull_request": {"url": "x"}}
        ]),
        1,
    )
    .await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues")))
        .and(body_partial_json(
            json!({"title": TITLE, "labels": ["scraper-broken"]}),
        ))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"number": 42, "title": TITLE})),
        )
        .expect(1)
        .mount(&gh)
        .await;
    let hc = checker(&gh);
    assert_eq!(
        hc.check_source(&pool, &src).await.unwrap(),
        HealthAction::Opened(42)
    );
    let open = repo::open_health_issue(&pool, src.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(open.github_issue_number, 42);
    assert!(open.reason.contains("found 0 events"), "{}", open.reason);

    // The issue body carries the reason, a run table and the source link.
    let reqs = gh.received_requests().await.unwrap();
    let create = reqs.iter().find(|r| r.method.as_str() == "POST").unwrap();
    let body: serde_json::Value = serde_json::from_slice(&create.body).unwrap();
    let text = body["body"].as_str().unwrap();
    assert!(text.contains("found 0 events"));
    assert!(text.contains("| started (UTC) | ok | events | errors |"));
    assert!(text.contains("https://www.serpentinegalleries.org"));

    // Phase 2: still broken -> no new issue (DB dedupe; POST expect(1) above).
    add_run(&pool, &src, 5, 0, 0, true).await;
    assert_eq!(
        hc.check_source(&pool, &src).await.unwrap(),
        HealthAction::AlreadyOpen(42)
    );
    drop(hc);
    gh.verify().await;

    // Phase 3: DB lost its record (e.g. reset) -> GitHub search finds the
    // open issue by label + title and adopts it instead of creating one.
    sqlx::query("DELETE FROM events.health_issues")
        .execute(&pool)
        .await
        .unwrap();
    let gh2 = MockServer::start().await;
    mock_list(&gh2, json!([{"number": 42, "title": TITLE}]), 1).await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&gh2)
        .await;
    let hc2 = checker(&gh2);
    assert_eq!(
        hc2.check_source(&pool, &src).await.unwrap(),
        HealthAction::Adopted(42)
    );
    assert!(
        repo::open_health_issue(&pool, src.id)
            .await
            .unwrap()
            .is_some()
    );
    gh2.verify().await;

    // Phase 4: recovery -> comment + close, DB row closed.
    add_run(&pool, &src, 1, 11, 0, true).await;
    let gh3 = MockServer::start().await;
    mock_list(&gh3, json!([{"number": 42, "title": TITLE}]), 1).await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues/42/comments")))
        .and(body_partial_json(json!({})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
        .expect(1)
        .mount(&gh3)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("/repos/{REPO}/issues/42")))
        .and(body_partial_json(json!({"state": "closed"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"number": 42, "title": TITLE})),
        )
        .expect(1)
        .mount(&gh3)
        .await;
    let hc3 = checker(&gh3);
    assert_eq!(
        hc3.check_source(&pool, &src).await.unwrap(),
        HealthAction::Closed(vec![42])
    );
    assert!(
        repo::open_health_issue(&pool, src.id)
            .await
            .unwrap()
            .is_none()
    );
    // Healthy again: nothing more to do.
    assert_eq!(
        hc3.check_source(&pool, &src).await.unwrap(),
        HealthAction::Healthy
    );
    gh3.verify().await;

    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn consecutive_errors_trip_and_single_error_does_not() {
    let Some(db) = TestDb::create("consecutive_errors_trip_and_single_error_does_not").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::source_by_key(&pool, "ticketmaster")
        .await
        .unwrap()
        .unwrap();
    let hc = HealthChecker::new(HealthConfig::default(), None);

    add_run(&pool, &src, 30, 10, 0, true).await;
    add_run(&pool, &src, 20, 0, 1, false).await;
    // One failed run after a good one: zero-events rule trips (avg 10 > 0).
    assert_eq!(
        hc.check_source(&pool, &src).await.unwrap(),
        HealthAction::NoFiler
    );
    let db2 = repo::open_health_issue(&pool, src.id).await.unwrap();
    assert!(db2.is_none(), "without a filer nothing is recorded");

    // A run with a partial error but a normal count: that is the second
    // consecutive run with errors, so the consecutive-errors rule trips.
    add_run(&pool, &src, 10, 10, 1, true).await;
    assert_eq!(
        hc.check_source(&pool, &src).await.unwrap(),
        HealthAction::NoFiler
    );
    // Clean run: healthy.
    add_run(&pool, &src, 5, 10, 0, true).await;
    assert_eq!(
        hc.check_source(&pool, &src).await.unwrap(),
        HealthAction::Healthy
    );
    // A single run with an error after a clean one: degraded, below threshold.
    add_run(&pool, &src, 1, 10, 1, true).await;
    assert_eq!(
        hc.check_source(&pool, &src).await.unwrap(),
        HealthAction::Degraded
    );

    pool.close().await;
    db.drop_db().await;
}
