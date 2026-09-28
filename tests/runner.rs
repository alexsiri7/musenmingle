//! Ingest runner with fake sources (no network).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use common::TestDb;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::health::{HealthAction, HealthChecker, HealthConfig};
use musenmingle::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use musenmingle::normalise::dedupe_key;
use musenmingle::repo;
use musenmingle::runner::{INGEST_LOCK_KEY, RunSummary, Runner};
use musenmingle::sources::{SkipReason, Source, SourceError};

struct FakeSource {
    now: DateTime<Utc>,
    delay: Option<Duration>,
    fetch_timeout: Option<Duration>,
}

fn raw(id: &str, days_from_now: i64) -> RawEvent {
    RawEvent {
        source_event_id: id.into(),
        source_url: Some(format!("https://fake.test/{id}")),
        payload: serde_json::json!({ "id": id, "days": days_from_now }),
    }
}

#[async_trait]
impl Source for FakeSource {
    fn key(&self) -> &str {
        "fake"
    }

    fn fetch_timeout(&self) -> Option<Duration> {
        self.fetch_timeout
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        ctx.report_error("one detail page failed");
        Ok(vec![
            raw("upcoming-1", 3),
            raw("upcoming-2", 10),
            raw("past", -30),
            raw("irrelevant", 5),
            raw("broken", 5),
        ])
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        match raw.source_event_id.as_str() {
            "irrelevant" => return Ok(None),
            "broken" => return Err(SourceError::Parse("no date".into())),
            _ => {}
        }
        let days = raw.payload["days"].as_i64().unwrap();
        let starts_at = self.now + chrono::Duration::days(days);
        let title = format!("Fake {}", raw.source_event_id);
        Ok(Some(NewEvent {
            sessions: Vec::new(),
            dedupe_key: dedupe_key(&title, starts_at, Some("Fake Hall")),
            title,
            description: None,
            venue_name: Some("Fake Hall".into()),
            address: None,
            lat: Some(51.5),
            lng: Some(-0.12),
            starts_at,
            ends_at: None,
            all_day: false,
            price: Price::default(),
            url: raw.source_url.clone(),
            image_url: None,
            category: Category::Community,
            tags: vec![],
        }))
    }
}

fn runner(
    pool: sqlx::PgPool,
    now: DateTime<Utc>,
    delay: Option<Duration>,
    timeout: Duration,
) -> Runner {
    Runner {
        pool,
        ctx: FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap(),
        factory: Box::new(move |row| {
            if row.key == "fake" {
                Ok(Box::new(FakeSource {
                    now,
                    delay,
                    fetch_timeout: None,
                }))
            } else {
                Err(SkipReason::UnknownKey)
            }
        }),
        health: HealthChecker::new(HealthConfig::default(), None),
        source_timeout: timeout,
        enrich: None,
        qa: None,
        venues: None,
        form_issues: Default::default(),
    }
}

#[tokio::test]
async fn runs_due_sources_records_runs_and_respects_intervals() {
    let Some(db) = TestDb::create("runs_due_sources_records_runs_and_respects_intervals").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    // Only our fake source is enabled for this test.
    sqlx::query("UPDATE events.sources SET enabled = false")
        .execute(&pool)
        .await
        .unwrap();
    let src = repo::upsert_source(
        &pool,
        "fake",
        SourceKind::Scraper,
        "https://fake.test",
        60,
        true,
    )
    .await
    .unwrap();

    let now = Utc::now();
    let r = runner(pool.clone(), now, None, Duration::from_secs(5));
    let RunSummary::Ran(reports) = r.run_once(now).await.unwrap() else {
        panic!("expected a run");
    };
    assert_eq!(reports.len(), 1);
    let rep = &reports[0];
    assert_eq!(rep.key, "fake");
    assert!(rep.ok);
    assert_eq!(rep.events_found, 2);
    assert_eq!(rep.created, 2);
    assert_eq!(rep.skipped, 2, "past + irrelevant are skipped, not errors");
    assert_eq!(
        rep.errors, 2,
        "one normalise error + one reported soft error"
    );
    assert_eq!(rep.health, Some(HealthAction::Degraded));

    let runs = repo::recent_runs(&pool, src.id, 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].events_found, runs[0].errors, runs[0].ok),
        (2, 2, true)
    );
    let summary = runs[0].error_summary.as_deref().unwrap();
    assert!(summary.contains("normalise broken") && summary.contains("one detail page failed"));
    let src_after = repo::source_by_key(&pool, "fake").await.unwrap().unwrap();
    assert!(src_after.last_run_at.is_some());

    // Interval (60 min) not elapsed: nothing runs.
    let RunSummary::Ran(again) = r
        .run_once(now + chrono::Duration::minutes(30))
        .await
        .unwrap()
    else {
        panic!("expected a run");
    };
    assert!(again.is_empty());
    // Elapsed: runs again; same events are updated, not duplicated.
    let RunSummary::Ran(later) = r
        .run_once(now + chrono::Duration::minutes(61))
        .await
        .unwrap()
    else {
        panic!("expected a run");
    };
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].created, 0);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM events.events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2);

    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn timeouts_are_recorded_as_failed_runs() {
    let Some(db) = TestDb::create("timeouts_are_recorded_as_failed_runs").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    sqlx::query("UPDATE events.sources SET enabled = false")
        .execute(&pool)
        .await
        .unwrap();
    let src = repo::upsert_source(
        &pool,
        "fake",
        SourceKind::Scraper,
        "https://fake.test",
        60,
        true,
    )
    .await
    .unwrap();
    let now = Utc::now();
    let r = runner(
        pool.clone(),
        now,
        Some(Duration::from_secs(5)),
        Duration::from_millis(100),
    );
    let RunSummary::Ran(reports) = r.run_once(now).await.unwrap() else {
        panic!("expected a run");
    };
    assert!(!reports[0].ok);
    assert_eq!(reports[0].errors, 1);
    let runs = repo::recent_runs(&pool, src.id, 1).await.unwrap();
    assert!(!runs[0].ok);
    assert!(
        runs[0]
            .error_summary
            .as_deref()
            .unwrap()
            .contains("timed out")
    );

    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn a_source_can_ask_for_a_longer_fetch_timeout() {
    let Some(db) = TestDb::create("a_source_can_ask_for_a_longer_fetch_timeout").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    sqlx::query("UPDATE events.sources SET enabled = false")
        .execute(&pool)
        .await
        .unwrap();
    repo::upsert_source(
        &pool,
        "fake",
        SourceKind::Scraper,
        "https://fake.test",
        60,
        true,
    )
    .await
    .unwrap();
    let now = Utc::now();
    let r = Runner {
        factory: Box::new(move |_| {
            Ok(Box::new(FakeSource {
                now,
                delay: Some(Duration::from_millis(300)),
                fetch_timeout: Some(Duration::from_secs(5)),
            }))
        }),
        ..runner(pool.clone(), now, None, Duration::from_millis(100))
    };
    let RunSummary::Ran(reports) = r.run_once(now).await.unwrap() else {
        panic!("expected a run");
    };
    assert!(reports[0].ok);

    pool.close().await;
    db.drop_db().await;
}

async fn skip_state(pool: &sqlx::PgPool) -> (Option<String>, Option<DateTime<Utc>>) {
    sqlx::query_as("SELECT skip_reason, skipped_at FROM events.sources WHERE key = 'fake'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn unbuildable_source_is_recorded_as_skipped_then_cleared_by_a_run() {
    let Some(db) =
        TestDb::create("unbuildable_source_is_recorded_as_skipped_then_cleared_by_a_run").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    sqlx::query("UPDATE events.sources SET enabled = false")
        .execute(&pool)
        .await
        .unwrap();
    let src = repo::upsert_source(
        &pool,
        "fake",
        SourceKind::Scraper,
        "https://fake.test",
        60,
        true,
    )
    .await
    .unwrap();
    let now: DateTime<Utc> = "2026-09-26T06:00:00Z".parse().unwrap();
    let configured = Arc::new(AtomicBool::new(false));
    let factory_configured = configured.clone();
    let r = Runner {
        pool: pool.clone(),
        ctx: FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap(),
        factory: Box::new(move |_| {
            if factory_configured.load(Ordering::SeqCst) {
                Ok(Box::new(FakeSource {
                    now,
                    delay: None,
                    fetch_timeout: None,
                }))
            } else {
                Err(SkipReason::MissingConfig("FAKE_API_KEY"))
            }
        }),
        health: HealthChecker::new(HealthConfig::default(), None),
        source_timeout: Duration::from_secs(5),
        enrich: None,
        qa: None,
        venues: None,
        form_issues: Default::default(),
    };

    let RunSummary::Ran(reports) = r.run_once(now).await.unwrap() else {
        panic!("expected a run");
    };
    assert!(reports.is_empty());
    assert!(
        repo::recent_runs(&pool, src.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let after_skip = repo::source_by_key(&pool, "fake").await.unwrap().unwrap();
    assert_eq!(after_skip.last_run_at, None);
    assert_eq!(
        skip_state(&pool).await,
        (Some("FAKE_API_KEY not set".into()), Some(now))
    );
    let issues: i64 = sqlx::query_scalar("SELECT count(*) FROM events.health_issues")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(issues, 0, "a skip never reaches the health checker");

    // Still due on the next tick, well inside the 60-minute interval.
    let next_tick = now + chrono::Duration::minutes(1);
    let RunSummary::Ran(reports) = r.run_once(next_tick).await.unwrap() else {
        panic!("expected a run");
    };
    assert!(reports.is_empty());
    assert_eq!(skip_state(&pool).await.1, Some(next_tick));

    configured.store(true, Ordering::SeqCst);
    let RunSummary::Ran(reports) = r
        .run_once(now + chrono::Duration::minutes(2))
        .await
        .unwrap()
    else {
        panic!("expected a run");
    };
    assert_eq!(reports.len(), 1);
    assert_eq!(repo::recent_runs(&pool, src.id, 10).await.unwrap().len(), 1);
    let after_run = repo::source_by_key(&pool, "fake").await.unwrap().unwrap();
    assert!(after_run.last_run_at.is_some());
    assert_eq!(skip_state(&pool).await, (None, None));

    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn overlapping_runs_are_prevented_by_advisory_lock() {
    let Some(db) = TestDb::create("overlapping_runs_are_prevented_by_advisory_lock").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let mut holder = pool.acquire().await.unwrap();
    let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(INGEST_LOCK_KEY)
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    assert!(got);
    let r = runner(pool.clone(), Utc::now(), None, Duration::from_secs(1));
    assert_eq!(r.run_once(Utc::now()).await.unwrap(), RunSummary::Locked);
    drop(holder);
    drop(r);
    pool.close().await;
    db.drop_db().await;
}

/// Emits an upcoming and a past event, both ending before they start (the
/// database refuses the upcoming one; the past one is dropped).
struct BackwardsSource {
    now: DateTime<Utc>,
}

#[async_trait]
impl Source for BackwardsSource {
    fn key(&self) -> &str {
        "fake"
    }

    async fn fetch(&self, _: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        Ok(vec![raw("upcoming", 3), raw("past", -30)])
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        let fake = FakeSource {
            now: self.now,
            delay: None,
            fetch_timeout: None,
        };
        let mut ev = fake.normalise(raw)?.unwrap();
        ev.ends_at = Some(ev.starts_at - chrono::Duration::hours(1));
        Ok(Some(ev))
    }
}

#[tokio::test]
async fn qa_rules_record_findings_and_counts_for_the_run() {
    let Some(db) = TestDb::create("qa_rules_record_findings_and_counts_for_the_run").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    sqlx::query("UPDATE events.sources SET enabled = false")
        .execute(&pool)
        .await
        .unwrap();
    let src = repo::upsert_source(
        &pool,
        "fake",
        SourceKind::Scraper,
        "https://fake.test",
        60,
        true,
    )
    .await
    .unwrap();
    let now = Utc::now();
    let r = Runner {
        factory: Box::new(move |_| Ok(Box::new(BackwardsSource { now }))),
        ..runner(pool.clone(), now, None, Duration::from_secs(5))
    };
    let RunSummary::Ran(reports) = r.run_once(now).await.unwrap() else {
        panic!("expected a run");
    };
    assert_eq!(reports[0].qa_findings, 1);
    assert_eq!(reports[0].qa_check, None, "no AI check without a QaChecker");

    let run = &repo::recent_runs(&pool, src.id, 1).await.unwrap()[0];
    let rows: Vec<(String, i32, serde_json::Value)> =
        sqlx::query_as("SELECT rule, affected, examples FROM events.qa_findings WHERE run_id = $1")
            .bind(run.id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1),
        ("end_before_start", 2),
        "events that were not stored count too"
    );
    assert_eq!(rows[0].2[1]["source_event_id"], "past");
    let counts: (Option<i32>, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT events_checked, missing_venue, missing_coords FROM events.source_runs WHERE id = $1",
    )
    .bind(run.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (Some(0), Some(0), Some(0)), "neither was stored");

    pool.close().await;
    db.drop_db().await;
}
