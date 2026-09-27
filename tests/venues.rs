//! Venues as first-class objects (#204): `repo::sync_venues` creates a
//! venue per venue name (deduped on the normalised name and the aliases,
//! areas skipped), links events to it, fills venues' gaps from their events
//! and `events.venue_hours`, and events' missing coordinates from the venue.

mod common;

use chrono::{DateTime, Utc};
use common::TestDb;
use musenmingle::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use musenmingle::repo::{self, VenueSync};
use sqlx::PgPool;

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn event(title: &str, venue: &str, address: Option<&str>, at: Option<(f64, f64)>) -> NewEvent {
    NewEvent {
        title: title.into(),
        description: None,
        venue_name: Some(venue.into()),
        address: address.map(Into::into),
        lat: at.map(|p| p.0),
        lng: at.map(|p| p.1),
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

type Venue = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<f64>,
    Option<String>,
    Option<String>,
);

async fn venue(pool: &PgPool, name: &str) -> Venue {
    sqlx::query_as(
        "SELECT name, slug, address, postcode, lat, borough, coords_source
         FROM events.venues WHERE name = $1",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `(title, venue name or None, has coordinates)` per event.
async fn links(pool: &PgPool) -> Vec<(String, Option<String>, bool)> {
    sqlx::query_as(
        "SELECT ev.title, v.name, ev.lat IS NOT NULL FROM events.events ev
         LEFT JOIN events.venues v ON v.id = ev.venue_id ORDER BY ev.title",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn venues_are_created_deduped_linked_and_located() {
    let Some(db) = TestDb::create("venues_are_created_deduped_linked_and_located").await else {
        return;
    };
    let pool = db.migrated_pool().await;
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

    let peckham = "65-67 Peckham Road, London SE5 8UH";
    let point = Some((51.474, -0.0811));
    // Two spellings of one venue, one with a point, one without.
    ingest(
        &pool,
        src,
        &event("a", "Peckham Arts Hall", Some(peckham), point),
    )
    .await;
    ingest(
        &pool,
        src,
        &event("b", "The Peckham Arts Hall", Some(peckham), point),
    )
    .await;
    ingest(&pool, src, &event("c", "Peckham Arts Hall", None, None)).await;
    // No point anywhere: a venue without coordinates.
    ingest(
        &pool,
        src,
        &event(
            "d",
            "Nowhere Rooms",
            Some("1 Some Street, London N1 2AN"),
            None,
        ),
    )
    .await;
    // An alias of a seeded venue, an area, and a placeholder.
    ingest(
        &pool,
        src,
        &event("e", "Institute of Contemporary Arts", None, None),
    )
    .await;
    ingest(&pool, src, &event("f", "Clerkenwell", None, None)).await;
    ingest(&pool, src, &event("g", "-", None, None)).await;
    // A seeded venue with coordinates and a venue_hours row (Camden Art Centre).
    ingest(&pool, src, &event("h", "Camden Art Centre", None, None)).await;

    let r = repo::sync_venues(&pool).await.unwrap();
    assert_eq!(r.created, 3, "{r:?}"); // Peckham Arts Hall, Nowhere Rooms, Camden Art Centre
    assert_eq!(r.linked, 6);
    assert_eq!(r.located, 1); // c
    // Idempotent.
    assert_eq!(
        repo::sync_venues(&pool).await.unwrap(),
        VenueSync::default()
    );

    assert_eq!(
        links(&pool).await,
        [
            ("a".into(), Some("Peckham Arts Hall".into()), true),
            ("b".into(), Some("Peckham Arts Hall".into()), true),
            ("c".into(), Some("Peckham Arts Hall".into()), true),
            ("d".into(), Some("Nowhere Rooms".into()), false),
            ("e".into(), Some("ICA".into()), false),
            ("f".into(), None, false),
            ("g".into(), None, false),
            ("h".into(), Some("Camden Art Centre".into()), false),
        ]
    );
    assert_eq!(
        venue(&pool, "Peckham Arts Hall").await,
        (
            "Peckham Arts Hall".into(),
            Some("peckham-arts-hall".into()),
            Some(peckham.into()),
            Some("SE5 8UH".into()),
            Some(51.474),
            Some("southwark".into()),
            Some("listing".into()),
        )
    );
    let nowhere = venue(&pool, "Nowhere Rooms").await;
    assert_eq!(nowhere.3.as_deref(), Some("N1 2AN"));
    assert_eq!((nowhere.4, nowhere.6), (None, None));
    // Every venue has a unique slug, including the seeded ones.
    let (missing, dupes): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE slug IS NULL), count(*) - count(DISTINCT slug)
         FROM events.venues",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((missing, dupes), (0, 0));

    // venue_hours are copied onto the venue.
    let (hours, src_note): (Option<serde_json::Value>, Option<String>) = sqlx::query_as(
        "SELECT opening_hours, hours_source FROM events.venues WHERE name = 'Camden Art Centre'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(hours.is_some() && src_note.is_some());

    // A venue that later gets a point hands it to its events; an event that
    // changes venue is relinked.
    sqlx::query(
        "UPDATE events.venues SET lat = 51.544, lng = -0.0999, coords_source = 'test'
         WHERE name = 'Nowhere Rooms'",
    )
    .execute(&pool)
    .await
    .unwrap();
    ingest(&pool, src, &event("c", "Clerkenwell", None, None)).await;
    let r = repo::sync_venues(&pool).await.unwrap();
    assert_eq!((r.linked, r.located, r.updated), (1, 1, 1), "{r:?}");
    let l = links(&pool).await;
    assert_eq!(l[2], ("c".into(), None, true));
    assert_eq!(l[3], ("d".into(), Some("Nowhere Rooms".into()), true));
    assert_eq!(
        venue(&pool, "Nowhere Rooms").await.5.as_deref(),
        Some("islington")
    );
}
