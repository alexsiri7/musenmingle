//! The all_day backfill migration (#99) flags stored date-only rows: London
//! midnight start and end (or no end), every linked source a date-only
//! venue scraper.

mod common;

use common::TestDb;
use musenmingle::db;
use sqlx::PgPool;
use uuid::Uuid;

/// The last migration before `20260928120001_backfill_all_day.sql`.
const BEFORE_BACKFILL: i64 = 20260928110001;

/// Inserts an event whose times are London wall-clock times.
async fn insert_event(pool: &PgPool, title: &str, starts: &str, ends: Option<&str>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events (title, starts_at, ends_at, category, dedupe_key)
         VALUES ($1, $2::timestamp AT TIME ZONE 'Europe/London',
                 $3::timestamp AT TIME ZONE 'Europe/London', 'exhibition', $1)
         RETURNING id",
    )
    .bind(title)
    .bind(starts)
    .bind(ends)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn link(pool: &PgPool, event: Uuid, source: &str) {
    sqlx::query(
        "INSERT INTO events.event_sources (event_id, source_id, source_event_id, raw)
         SELECT $1, id, $1::text, '{}' FROM events.sources WHERE key = $2",
    )
    .bind(event)
    .bind(source)
    .execute(pool)
    .await
    .unwrap();
}

async fn all_day(pool: &PgPool, event: Uuid) -> bool {
    sqlx::query_scalar("SELECT all_day FROM events.events WHERE id = $1")
        .bind(event)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn backfill_flags_only_midnight_rows_from_date_only_sources() {
    let Some(tdb) = TestDb::create("backfill_all_day").await else {
        return;
    };
    let pool = db::connect(&tdb.url()).await.unwrap();
    db::ensure_schema(&pool).await.unwrap();
    db::migrator().run_to(BEFORE_BACKFILL, &pool).await.unwrap();

    let one_day = insert_event(&pool, "One day", "2026-10-04 00:00", None).await;
    link(&pool, one_day, "somerset-house").await;

    let range = insert_event(&pool, "Range", "2026-11-01 00:00", Some("2026-12-01 00:00")).await;
    link(&pool, range, "courtauld").await;

    let merged = insert_event(&pool, "Merged", "2026-10-05 00:00", None).await;
    link(&pool, merged, "somerset-house").await;
    link(&pool, merged, "courtauld").await;

    let timed = insert_event(&pool, "Timed", "2026-10-04 18:00", None).await;
    link(&pool, timed, "courtauld").await;

    let timed_end = insert_event(
        &pool,
        "Timed end",
        "2026-10-04 00:00",
        Some("2026-10-04 17:00"),
    )
    .await;
    link(&pool, timed_end, "somerset-house").await;

    // UTC midnight is 01:00 in London during BST.
    let utc_midnight = insert_event(&pool, "UTC midnight", "2026-10-04 01:00", None).await;
    link(&pool, utc_midnight, "somerset-house").await;

    let ticketed = insert_event(&pool, "Ticketed", "2026-10-06 00:00", None).await;
    link(&pool, ticketed, "ticketmaster").await;

    let mixed = insert_event(&pool, "Mixed", "2026-10-07 00:00", None).await;
    link(&pool, mixed, "somerset-house").await;
    link(&pool, mixed, "ticketmaster").await;

    let unlinked = insert_event(&pool, "Unlinked", "2026-10-08 00:00", None).await;

    db::migrate(&pool).await.unwrap();

    for event in [one_day, range, merged] {
        assert!(all_day(&pool, event).await);
    }
    for event in [timed, timed_end, utc_midnight, ticketed, mixed, unlinked] {
        assert!(!all_day(&pool, event).await);
    }

    pool.close().await;
    tdb.drop_db().await;
}
