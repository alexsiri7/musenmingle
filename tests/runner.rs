//! Ingest runner with fake sources (no network).

mod common;

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use common::TestDb;
use thaleia::config::RateLimitConfig;
use thaleia::fetch::FetchContext;
use thaleia::health::{HealthAction, HealthChecker, HealthConfig};
use thaleia::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use thaleia::normalise::dedupe_key;
use thaleia::repo;
use thaleia::runner::{INGEST_LOCK_KEY, RunSummary, Runner};
use thaleia::sources::{Source, SourceError};

struct FakeSource {
    now: DateTime<Utc>,
    delay: Option<Duration>,
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
            dedupe_key: dedupe_key(&title, starts_at, Some("Fake Hall")),
            title,
            description: None,
            venue_name: Some("Fake Hall".into()),
            address: None,
            lat: Some(51.5),
            lng: Some(-0.12),
            starts_at,
            ends_at: None,
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
        ctx: FetchContext::new(RateLimitConfig::disabled()).unwrap(),
        factory: Box::new(move |row| {
            (row.key == "fake").then(|| Box::new(FakeSource { now, delay }) as Box<dyn Source>)
        }),
        health: HealthChecker::new(HealthConfig::default(), None),
        source_timeout: timeout,
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
