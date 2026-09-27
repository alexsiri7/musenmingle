//! Opening hours (#168): read from a listing's full description at ingest,
//! inherited from `events.venue_hours`, kept off timed events, and used by
//! `at=now`, `open_now`/`open_at`, `open_on` and the `when=` buckets.

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

/// The Chats Palace listing (#168): Wed, Thu, Fri & Sun 11:00–15:00,
/// 9 Sep – 1 Nov 2026 (all-day; London midnights, end inclusive).
fn chats_palace(title: &str, venue: &str, description: Option<&str>, all_day: bool) -> NewEvent {
    NewEvent {
        sessions: Vec::new(),
        title: title.into(),
        description: description.map(str::to_string),
        venue_name: Some(venue.into()),
        address: None,
        lat: None,
        lng: None,
        starts_at: t("2026-09-08T23:00:00Z"),
        ends_at: Some(t("2026-10-31T23:00:00Z")),
        all_day,
        price: Price::default(),
        url: None,
        image_url: None,
        category: Category::Exhibition,
        tags: Vec::new(),
        dedupe_key: title.into(),
    }
}

const TEXT: &str = "Separated by fifty years the photographs have been taken by Hackney \
    photographer Neil Martinson. The exhibition will be showing September and October 2026.  \
    For the duration of its stay, the Exhibition will be on display on Wednesdays, Thursday, \
    Fridays, &amp; Sundays from 11am-3pm.";

async fn ingest(pool: &PgPool, source: i64, e: &NewEvent) {
    let raw = RawEvent {
        source_event_id: e.title.clone(),
        source_url: None,
        payload: serde_json::json!({}),
    };
    repo::upsert_event(pool, source, e, &raw).await.unwrap();
}

async fn titles(pool: &PgPool, raw: &str, now: &str) -> Vec<String> {
    let q = parse_query_at(raw, t(now)).unwrap();
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
async fn hours_are_stored_inherited_and_filtered_on() {
    let Some(db) = TestDb::create("hours_are_stored_inherited_and_filtered_on").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::upsert_source(
        &pool,
        "hours-test",
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
        &chats_palace("chats", "Chats Palace Arts Centre", Some(TEXT), true),
    )
    .await;
    // No hours in the text: inherits the seeded South London Botanical
    // Institute hours (Thu 10–16, Sat 10–14).
    ingest(
        &pool,
        src,
        &chats_palace("slbi", "South London Botanical Institute", None, true),
    )
    .await;
    // No hours anywhere: keeps the date-only behaviour.
    ingest(
        &pool,
        src,
        &chats_palace("plain", "Somewhere Else", None, true),
    )
    .await;
    // A timed run never gets hours, even with the same text.
    ingest(
        &pool,
        src,
        &chats_palace("timed", "Chats Palace Arts Centre", Some(TEXT), false),
    )
    .await;

    let rows: Vec<(String, Option<serde_json::Value>, Option<String>)> =
        sqlx::query_as("SELECT title, opening_hours, hours_note FROM events.events ORDER BY title")
            .fetch_all(&pool)
            .await
            .unwrap();
    let by = |title: &str| rows.iter().find(|r| r.0 == title).unwrap().clone();
    assert_eq!(
        by("chats").1,
        Some(serde_json::json!([{"days": [3, 4, 5, 7], "opens": "11:00", "closes": "15:00"}]))
    );
    assert!(by("chats").2.unwrap().ends_with("Sundays from 11am-3pm."));
    assert_eq!(
        by("slbi").1,
        Some(serde_json::json!([
            {"days": [4], "opens": "10:00", "closes": "16:00"},
            {"days": [6], "opens": "10:00", "closes": "14:00"}
        ]))
    );
    assert_eq!(by("slbi").2, None);
    assert_eq!(by("plain").1, None);
    assert_eq!(by("timed").1, None);

    let all = ["chats", "plain", "slbi", "timed"];
    let only = |keep: &[&str]| keep.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // The issue's case: Monday 12 Oct at 21:00 London. Only the event
    // without hours (and the timed run, whose "hours" are its dates) is on.
    assert_eq!(
        titles(&pool, "at=now", "2026-10-12T20:00:00Z").await,
        only(&["plain", "timed"])
    );
    // Wed 14 Oct 09:00 BST: Chats Palace opens at 11:00, within 3 hours.
    assert_eq!(
        titles(&pool, "at=now", "2026-10-14T08:00:00Z").await,
        only(&["chats", "plain", "timed"])
    );
    // ... but is not open at that instant.
    assert_eq!(
        titles(&pool, "open_now=true", "2026-10-14T08:00:00Z").await,
        only(&["plain", "timed"])
    );
    // The clock change: Sun 25 Oct 10:30Z is 10:30 GMT (closed), 11:30Z open.
    assert_eq!(
        titles(
            &pool,
            "open_at=2026-10-25T10:30:00Z",
            "2026-09-27T12:00:00Z"
        )
        .await,
        only(&["plain", "timed"])
    );
    assert_eq!(
        titles(&pool, "open_at=2026-10-25T11:30", "2026-09-27T12:00:00Z").await,
        only(&["chats", "plain", "timed"])
    );
    // Thu 15 Oct 12:00 BST (London wall clock): both venues open.
    assert_eq!(
        titles(&pool, "open_at=2026-10-15T12:00", "2026-09-27T12:00:00Z").await,
        all.map(String::from).to_vec()
    );
    // open_on: Sunday → Chats Palace (Sun) but not SLBI (Thu, Sat).
    assert_eq!(
        titles(&pool, "open_on=sun&from=2026-10-01", "2026-09-27T12:00:00Z").await,
        only(&["chats", "plain", "timed"])
    );
    assert_eq!(
        titles(
            &pool,
            "open_on=monday&from=2026-10-01",
            "2026-09-27T12:00:00Z"
        )
        .await,
        only(&["plain", "timed"])
    );
    // when=: hours replace the start time. Neither venue is open in the
    // evening; both are in the daytime; SLBI is open on Saturdays.
    assert_eq!(
        titles(
            &pool,
            "when=evening&from=2026-10-01",
            "2026-09-27T12:00:00Z"
        )
        .await,
        Vec::<String>::new()
    );
    assert_eq!(
        titles(
            &pool,
            "when=daytime&from=2026-10-01",
            "2026-09-27T12:00:00Z"
        )
        .await,
        all.map(String::from).to_vec()
    );
    assert_eq!(
        titles(
            &pool,
            "when=weekend&from=2026-10-01",
            "2026-09-27T12:00:00Z"
        )
        .await,
        all.map(String::from).to_vec()
    );
    // A window whose only weekend day is Sat 3 Oct: Chats Palace is closed
    // on Saturdays.
    assert_eq!(
        titles(
            &pool,
            "when=weekend&from=2026-10-01&to=2026-10-03",
            "2026-09-27T12:00:00Z"
        )
        .await,
        only(&["plain", "slbi", "timed"])
    );
    // Facet counts use the same predicates.
    let q = parse_query_at("from=2026-10-01&facets=true", t("2026-09-27T12:00:00Z")).unwrap();
    let counts = repo::listing_counts(&pool, &q.filter, q.near.as_ref())
        .await
        .unwrap();
    assert_eq!(counts.evening, 0);
    assert_eq!(counts.daytime, 4);

    // Hours are refreshed from the text and cleared when the run stops
    // being all-day.
    ingest(
        &pool,
        src,
        &chats_palace("chats", "Chats Palace Arts Centre", Some(TEXT), false),
    )
    .await;
    let h: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT opening_hours FROM events.events WHERE title = 'chats'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(h, None);
    db.drop_db().await;
}

/// Structured schema.org hours in a listing's payload (#206): the event's
/// own spec wins over its text; its `location`'s hours are the venue's,
/// filling `events.venues.opening_hours` (never overwriting) and serving as
/// the exhibition's fallback.
#[tokio::test]
async fn structured_hours_fill_the_event_and_its_venue() {
    let Some(db) = TestDb::create("structured_hours_fill_the_event_and_its_venue").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::upsert_source(
        &pool,
        "hours-structured",
        SourceKind::Scraper,
        "https://example.org",
        60,
        true,
    )
    .await
    .unwrap()
    .id;
    let upsert = |e: NewEvent, payload: serde_json::Value| {
        let pool = pool.clone();
        async move {
            let raw = RawEvent {
                source_event_id: e.title.clone(),
                source_url: Some("https://www.example.org/whats-on/x".into()),
                payload,
            };
            repo::upsert_event(&pool, src, &e, &raw).await.unwrap();
        }
    };
    let place = serde_json::json!({
        "@type": "ExhibitionEvent",
        "location": {"@type": "Place", "name": "Rivington Place",
                     "openingHours": ["Tu-Fr 11:00-18:00", "Sa 12:00-18:00"]}
    });
    let spec = serde_json::json!({
        "openingHoursSpecification": [{"dayOfWeek": ["Monday", "Tuesday"],
                                       "opens": "09:00", "closes": "12:00"}]
    });
    // Before the venue row exists: the exhibition still gets the hours.
    upsert(
        chats_palace("placed", "Rivington Place", None, true),
        place.clone(),
    )
    .await;
    // The event's own spec wins over the hours in its text.
    upsert(
        chats_palace("spec", "Chats Palace Arts Centre", Some(TEXT), true),
        spec,
    )
    .await;
    repo::sync_venues(&pool).await.unwrap();
    let venue_hours = || async {
        sqlx::query_as::<_, (Option<serde_json::Value>, Option<String>)>(
            "SELECT opening_hours, hours_source FROM events.venues WHERE name = 'Rivington Place'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(venue_hours().await.0, None);
    // The next run's listing fills the venue's hours.
    upsert(
        chats_palace("placed", "Rivington Place", None, true),
        place.clone(),
    )
    .await;
    let want = serde_json::json!([
        {"days": [2, 3, 4, 5], "opens": "11:00", "closes": "18:00"},
        {"days": [6], "opens": "12:00", "closes": "18:00"}
    ]);
    let (h, source) = venue_hours().await;
    assert_eq!(h, Some(want.clone()));
    assert!(
        source
            .unwrap()
            .starts_with("schema.org structured data on www.example.org, "),
    );
    // Never overwritten by different structured hours later.
    let other = serde_json::json!({"location": {"openingHours": "Mo 10:00-11:00"}});
    upsert(chats_palace("other", "Rivington Place", None, true), other).await;
    assert_eq!(venue_hours().await.0, Some(want.clone()));

    let rows: Vec<(String, Option<serde_json::Value>, Option<String>)> =
        sqlx::query_as("SELECT title, opening_hours, hours_note FROM events.events ORDER BY title")
            .fetch_all(&pool)
            .await
            .unwrap();
    let by = |title: &str| rows.iter().find(|r| r.0 == title).unwrap().clone();
    assert_eq!(by("placed").1, Some(want));
    assert_eq!(
        by("spec").1,
        Some(serde_json::json!([{"days": [1, 2], "opens": "09:00", "closes": "12:00"}]))
    );
    assert_eq!(by("spec").2, None);
    db.drop_db().await;
}
