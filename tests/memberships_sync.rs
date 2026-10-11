//! `repo::sync_memberships` (#315): it records each event's memberships
//! once, and follows its venue's offer when that changes or goes away.

mod common;

use common::TestDb;
use musenmingle::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use musenmingle::repo;
use sqlx::PgPool;

fn event(title: &str, venue: &str, description: Option<&str>) -> NewEvent {
    NewEvent {
        sessions: Vec::new(),
        title: title.into(),
        description: description.map(str::to_string),
        venue_name: Some(venue.into()),
        address: None,
        lat: Some(51.5076),
        lng: Some(-0.0994),
        starts_at: "2026-10-10T10:00:00Z".parse().unwrap(),
        ends_at: None,
        all_day: false,
        price: Price::default(),
        url: None,
        image_url: None,
        category: Category::Exhibition,
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

async fn stored(pool: &PgPool) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT ev.title, em.membership, em.offer
         FROM events.event_memberships em JOIN events.events ev ON ev.id = em.event_id
         ORDER BY ev.title",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn set_offer(pool: &PgPool, sql: &'static str) {
    sqlx::query(sql).execute(pool).await.unwrap();
}

#[tokio::test]
async fn memberships_follow_the_venue_and_the_text() {
    let Some(db) = TestDb::create("memberships_follow_the_venue_and_the_text").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::upsert_source(
        &pool,
        "memberships-test",
        SourceKind::Scraper,
        "https://example.org",
        60,
        true,
    )
    .await
    .unwrap()
    .id;
    ingest(&pool, src, &event("Electric Dreams", "Tate Modern", None)).await;
    ingest(
        &pool,
        src,
        &event(
            "Prints and drawings",
            "Hoxton Community Hall",
            Some("Half price with an Art Pass."),
        ),
    )
    .await;
    ingest(
        &pool,
        src,
        &event("Local makers", "Hoxton Community Hall", None),
    )
    .await;
    repo::sync_venues(&pool).await.unwrap();

    let text_only = (
        "Prints and drawings".to_string(),
        "art_pass".to_string(),
        None,
    );
    assert_eq!(repo::sync_memberships(&pool).await.unwrap(), 1);
    assert_eq!(repo::sync_memberships(&pool).await.unwrap(), 0);
    assert_eq!(stored(&pool).await, std::slice::from_ref(&text_only));

    set_offer(
        &pool,
        "INSERT INTO events.venue_memberships (venue_id, membership, offer, source_note)
         SELECT id, 'art_pass', '50% off exhibitions', 'the author' FROM events.venues
         WHERE name = 'Tate Modern'",
    )
    .await;
    assert_eq!(repo::sync_memberships(&pool).await.unwrap(), 1);
    let tate = |offer: &str| {
        (
            "Electric Dreams".to_string(),
            "art_pass".to_string(),
            Some(offer.to_string()),
        )
    };
    assert_eq!(
        stored(&pool).await,
        [tate("50% off exhibitions"), text_only.clone()]
    );

    set_offer(
        &pool,
        "UPDATE events.venue_memberships SET offer = 'Free entry to exhibitions'",
    )
    .await;
    assert_eq!(repo::sync_memberships(&pool).await.unwrap(), 1);
    assert_eq!(
        stored(&pool).await,
        [tate("Free entry to exhibitions"), text_only.clone()]
    );

    set_offer(&pool, "DELETE FROM events.venue_memberships").await;
    assert_eq!(repo::sync_memberships(&pool).await.unwrap(), 1);
    assert_eq!(stored(&pool).await, [text_only]);
    pool.close().await;
    db.drop_db().await;
}
