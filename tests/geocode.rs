//! Venue locations (#204): venues without coordinates are geocoded from
//! their postcode on postcodes.io (live, then terminated postcodes), their
//! events pick the point up, and a venue with upcoming events that still
//! has none raises one owner alert a day until it clears.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use common::TestDb;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::geocode::{VenueChecks, postcode_point};
use musenmingle::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use musenmingle::notify::Notifier;
use musenmingle::repo;
use sqlx::PgPool;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn found(lat: f64, lng: f64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "status": 200,
        "result": {"postcode": "X", "latitude": lat, "longitude": lng}
    }))
}

async fn postcodes_io() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/postcodes/SE58UH"))
        .respond_with(found(51.474002, -0.081087))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/terminated_postcodes/N19ZZ"))
        .respond_with(found(51.53, -0.12))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/postcodes/E10XX"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    // Everything else (robots.txt included): 404.
    server
}

#[tokio::test]
async fn postcodes_resolve_live_then_terminated() {
    let server = postcodes_io().await;
    let base = Url::parse(&server.uri()).unwrap();
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    assert_eq!(
        postcode_point(&ctx, &base, "SE5 8UH").await.unwrap(),
        Some((51.474002, -0.081087))
    );
    assert_eq!(
        postcode_point(&ctx, &base, "N1 9ZZ").await.unwrap(),
        Some((51.53, -0.12))
    );
    assert_eq!(postcode_point(&ctx, &base, "W1 1AA").await.unwrap(), None);
    assert!(postcode_point(&ctx, &base, "E1 0XX").await.is_err());
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<(String, String)>>>);

#[async_trait]
impl Notifier for Recorder {
    async fn notify(&self, title: &str, body: &str, _priority: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().push((title.into(), body.into()));
        Ok(())
    }
}

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn event(title: &str, venue: &str, address: &str) -> NewEvent {
    NewEvent {
        title: title.into(),
        description: None,
        venue_name: Some(venue.into()),
        address: Some(address.into()),
        lat: None,
        lng: None,
        starts_at: t("2026-10-10T18:00:00Z"),
        ends_at: None,
        all_day: false,
        price: Price::default(),
        url: None,
        image_url: None,
        category: Category::Talk,
        tags: Vec::new(),
        dedupe_key: title.into(),
    }
}

async fn ingest(pool: &PgPool, source: i64, e: &NewEvent) {
    let raw = RawEvent {
        source_event_id: e.title.clone(),
        source_url: None,
        payload: serde_json::json!({}),
    };
    repo::upsert_event(pool, source, e, &raw).await.unwrap();
}

#[tokio::test]
async fn venues_are_geocoded_and_missing_ones_alert() {
    let Some(db) = TestDb::create("venues_are_geocoded_and_missing_ones_alert").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let server = postcodes_io().await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let recorder = Recorder::default();
    let checks = VenueChecks {
        postcodes_base: Url::parse(&server.uri()).unwrap(),
        notifier: Box::new(recorder.clone()),
    };
    let src = repo::upsert_source(
        &pool,
        "test",
        SourceKind::Scraper,
        "https://example.org",
        60,
        true,
    )
    .await
    .unwrap()
    .id;
    ingest(
        &pool,
        src,
        &event("a", "Peckham Hall", "65 Peckham Road, London SE5 8UH"),
    )
    .await;
    ingest(
        &pool,
        src,
        &event("b", "Gone Rooms", "1 Old Street, London N1 9ZZ"),
    )
    .await;
    ingest(
        &pool,
        src,
        &event("c", "Nobody Knows", "2 Lost Lane, London W1 1AA"),
    )
    .await;
    ingest(
        &pool,
        src,
        &event("d", "Flaky", "3 Down Road, London E1 0XX"),
    )
    .await;
    repo::sync_venues(&pool).await.unwrap();

    let now = t("2026-10-01T09:00:00Z");
    let r = checks.run(&pool, &ctx, now).await.unwrap();
    assert_eq!((r.geocoded, r.not_found, r.failed), (2, 1, 1), "{r:?}");
    assert_eq!(r.missing, ["Flaky", "Nobody Knows"]);

    let (lat, borough, source): (Option<f64>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT lat, borough, coords_source FROM events.venues WHERE name = 'Peckham Hall'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(lat, Some(51.474002));
    assert_eq!(borough.as_deref(), Some("southwark"));
    assert_eq!(source.as_deref(), Some("postcodes.io SE5 8UH 2026-10-01"));
    // The events picked the point up.
    let located: Vec<(String, bool)> =
        sqlx::query_as("SELECT title, lat IS NOT NULL FROM events.events ORDER BY title")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        located,
        [
            ("a".into(), true),
            ("b".into(), true),
            ("c".into(), false),
            ("d".into(), false)
        ]
    );

    // One alert a day; the unknown postcode is not retried within a week,
    // the failed lookup is.
    let sent = recorder.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].1.starts_with("2 venue(s)"), "{sent:?}");
    assert!(sent[0].1.contains("Flaky, Nobody Knows"), "{sent:?}");
    let r = checks
        .run(&pool, &ctx, now + Duration::hours(2))
        .await
        .unwrap();
    assert_eq!((r.not_found, r.failed), (0, 1));
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    checks
        .run(&pool, &ctx, now + Duration::days(1))
        .await
        .unwrap();
    assert_eq!(recorder.0.lock().unwrap().len(), 2);

    // Fixed by hand (a migration): the alert clears with one notice.
    sqlx::query(
        "UPDATE events.venues SET lat = 51.5, lng = -0.1, coords_source = 'test'
         WHERE lat IS NULL",
    )
    .execute(&pool)
    .await
    .unwrap();
    let r = checks
        .run(&pool, &ctx, now + Duration::days(1))
        .await
        .unwrap();
    assert!(r.missing.is_empty());
    let sent = recorder.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 3);
    assert!(sent[2].0.contains("every venue"), "{sent:?}");
    checks
        .run(&pool, &ctx, now + Duration::days(2))
        .await
        .unwrap();
    assert_eq!(recorder.0.lock().unwrap().len(), 3);
}
