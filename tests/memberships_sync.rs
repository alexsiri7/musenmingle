//! `repo::sync_memberships` (#315): it records each event's memberships
//! once, and follows its venue's offer when that changes or goes away; and
//! the seeded National Art Pass venues (#317).

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
    // The test keeps its own list, not the seeded venues (#317).
    set_offer(&pool, "DELETE FROM events.venue_memberships").await;
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

#[tokio::test]
async fn the_seeded_art_pass_venues_cite_their_own_pages() {
    let Some(db) = TestDb::create("the_seeded_art_pass_venues_cite_their_own_pages").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let seeded: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT v.name, vm.offer, vm.source_note
         FROM events.venue_memberships vm JOIN events.venues v ON v.id = vm.venue_id
         WHERE vm.membership = 'art_pass' ORDER BY v.name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(seeded.len(), 11, "{seeded:?}");
    for (venue, _, note) in &seeded {
        let (url, date) = note.split_once(' ').unwrap();
        assert!(url.starts_with("https://"), "{venue}: {note}");
        assert!(!url.contains("artfund.org"), "{venue}: {note}");
        assert!(
            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok(),
            "{venue}: {note}"
        );
    }
    assert!(seeded.contains(&(
        "Tate Modern".into(),
        "50% off exhibitions".into(),
        "https://www.tate.org.uk/about-us/frequently-asked-questions 2026-10-11".into(),
    )));

    // A listing at a seeded venue its source names the same way gets the offer.
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
    ingest(&pool, src, &event("Prints", "Whitechapel Gallery", None)).await;
    repo::sync_venues(&pool).await.unwrap();
    assert_eq!(repo::sync_memberships(&pool).await.unwrap(), 1);
    assert_eq!(
        stored(&pool).await,
        [(
            "Prints".to_string(),
            "art_pass".to_string(),
            Some("50% off exhibition tickets (£8.25, standard £16.50)".to_string()),
        )]
    );
    pool.close().await;
    db.drop_db().await;
}

/// The migration before `20261011100001_seed_art_pass_venues.sql`.
const BEFORE_ART_PASS_SEED: i64 = 20261011000001;

#[tokio::test]
async fn the_art_pass_seed_attaches_to_venues_that_already_exist() {
    let Some(db) = TestDb::create("the_art_pass_seed_attaches_to_venues_that_already_exist").await
    else {
        return;
    };
    let pool = musenmingle::db::connect(&db.url()).await.unwrap();
    musenmingle::db::ensure_schema(&pool).await.unwrap();
    musenmingle::db::migrator()
        .run_to(BEFORE_ART_PASS_SEED, &pool)
        .await
        .unwrap();
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
    // In production `repo::sync_venues` has made these rows from listings
    // before the seed runs.
    let venues = [
        "Tate Modern",
        "Tate Britain",
        "Barbican Centre",
        "Whitechapel Gallery",
        "Estorick Collection",
        "The Photographers' Gallery",
        "The Courtauld Gallery",
        "Old Royal Naval College",
        "Garden Museum",
        "Handel Hendrix House",
        "Foundling Museum",
    ];
    for venue in venues {
        ingest(&pool, src, &event(venue, venue, None)).await;
    }
    repo::sync_venues(&pool).await.unwrap();
    let before: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM events.venues ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();

    musenmingle::db::migrate(&pool).await.unwrap();

    let after: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM events.venues ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(after, before, "the seed made no second venue row");
    let attached: Vec<String> = sqlx::query_scalar(
        "SELECT v.name FROM events.venue_memberships vm
         JOIN events.venues v ON v.id = vm.venue_id
         WHERE vm.membership = 'art_pass' ORDER BY v.name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut expected = venues.map(str::to_string).to_vec();
    expected.sort();
    assert_eq!(attached, expected);
    assert_eq!(
        repo::sync_memberships(&pool).await.unwrap(),
        venues.len() as u64
    );
    pool.close().await;
    db.drop_db().await;
}
