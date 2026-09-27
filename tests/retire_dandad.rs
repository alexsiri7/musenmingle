//! The D&AD retirement migration (#101) removes the events only D&AD listed
//! and unlinks it from events another source also lists.

mod common;

use common::TestDb;
use musenmingle::db;
use sqlx::PgPool;
use uuid::Uuid;

/// The last migration before `20260927980001_retire_dandad.sql`.
const BEFORE_RETIREMENT: i64 = 20260927970001;

async fn insert_event(pool: &PgPool, title: &str, url: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events (title, starts_at, category, dedupe_key, url)
         VALUES ($1, now() + interval '7 days', 'talk', $1, $2)
         RETURNING id",
    )
    .bind(title)
    .bind(url)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn link(pool: &PgPool, event: Uuid, source: &str, url: &str) {
    sqlx::query(
        "INSERT INTO events.event_sources (event_id, source_id, source_event_id, source_url, raw)
         SELECT $1, id, $3, $3, '{}' FROM events.sources WHERE key = $2",
    )
    .bind(event)
    .bind(source)
    .bind(url)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn retirement_deletes_dandad_only_events_and_unlinks_shared_ones() {
    let Some(tdb) = TestDb::create("retire_dandad").await else {
        return;
    };
    let pool = db::connect(&tdb.url()).await.unwrap();
    db::ensure_schema(&pool).await.unwrap();
    db::migrator()
        .run_to(BEFORE_RETIREMENT, &pool)
        .await
        .unwrap();

    let dandad_url = "https://www.dandad.org/events/only/";
    let only = insert_event(&pool, "D&AD only", dandad_url).await;
    link(&pool, only, "dandad", dandad_url).await;

    let shared_dandad_url = "https://www.dandad.org/events/shared/";
    let barbican_url = "https://www.barbican.org.uk/shared";
    let shared = insert_event(&pool, "Shared", shared_dandad_url).await;
    link(&pool, shared, "dandad", shared_dandad_url).await;
    link(&pool, shared, "barbican", barbican_url).await;

    let other = insert_event(&pool, "Barbican only", barbican_url).await;
    link(
        &pool,
        other,
        "barbican",
        "https://www.barbican.org.uk/other",
    )
    .await;

    db::migrate(&pool).await.unwrap();

    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM events.events ORDER BY title")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids, vec![other, shared]);
    let shared_url: Option<String> =
        sqlx::query_scalar("SELECT url FROM events.events WHERE id = $1")
            .bind(shared)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(shared_url.as_deref(), Some(barbican_url));
    let other_url: Option<String> =
        sqlx::query_scalar("SELECT url FROM events.events WHERE id = $1")
            .bind(other)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(other_url.as_deref(), Some(barbican_url));

    let dandad_links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
          WHERE s.key = 'dandad'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(dandad_links, 0);
    let enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM events.sources WHERE key = 'dandad'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!enabled);
    let refused: String = sqlx::query_scalar(
        "SELECT reason_code FROM events.refused_sources WHERE domain = 'dandad.org'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refused, "terms");

    pool.close().await;
    tdb.drop_db().await;
}
