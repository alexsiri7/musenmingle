//! Cross-source merge: the same event from Ticketmaster and from the venue's
//! own site becomes ONE `events.events` row with TWO `events.event_sources`.

mod common;

use common::{TestDb, fixture};
use thaleia::model::RawEvent;
use thaleia::repo;
use thaleia::sources::serpentine::parse_detail;
use thaleia::sources::{serpentine, ticketmaster};

const TALK_SLUG: &str = "saturday-talks-liz-stumpf-on-lanza-ateliers-2026-serpentine-pavilion-2";

fn ticketmaster_talk() -> RawEvent {
    let page: serde_json::Value =
        serde_json::from_str(&fixture("ticketmaster/events-page-0.json")).unwrap();
    let ev = page["_embedded"]["events"][0].clone();
    assert_eq!(ev["id"], "Z698xZG2Z17aTalks");
    RawEvent {
        source_event_id: ev["id"].as_str().unwrap().into(),
        source_url: ev["url"].as_str().map(str::to_string),
        payload: ev,
    }
}

fn serpentine_talk() -> RawEvent {
    parse_detail(
        &fixture(&format!(
            "scrapers/serpentine-galleries/detail/{TALK_SLUG}.html"
        )),
        &format!("/whats-on/{TALK_SLUG}/"),
    )
    .expect("fixture has JSON-LD Event")
}

#[tokio::test]
async fn ticketmaster_and_venue_site_copies_merge_into_one_event() {
    let Some(db) = TestDb::create("ticketmaster_and_venue_site_copies_merge_into_one_event").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let tm_src = repo::source_by_key(&pool, "ticketmaster")
        .await
        .unwrap()
        .unwrap();
    let sp_src = repo::source_by_key(&pool, "serpentine-galleries")
        .await
        .unwrap()
        .unwrap();

    let tm_raw = ticketmaster_talk();
    let tm_event = ticketmaster::normalise_event(&tm_raw.payload)
        .unwrap()
        .unwrap();
    let sp_raw = serpentine_talk();
    let sp_event = serpentine::normalise_payload(&sp_raw.payload)
        .unwrap()
        .unwrap();

    // Genuinely different inputs (title case/apostrophe, time zone handling,
    // address formatting, end time) that normalise to the same key.
    assert_ne!(tm_event.title, sp_event.title);
    assert_ne!(tm_event.ends_at, sp_event.ends_at);
    assert_eq!(tm_event.dedupe_key, sp_event.dedupe_key);

    let a = repo::upsert_event(&pool, tm_src.id, &tm_event, &tm_raw)
        .await
        .unwrap();
    assert!(a.created);
    let b = repo::upsert_event(&pool, sp_src.id, &sp_event, &sp_raw)
        .await
        .unwrap();
    assert!(!b.created);
    assert_eq!(a.event_id, b.event_id);

    // Re-running both is idempotent.
    let a2 = repo::upsert_event(&pool, tm_src.id, &tm_event, &tm_raw)
        .await
        .unwrap();
    let b2 = repo::upsert_event(&pool, sp_src.id, &sp_event, &sp_raw)
        .await
        .unwrap();
    assert_eq!((a2.event_id, a2.created), (a.event_id, false));
    assert_eq!((b2.event_id, b2.created), (a.event_id, false));

    let n_events: i64 = sqlx::query_scalar("SELECT count(*) FROM events.events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n_events, 1);
    let links: Vec<(i64, String, serde_json::Value)> = sqlx::query_as(
        "SELECT source_id, source_event_id, raw FROM events.event_sources
         WHERE event_id = $1 ORDER BY source_id",
    )
    .bind(a.event_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(links.len(), 2);
    let mut ids: Vec<i64> = links.iter().map(|l| l.0).collect();
    ids.sort();
    let mut want = vec![tm_src.id, sp_src.id];
    want.sort();
    assert_eq!(ids, want);
    assert!(links.iter().any(|l| l.1 == "Z698xZG2Z17aTalks"));
    assert!(
        links
            .iter()
            .any(|l| l.1 == TALK_SLUG && l.2.get("jsonld").is_some())
    );

    // Merge policy: the first source's values stand, the second fills gaps
    // (Ticketmaster had no end time; the venue site does).
    let row = repo::get_event(&pool, a.event_id).await.unwrap().unwrap();
    assert_eq!(row.title, tm_event.title);
    assert_eq!(row.ends_at, sp_event.ends_at);
    assert!(row.is_free);
    assert!(row.tags.contains(&"lecture/seminar".to_string()));
    assert!(row.tags.contains(&"contemporary-art".to_string()));

    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn different_dates_stay_separate_and_retitled_events_move() {
    let Some(db) = TestDb::create("different_dates_stay_separate_and_retitled_events_move").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::source_by_key(&pool, "ticketmaster")
        .await
        .unwrap()
        .unwrap();
    let raw = ticketmaster_talk();
    let base = ticketmaster::normalise_event(&raw.payload)
        .unwrap()
        .unwrap();

    // Same title, a week later, from a different source id: separate event.
    let mut later = base.clone();
    later.starts_at += chrono::Duration::days(7);
    later.dedupe_key =
        thaleia::normalise::dedupe_key(&later.title, later.starts_at, later.venue_name.as_deref());
    let raw_later = RawEvent {
        source_event_id: "other-id".into(),
        ..raw.clone()
    };
    let x = repo::upsert_event(&pool, src.id, &base, &raw)
        .await
        .unwrap();
    let y = repo::upsert_event(&pool, src.id, &later, &raw_later)
        .await
        .unwrap();
    assert_ne!(x.event_id, y.event_id);

    // The first listing is later corrected to the second date: its link
    // moves to the existing event and the orphaned row is removed.
    let z = repo::upsert_event(&pool, src.id, &later, &raw)
        .await
        .unwrap();
    assert_eq!(z.event_id, y.event_id);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM events.events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // A plain title fix from the same source updates in place.
    let mut renamed = later.clone();
    renamed.title = "Saturday Talks (updated)".into();
    renamed.dedupe_key = thaleia::normalise::dedupe_key(
        &renamed.title,
        renamed.starts_at,
        renamed.venue_name.as_deref(),
    );
    let w = repo::upsert_event(&pool, src.id, &renamed, &raw_later)
        .await
        .unwrap();
    assert_eq!(w.event_id, y.event_id);
    let row = repo::get_event(&pool, y.event_id).await.unwrap().unwrap();
    assert_eq!(row.title, "Saturday Talks (updated)");

    pool.close().await;
    db.drop_db().await;
}
