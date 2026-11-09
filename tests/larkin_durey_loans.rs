//! The Larkin Durey museum-loans migration (#305) deletes the two loans only
//! Larkin Durey listed, keeps one another source also lists, and leaves the
//! gallery's other events alone.

mod common;

use common::TestDb;
use musenmingle::db;
use sqlx::PgPool;
use uuid::Uuid;

/// The last migration before `20261109400001_artlogic_larkin_durey_loans.sql`.
const BEFORE_LOANS: i64 = 20261109300001;

async fn insert_event(pool: &PgPool, title: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events (title, starts_at, category, dedupe_key)
         VALUES ($1, now() + interval '7 days', 'exhibition', $1)
         RETURNING id",
    )
    .bind(title)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn link(pool: &PgPool, event: Uuid, source: &str, id: &str) {
    sqlx::query(
        "INSERT INTO events.event_sources (event_id, source_id, source_event_id, raw)
         SELECT $1, id, $3, '{}' FROM events.sources WHERE key = $2",
    )
    .bind(event)
    .bind(source)
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn loans_migration_deletes_larkin_durey_only_loans_and_keeps_shared_ones() {
    let Some(tdb) = TestDb::create("larkin_durey_loans").await else {
        return;
    };
    let pool = db::connect(&tdb.url()).await.unwrap();
    db::ensure_schema(&pool).await.unwrap();
    db::migrator().run_to(BEFORE_LOANS, &pool).await.unwrap();

    let only = insert_event(&pool, "Anina Major: The Sacred Mangrove").await;
    link(
        &pool,
        only,
        "artlogic-larkin-durey",
        "www.larkindurey.com:196",
    )
    .await;

    let shared = insert_event(&pool, "Through Our Eyes: Sámi Art and Culture").await;
    link(
        &pool,
        shared,
        "artlogic-larkin-durey",
        "www.larkindurey.com:198",
    )
    .await;
    link(&pool, shared, "somerset-house", "somerset-house:sami").await;

    let own = insert_event(&pool, "A show at the gallery").await;
    link(
        &pool,
        own,
        "artlogic-larkin-durey",
        "www.larkindurey.com:150",
    )
    .await;

    db::migrate(&pool).await.unwrap();

    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM events.events ORDER BY title")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids, vec![own, shared]);
    let shared_links: Vec<String> = sqlx::query_scalar(
        "SELECT s.key FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
          WHERE es.event_id = $1 ORDER BY s.key",
    )
    .bind(shared)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(shared_links, ["artlogic-larkin-durey", "somerset-house"]);

    let skip_match: serde_json::Value = sqlx::query_scalar(
        "SELECT config -> 'skip_match' FROM events.sources WHERE key = 'artlogic-larkin-durey'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(skip_match, serde_json::json!(["Museum", "Somerset House"]));

    pool.close().await;
    tdb.drop_db().await;
}
