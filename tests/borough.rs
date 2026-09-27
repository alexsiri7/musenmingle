//! London boroughs (#79): `repo::upsert_event` stores each event's borough
//! from its final coordinates, `repo::sync_boroughs` backfills and
//! refreshes it, and `borough=` filters the listing with facet counts that
//! agree (events without a location count as `unknown`).

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

fn event(title: &str, venue: &str, at: Option<(f64, f64)>) -> NewEvent {
    NewEvent {
        sessions: Vec::new(),
        title: title.into(),
        description: None,
        venue_name: Some(venue.into()),
        address: None,
        lat: at.map(|p| p.0),
        lng: at.map(|p| p.1),
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

async fn boroughs(pool: &PgPool) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT title, borough FROM events.events ORDER BY title")
        .fetch_all(pool)
        .await
        .unwrap()
}

fn row(title: &str, borough: Option<&str>) -> (String, Option<String>) {
    (title.into(), borough.map(Into::into))
}

#[tokio::test]
async fn boroughs_are_stored_filtered_and_counted() {
    let Some(db) = TestDb::create("boroughs_are_stored_filtered_and_counted").await else {
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

    ingest(
        &pool,
        src,
        &event("tate", "Tate Modern", Some((51.5076, -0.0994))),
    )
    .await;
    ingest(
        &pool,
        src,
        &event("barbican", "Barbican", Some((51.5202, -0.0938))),
    )
    .await;
    ingest(
        &pool,
        src,
        &event(
            "whitechapel",
            "Whitechapel Gallery",
            Some((51.5160, -0.0705)),
        ),
    )
    .await;
    ingest(&pool, src, &event("vam", "V&A", Some((51.4966, -0.1722)))).await;
    ingest(&pool, src, &event("nowhere", "Somewhere", None)).await;
    ingest(
        &pool,
        src,
        &event("brighton", "Brighton Dome", Some((50.8225, -0.1372))),
    )
    .await;
    // No coordinates of its own: filled from the seeded `events.venues`.
    ingest(&pool, src, &event("ra", "Royal Academy of Arts", None)).await;

    // Set at upsert time; the sync then has nothing to do.
    let want = [
        row("barbican", Some("city-of-london")),
        row("brighton", None),
        row("nowhere", None),
        row("ra", Some("westminster")),
        row("tate", Some("southwark")),
        row("vam", Some("kensington-and-chelsea")),
        row("whitechapel", Some("tower-hamlets")),
    ];
    assert_eq!(boroughs(&pool).await, want);
    assert_eq!(repo::sync_boroughs(&pool).await.unwrap(), 0);

    // Backfill: rows written before the column existed (or edited by hand).
    sqlx::query("UPDATE events.events SET borough = NULL")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE events.events SET borough = 'camden' WHERE title = 'nowhere'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(repo::sync_boroughs(&pool).await.unwrap(), 6);
    assert_eq!(boroughs(&pool).await, want);
    assert_eq!(repo::sync_boroughs(&pool).await.unwrap(), 0);

    // The filter, repeated values, and events without a borough never match.
    assert_eq!(titles(&pool, "borough=southwark").await, ["tate"]);
    assert_eq!(
        titles(&pool, "borough=city-of-london&borough=tower-hamlets").await,
        ["barbican", "whitechapel"]
    );
    assert!(titles(&pool, "borough=camden").await.is_empty());

    // Facet counts ignore the borough selection, apply the other filters,
    // and count events without a borough as `unknown`.
    let q = parse_query_at("borough=southwark", t(NOW)).unwrap();
    let mut counts = repo::facet_counts(&pool, &q, repo::Facet::Borough)
        .await
        .unwrap();
    counts.sort();
    let counts: Vec<(&str, i64)> = counts.iter().map(|(k, n)| (k.as_str(), *n)).collect();
    assert_eq!(
        counts,
        [
            ("city-of-london", 1),
            ("kensington-and-chelsea", 1),
            ("southwark", 1),
            ("tower-hamlets", 1),
            ("unknown", 2),
            ("westminster", 1),
        ]
    );
    let q = parse_query_at("q=tate", t(NOW)).unwrap();
    let counts = repo::facet_counts(&pool, &q, repo::Facet::Borough)
        .await
        .unwrap();
    assert_eq!(counts, [("southwark".to_string(), 1)]);

    // Other counts respect the borough (2 of the 7 events, no prices).
    let q = parse_query_at("borough=westminster&borough=southwark", t(NOW)).unwrap();
    let listing = repo::listing_counts(&pool, &q.filter, None).await.unwrap();
    assert_eq!(listing.unknown, 2);

    // Moving an event (its source corrects the coordinates: Tate Britain) moves its borough.
    ingest(
        &pool,
        src,
        &event("tate", "Tate Modern", Some((51.4911, -0.1278))),
    )
    .await;
    assert!(titles(&pool, "borough=southwark").await.is_empty());
    assert_eq!(titles(&pool, "borough=westminster").await, ["ra", "tate"]);
}
