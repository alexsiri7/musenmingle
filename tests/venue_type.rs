//! Venue type (#78): `repo::sync_venue_types` classifies every event
//! (override in `events.venues`, source default, keywords, else `other`),
//! and `venue_type=` filters the listing with facet counts that agree.

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

fn event(title: &str, venue: &str) -> NewEvent {
    NewEvent {
        title: title.into(),
        description: None,
        venue_name: Some(venue.into()),
        address: None,
        lat: None,
        lng: None,
        starts_at: t("2026-10-10T18:00:00Z"),
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

async fn source(pool: &PgPool, key: &str) -> i64 {
    repo::upsert_source(
        pool,
        key,
        SourceKind::Scraper,
        "https://example.org",
        60,
        true,
    )
    .await
    .unwrap()
    .id
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
async fn venue_types_are_classified_filtered_and_counted() {
    let Some(db) = TestDb::create("venue_types_are_classified_filtered_and_counted").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let vam = source(&pool, "vam").await;
    let gallery = source(&pool, "artlogic-test-gallery").await;
    let tm = source(&pool, "ticketmaster").await;
    let luma = source(&pool, "luma-test").await;

    ingest(&pool, vam, &event("vam-show", "V&A South Kensington")).await;
    ingest(&pool, gallery, &event("gallery-show", "Test Gallery")).await;
    ingest(&pool, luma, &event("meetup", "Not For Sale Gallery")).await;
    // Ticketmaster has no default: the seeded override, keywords, other.
    ingest(&pool, tm, &event("ra-show", "The Royal Academy of Arts")).await;
    ingest(&pool, tm, &event("studio-show", "Acme Project Space")).await;
    ingest(&pool, tm, &event("gig", "Playhouse Theatre")).await;

    // New events start as 'other' until the sync.
    assert_eq!(
        titles(&pool, "venue_type=museum").await,
        Vec::<String>::new()
    );
    assert_eq!(repo::sync_venue_types(&pool).await.unwrap(), 5);
    assert_eq!(repo::sync_venue_types(&pool).await.unwrap(), 0);

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT title, venue_type FROM events.events ORDER BY title")
            .fetch_all(&pool)
            .await
            .unwrap();
    let rows: Vec<(&str, &str)> = rows.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(
        rows,
        [
            ("gallery-show", "commercial_gallery"),
            ("gig", "other"),
            ("meetup", "community"),
            ("ra-show", "museum"),
            ("studio-show", "artist_run"),
            ("vam-show", "museum"),
        ]
    );

    assert_eq!(
        titles(&pool, "venue_type=museum").await,
        ["ra-show", "vam-show"]
    );
    assert_eq!(
        titles(&pool, "venue_type=artist_run&venue_type=community").await,
        ["meetup", "studio-show"]
    );

    // Counts ignore the facet's own selection and apply the others.
    let q = parse_query_at("venue_type=museum&facets=true", t(NOW)).unwrap();
    let counts = repo::facet_counts(&pool, &q, repo::Facet::VenueType)
        .await
        .unwrap();
    assert_eq!(
        counts,
        [
            ("museum".to_string(), 2),
            ("artist_run".to_string(), 1),
            ("commercial_gallery".to_string(), 1),
            ("community".to_string(), 1),
            ("other".to_string(), 1),
        ]
    );
    let q = parse_query_at("source=ticketmaster", t(NOW)).unwrap();
    let counts = repo::facet_counts(&pool, &q, repo::Facet::VenueType)
        .await
        .unwrap();
    assert_eq!(counts.iter().map(|c| c.1).sum::<i64>(), 3);
    // And other facets' counts respect the venue type.
    let q = parse_query_at("venue_type=museum", t(NOW)).unwrap();
    let listing = repo::listing_counts(&pool, &q.filter, None).await.unwrap();
    assert_eq!(listing.evening, 2);

    // A new override moves an event at the next sync.
    sqlx::query(
        "INSERT INTO events.venues (name, venue_type) VALUES ('Playhouse Theatre', 'community')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(repo::sync_venue_types(&pool).await.unwrap(), 1);
    assert_eq!(
        titles(&pool, "venue_type=community").await,
        ["gig", "meetup"]
    );
}

#[tokio::test]
async fn venue_rows_without_coordinates_do_not_break_the_coordinates_lookup() {
    let Some(db) = TestDb::create("venue_rows_without_coordinates").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let tm = source(&pool, "ticketmaster").await;
    // Tate Modern is seeded with a type and no coordinates.
    ingest(&pool, tm, &event("tate", "Tate Modern")).await;
    // Cutty Sark keeps its #71 coordinates and gains a type.
    ingest(&pool, tm, &event("cutty", "Cutty Sark")).await;
    let rows: Vec<(String, Option<f64>)> =
        sqlx::query_as("SELECT title, lat FROM events.events ORDER BY title")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows[0].0, "cutty");
    assert!(rows[0].1.is_some());
    assert_eq!(rows[1], ("tate".to_string(), None));
    // Half a coordinate pair is rejected.
    assert!(
        sqlx::query("INSERT INTO events.venues (name, lat) VALUES ('Half', 51.5)")
            .execute(&pool)
            .await
            .is_err()
    );
}
