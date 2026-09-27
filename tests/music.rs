//! Music, option (b) (#209): the `music` category is stored and listed
//! like the others (in the default feed too), `repo::sync_music_tags` gives
//! music events their subtags, and `music=` filters the listing with facet
//! counts that agree.

mod common;

use chrono::{DateTime, Utc};
use common::TestDb;
use musenmingle::listing::parse_query_at;
use musenmingle::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use musenmingle::repo;
use sqlx::PgPool;

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn event(title: &str, category: Category, tags: &[&str]) -> NewEvent {
    NewEvent {
        sessions: Vec::new(),
        title: title.into(),
        description: None,
        venue_name: Some("Test Hall".into()),
        address: None,
        lat: None,
        lng: None,
        starts_at: t("2026-10-10T18:30:00Z"),
        ends_at: None,
        all_day: false,
        price: Price::default(),
        url: None,
        image_url: None,
        category,
        tags: tags.iter().map(|s| s.to_string()).collect(),
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

const NOW: &str = "2026-10-01T12:00:00Z";

async fn titles(pool: &PgPool, raw: &str) -> Vec<String> {
    let q = parse_query_at(raw, t(NOW)).unwrap();
    let mut v: Vec<String> = repo::list_events(pool, &q)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event.title)
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn music_events_are_listed_tagged_filtered_and_counted() {
    let Some(db) = TestDb::create("music_events_are_listed_tagged_filtered_and_counted").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::upsert_source(
        &pool,
        "music-test",
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
        &event("Julian Siegel Quartet", Category::Music, &["jazz"]),
    )
    .await;
    ingest(
        &pool,
        src,
        &event("String quartet: a world premiere", Category::Music, &[]),
    )
    .await;
    ingest(
        &pool,
        src,
        &event(
            "Hainbach live",
            Category::Music,
            &["Sound Art", "experimental"],
        ),
    )
    .await;
    // Not music: a talk about jazz gets no subtags.
    ingest(
        &pool,
        src,
        &event("Jazz in London: a talk", Category::Talk, &["jazz"]),
    )
    .await;

    // The category is stored and in the default feed.
    assert_eq!(
        titles(&pool, "").await,
        [
            "Hainbach live",
            "Jazz in London: a talk",
            "Julian Siegel Quartet",
            "String quartet: a world premiere",
        ]
    );
    assert_eq!(
        titles(&pool, "category=music").await,
        [
            "Hainbach live",
            "Julian Siegel Quartet",
            "String quartet: a world premiere",
        ]
    );

    // Subtags arrive with the post-run sync, once.
    assert_eq!(titles(&pool, "music=jazz").await, Vec::<String>::new());
    assert_eq!(repo::sync_music_tags(&pool).await.unwrap(), 3);
    assert_eq!(repo::sync_music_tags(&pool).await.unwrap(), 0);
    let rows: Vec<(String, Vec<String>)> =
        sqlx::query_as("SELECT title, music_tags FROM events.events ORDER BY title")
            .fetch_all(&pool)
            .await
            .unwrap();
    let rows: Vec<(&str, Vec<&str>)> = rows
        .iter()
        .map(|(a, b)| (a.as_str(), b.iter().map(String::as_str).collect()))
        .collect();
    assert_eq!(
        rows,
        [
            ("Hainbach live", vec!["experimental", "sound_art"]),
            ("Jazz in London: a talk", vec![]),
            ("Julian Siegel Quartet", vec!["jazz"]),
            (
                "String quartet: a world premiere",
                vec!["classical", "contemporary"]
            ),
        ]
    );

    assert_eq!(titles(&pool, "music=jazz").await, ["Julian Siegel Quartet"]);
    assert_eq!(
        titles(&pool, "music=sound_art&music=classical").await,
        ["Hainbach live", "String quartet: a world premiere"]
    );

    // Counts ignore the facet's own selection and apply the others.
    let q = parse_query_at("music=jazz&facets=true", t(NOW)).unwrap();
    let counts = repo::facet_counts(&pool, &q, repo::Facet::Music)
        .await
        .unwrap();
    assert_eq!(
        counts,
        [
            ("classical".to_string(), 1),
            ("contemporary".to_string(), 1),
            ("experimental".to_string(), 1),
            ("jazz".to_string(), 1),
            ("sound_art".to_string(), 1),
        ]
    );

    // A category change back out of music drops the subtags.
    sqlx::query("UPDATE events.events SET category = 'talk' WHERE title = 'Hainbach live'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(repo::sync_music_tags(&pool).await.unwrap(), 1);
    assert_eq!(titles(&pool, "music=sound_art").await, Vec::<String>::new());

    // The quick-pick count.
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
    let c = repo::quick_pick_counts(&pool, today, t(NOW), (t(NOW), t(NOW)))
        .await
        .unwrap();
    assert_eq!(c.music, 2);

    db.drop_db().await;
}
