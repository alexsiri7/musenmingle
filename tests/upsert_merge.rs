//! Cross-source merge: the same event from Ticketmaster and from the venue's
//! own site becomes ONE `events.events` row with TWO `events.event_sources`,
//! whether the dedupe keys agree exactly or only fuzzily, subject to
//! `events.merge_overrides`.

mod common;

use chrono::{DateTime, Utc};
use common::{TestDb, fixture};
use rust_decimal::Decimal;
use sqlx::PgPool;
use thaleia::matching::{self, MatchInput};
use thaleia::model::{Category, NewEvent, OverrideAction, Price, RawEvent, SourceKind};
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

    // Merge policy: the title is first-come; the venue site wins dates,
    // description and image; the API wins price and URL; anything else only
    // fills gaps.
    let row = repo::get_event(&pool, a.event_id).await.unwrap().unwrap();
    assert_eq!(row.title, tm_event.title);
    assert_eq!(row.starts_at, sp_event.starts_at);
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

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn ev(
    title: &str,
    venue: &str,
    (lat, lng): (f64, f64),
    starts: &str,
    ends: Option<&str>,
) -> NewEvent {
    let starts_at = t(starts);
    NewEvent {
        title: title.into(),
        description: None,
        venue_name: Some(venue.into()),
        address: None,
        lat: Some(lat),
        lng: Some(lng),
        starts_at,
        ends_at: ends.map(t),
        price: Price::default(),
        url: None,
        image_url: None,
        category: Category::Exhibition,
        tags: vec![],
        dedupe_key: thaleia::normalise::dedupe_key(title, starts_at, Some(venue)),
    }
}

fn raw(id: &str) -> RawEvent {
    RawEvent {
        source_event_id: id.into(),
        source_url: None,
        payload: serde_json::json!({}),
    }
}

async fn source_id(pool: &PgPool, key: &str) -> i64 {
    repo::source_by_key(pool, key).await.unwrap().unwrap().id
}

async fn event_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM events.events")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn link_count(pool: &PgPool, event_id: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM events.event_sources WHERE event_id = $1")
        .bind(event_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn add_override(pool: &PgPool, action: OverrideAction, x: (i64, &str), y: (i64, &str)) {
    let (a, b) = if x < y { (x, y) } else { (y, x) };
    sqlx::query(
        "INSERT INTO events.merge_overrides
             (action, source_id_a, source_event_id_a, source_id_b, source_event_id_b)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(action)
    .bind(a.0)
    .bind(a.1)
    .bind(b.0)
    .bind(b.1)
    .execute(pool)
    .await
    .unwrap();
}

const TATE: (f64, f64) = (51.5076, -0.0994);
const TATE_NEARBY: (f64, f64) = (51.5077, -0.0990);

/// The same Tate Modern show as Ticketmaster (priced, no description) and a
/// venue site (description, image, end time) list it, under different titles.
fn kusama_pair() -> (NewEvent, NewEvent) {
    let mut tm = ev(
        "Yayoi Kusama: Infinity Rooms – Tate Modern",
        "Tate Modern",
        TATE,
        "2026-10-10T17:00:00Z",
        None,
    );
    tm.price = Price {
        is_free: false,
        min: Some(Decimal::from(20)),
        max: Some(Decimal::from(20)),
        currency: Some("GBP".into()),
    };
    tm.url = Some("https://tickets.example/kusama".into());
    let mut sp = ev(
        "Yayoi Kusama: Infinity Mirror Rooms",
        "Tate Modern",
        TATE_NEARBY,
        "2026-10-10T18:00:00Z",
        Some("2026-10-10T21:00:00Z"),
    );
    sp.description = Some("Two rooms of mirrors.".into());
    sp.image_url = Some("https://venue.example/kusama.jpg".into());
    sp.url = Some("https://venue.example/kusama".into());
    (tm, sp)
}

#[tokio::test]
async fn fuzzy_merge_is_stable_across_reruns_in_both_orders() {
    let (tm, sp) = kusama_pair();
    assert_ne!(tm.dedupe_key, sp.dedupe_key);

    let mut results = Vec::new();
    for tm_first in [true, false] {
        let Some(db) = TestDb::create("fuzzy_merge_is_stable_across_reruns_in_both_orders").await
        else {
            return;
        };
        let pool = db.migrated_pool().await;
        let tm_src = source_id(&pool, "ticketmaster").await;
        let sp_src = source_id(&pool, "serpentine-galleries").await;
        let tm_listing = (tm_src, &tm, raw("tm-kusama"));
        let sp_listing = (sp_src, &sp, raw("sp-kusama"));
        let (first, second) = if tm_first {
            (&tm_listing, &sp_listing)
        } else {
            (&sp_listing, &tm_listing)
        };

        let mut outcomes = Vec::new();
        for (src, event, raw) in [first, second, first, second] {
            outcomes.push(repo::upsert_event(&pool, *src, event, raw).await.unwrap());
        }
        assert!(outcomes[0].created);
        assert!(outcomes[1..].iter().all(|o| !o.created), "{outcomes:?}");
        assert!(outcomes.iter().all(|o| o.event_id == outcomes[0].event_id));
        assert_eq!(event_count(&pool).await, 1);
        assert_eq!(link_count(&pool, outcomes[0].event_id).await, 2);

        let row = repo::get_event(&pool, outcomes[0].event_id)
            .await
            .unwrap()
            .unwrap();
        results.push((
            row.starts_at,
            row.ends_at,
            row.description,
            row.image_url,
            row.price_min,
            row.url,
        ));
        pool.close().await;
        db.drop_db().await;
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(
        results[0],
        (
            sp.starts_at,
            sp.ends_at,
            sp.description.clone(),
            sp.image_url.clone(),
            tm.price.min,
            tm.url.clone(),
        )
    );
}

#[tokio::test]
async fn same_source_listings_never_fuzzy_merge() {
    let Some(db) = TestDb::create("same_source_listings_never_fuzzy_merge").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let tm_src = source_id(&pool, "ticketmaster").await;
    let sp_src = source_id(&pool, "serpentine-galleries").await;
    let at = |title: &str| {
        ev(
            title,
            "Whitechapel Gallery",
            (51.5160, -0.0700),
            "2026-10-10T10:00:00Z",
            None,
        )
    };

    let threads = at("Cecilia Vicuña: Living Threads");
    let threads_exhibition = at("Cecilia Vicuña: Living Threads Exhibition");
    let s1 = repo::upsert_event(&pool, sp_src, &threads, &raw("sp-1"))
        .await
        .unwrap();
    let s2 = repo::upsert_event(&pool, sp_src, &threads_exhibition, &raw("sp-2"))
        .await
        .unwrap();
    assert!(s2.created);
    assert_eq!(event_count(&pool).await, 2);

    let quipu = repo::upsert_event(
        &pool,
        tm_src,
        &at("Cecilia Vicuña: Foraging Quipu"),
        &raw("tm-quipu"),
    )
    .await
    .unwrap();
    assert!(quipu.created);
    assert_eq!(event_count(&pool).await, 3);

    let tickets = at("Cecilia Vicuña: Living Threads Tickets");
    assert_ne!(tickets.dedupe_key, threads.dedupe_key);
    assert_ne!(tickets.dedupe_key, threads_exhibition.dedupe_key);
    let joined = repo::upsert_event(&pool, tm_src, &tickets, &raw("tm-threads"))
        .await
        .unwrap();
    assert!(!joined.created);
    // Both venue-site rows match equally well; the older one wins.
    assert_eq!(joined.event_id, s1.event_id);
    assert_eq!(event_count(&pool).await, 3);

    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn never_merge_override_splits_fuzzy_and_exact_merges() {
    let Some(db) = TestDb::create("never_merge_override_splits_fuzzy_and_exact_merges").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let tm_src = source_id(&pool, "ticketmaster").await;
    let sp_src = source_id(&pool, "serpentine-galleries").await;

    // Fuzzy merge, then split.
    let (tm, sp) = kusama_pair();
    let (tm_raw, sp_raw) = (raw("tm-kusama"), raw("sp-kusama"));
    let a = repo::upsert_event(&pool, tm_src, &tm, &tm_raw)
        .await
        .unwrap();
    let b = repo::upsert_event(&pool, sp_src, &sp, &sp_raw)
        .await
        .unwrap();
    assert_eq!(a.event_id, b.event_id);
    add_override(
        &pool,
        OverrideAction::NeverMerge,
        (tm_src, "tm-kusama"),
        (sp_src, "sp-kusama"),
    )
    .await;
    let b = repo::upsert_event(&pool, sp_src, &sp, &sp_raw)
        .await
        .unwrap();
    assert!(b.created);
    assert_ne!(a.event_id, b.event_id);
    assert_eq!(event_count(&pool).await, 2);
    assert_eq!(link_count(&pool, a.event_id).await, 1);
    for _ in 0..2 {
        let a2 = repo::upsert_event(&pool, tm_src, &tm, &tm_raw)
            .await
            .unwrap();
        let b2 = repo::upsert_event(&pool, sp_src, &sp, &sp_raw)
            .await
            .unwrap();
        assert_eq!((a2.event_id, b2.event_id), (a.event_id, b.event_id));
    }
    assert_eq!(event_count(&pool).await, 2);

    // Exact dedupe_key collision, then split.
    let tm_raw = ticketmaster_talk();
    let tm = ticketmaster::normalise_event(&tm_raw.payload)
        .unwrap()
        .unwrap();
    let sp_raw = serpentine_talk();
    let sp = serpentine::normalise_payload(&sp_raw.payload)
        .unwrap()
        .unwrap();
    assert_eq!(tm.dedupe_key, sp.dedupe_key);
    let a = repo::upsert_event(&pool, tm_src, &tm, &tm_raw)
        .await
        .unwrap();
    let b = repo::upsert_event(&pool, sp_src, &sp, &sp_raw)
        .await
        .unwrap();
    assert_eq!(a.event_id, b.event_id);
    assert_eq!(event_count(&pool).await, 3);
    add_override(
        &pool,
        OverrideAction::NeverMerge,
        (tm_src, &tm_raw.source_event_id),
        (sp_src, &sp_raw.source_event_id),
    )
    .await;
    for _ in 0..2 {
        let a2 = repo::upsert_event(&pool, tm_src, &tm, &tm_raw)
            .await
            .unwrap();
        let b2 = repo::upsert_event(&pool, sp_src, &sp, &sp_raw)
            .await
            .unwrap();
        assert_ne!(a2.event_id, b2.event_id);
    }
    assert_eq!(event_count(&pool).await, 4);
    let keys: Vec<String> =
        sqlx::query_scalar("SELECT dedupe_key FROM events.events WHERE dedupe_key LIKE $1")
            .bind(format!("{}%", tm.dedupe_key))
            .fetch_all(&pool)
            .await
            .unwrap();
    let suffixed = [
        format!("{}|{tm_src}:{}", tm.dedupe_key, tm_raw.source_event_id),
        format!("{}|{sp_src}:{}", sp.dedupe_key, sp_raw.source_event_id),
    ];
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert!(keys.contains(&tm.dedupe_key), "{keys:?}");
    assert!(keys.iter().any(|k| suffixed.contains(k)), "{keys:?}");

    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn force_merge_override_joins_non_matching_listings() {
    let Some(db) = TestDb::create("force_merge_override_joins_non_matching_listings").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let tm_src = source_id(&pool, "ticketmaster").await;
    let sp_src = source_id(&pool, "serpentine-galleries").await;
    let barbican = (51.5201, -0.0955);
    let tm = ev(
        "Noor: Light Installations",
        "Barbican Centre",
        barbican,
        "2026-10-10T18:00:00Z",
        None,
    );
    let sp = ev(
        "Noor",
        "Barbican Centre",
        barbican,
        "2026-10-10T18:00:00Z",
        None,
    );
    let (tm_raw, sp_raw) = (raw("tm-noor"), raw("sp-noor"));

    let a = repo::upsert_event(&pool, tm_src, &tm, &tm_raw)
        .await
        .unwrap();
    let b = repo::upsert_event(&pool, sp_src, &sp, &sp_raw)
        .await
        .unwrap();
    assert_ne!(a.event_id, b.event_id);
    assert_eq!(event_count(&pool).await, 2);

    add_override(
        &pool,
        OverrideAction::ForceMerge,
        (tm_src, "tm-noor"),
        (sp_src, "sp-noor"),
    )
    .await;
    let b = repo::upsert_event(&pool, sp_src, &sp, &sp_raw)
        .await
        .unwrap();
    assert_eq!(b.event_id, a.event_id);
    assert_eq!(event_count(&pool).await, 1);
    assert_eq!(link_count(&pool, a.event_id).await, 2);

    for (src, event, raw) in [(tm_src, &tm, &tm_raw), (sp_src, &sp, &sp_raw)] {
        let o = repo::upsert_event(&pool, src, event, raw).await.unwrap();
        assert_eq!(o.event_id, a.event_id);
    }
    assert_eq!(event_count(&pool).await, 1);

    pool.close().await;
    db.drop_db().await;
}

async fn starts_price_url(
    pool: &PgPool,
    id: uuid::Uuid,
) -> (DateTime<Utc>, Option<Decimal>, Option<String>) {
    let row = repo::get_event(pool, id).await.unwrap().unwrap();
    (row.starts_at, row.price_min, row.url)
}

#[tokio::test]
async fn sources_of_the_same_kind_only_fill_gaps() {
    let Some(db) = TestDb::create("sources_of_the_same_kind_only_fill_gaps").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let sp_src = source_id(&pool, "serpentine-galleries").await;
    let wc_src = source_id(&pool, "whitechapel-gallery").await;
    let tm_src = source_id(&pool, "ticketmaster").await;
    let other_api = repo::upsert_source(
        &pool,
        "other-api",
        SourceKind::Api,
        "https://api.example",
        60,
        true,
    )
    .await
    .unwrap()
    .id;

    // Two venue sites list the same show at different times of one day.
    let at = |starts: &str| {
        ev(
            "Cecilia Vicuña: Living Threads",
            "Whitechapel Gallery",
            (51.5160, -0.0700),
            starts,
            None,
        )
    };
    let first = repo::upsert_event(&pool, sp_src, &at("2026-10-10T10:00:00Z"), &raw("sp"))
        .await
        .unwrap();
    for (src, id, starts) in [
        (wc_src, "wc", "2026-10-10T11:00:00Z"),
        (sp_src, "sp", "2026-10-10T12:00:00Z"),
        (wc_src, "wc", "2026-10-10T13:00:00Z"),
    ] {
        let o = repo::upsert_event(&pool, src, &at(starts), &raw(id))
            .await
            .unwrap();
        assert_eq!(o.event_id, first.event_id);
        let (starts_at, ..) = starts_price_url(&pool, first.event_id).await;
        assert_eq!(
            starts_at,
            t("2026-10-10T10:00:00Z"),
            "after {id} at {starts}"
        );
    }

    // Two APIs list the same event with different prices and ticket URLs.
    let priced = |price: i64, url: &str| {
        let mut e = ev(
            "Noor: Light Installations",
            "Barbican Centre",
            (51.5201, -0.0955),
            "2026-10-11T18:00:00Z",
            None,
        );
        e.price = Price {
            is_free: false,
            min: Some(Decimal::from(price)),
            max: Some(Decimal::from(price)),
            currency: Some("GBP".into()),
        };
        e.url = Some(url.into());
        e
    };
    let first = repo::upsert_event(&pool, tm_src, &priced(20, "https://tm/a"), &raw("tm"))
        .await
        .unwrap();
    for (src, id, price, url) in [
        (other_api, "other", 30, "https://other/b"),
        (tm_src, "tm", 25, "https://tm/c"),
        (other_api, "other", 35, "https://other/d"),
    ] {
        let o = repo::upsert_event(&pool, src, &priced(price, url), &raw(id))
            .await
            .unwrap();
        assert_eq!(o.event_id, first.event_id);
        let (_, price_min, url) = starts_price_url(&pool, first.event_id).await;
        assert_eq!(
            (price_min, url.as_deref()),
            (Some(Decimal::from(20)), Some("https://tm/a")),
            "after {id}"
        );
    }

    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn a_later_start_drops_an_end_that_would_precede_it() {
    let Some(db) = TestDb::create("a_later_start_drops_an_end_that_would_precede_it").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let tm_src = source_id(&pool, "ticketmaster").await;
    let sp_src = source_id(&pool, "serpentine-galleries").await;
    let venue = (51.5045, -0.1751);
    let ends_at = |id: uuid::Uuid| {
        let pool = &pool;
        async move { repo::get_event(pool, id).await.unwrap().unwrap().ends_at }
    };

    // Sole owner moves its start past the end it reported before.
    let show = |starts: &str, ends: Option<&str>| {
        ev("Soto: Pénétrable", "Serpentine South", venue, starts, ends)
    };
    let a = repo::upsert_event(
        &pool,
        sp_src,
        &show("2026-10-01T10:00:00Z", Some("2026-10-05T18:00:00Z")),
        &raw("sp-soto"),
    )
    .await
    .unwrap();
    let a2 = repo::upsert_event(
        &pool,
        sp_src,
        &show("2026-10-20T10:00:00Z", None),
        &raw("sp-soto"),
    )
    .await
    .unwrap();
    assert_eq!(a2.event_id, a.event_id);
    assert_eq!(ends_at(a.event_id).await, None);

    // The venue site's later start wins over the API's earlier end, and the
    // API's end does not come back on its next run.
    let talk = |starts: &str, ends: Option<&str>| {
        ev(
            "Park Nights: Talk",
            "Serpentine Pavilion",
            venue,
            starts,
            ends,
        )
    };
    let tm_talk = talk("2026-10-12T10:00:00Z", Some("2026-10-12T12:00:00Z"));
    let b = repo::upsert_event(&pool, tm_src, &tm_talk, &raw("tm-talk"))
        .await
        .unwrap();
    let b2 = repo::upsert_event(
        &pool,
        sp_src,
        &talk("2026-10-12T18:00:00Z", None),
        &raw("sp-talk"),
    )
    .await
    .unwrap();
    assert_eq!(b2.event_id, b.event_id);
    assert_eq!(ends_at(b.event_id).await, None);
    let b3 = repo::upsert_event(&pool, tm_src, &tm_talk, &raw("tm-talk"))
        .await
        .unwrap();
    assert_eq!(b3.event_id, b.event_id);
    let row = repo::get_event(&pool, b.event_id).await.unwrap().unwrap();
    assert_eq!(
        (row.starts_at, row.ends_at),
        (t("2026-10-12T18:00:00Z"), None)
    );

    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn fuzzy_merge_prefers_the_closest_title_over_the_oldest() {
    let Some(db) = TestDb::create("fuzzy_merge_prefers_the_closest_title_over_the_oldest").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let tm_src = source_id(&pool, "ticketmaster").await;
    let sp_src = source_id(&pool, "serpentine-galleries").await;
    let at = |title: &str| ev(title, "Tate Modern", TATE, "2026-10-10T18:00:00Z", None);

    let older = at("Yayoi Kusama: Infinity Mirror Rooms");
    let newer = at("Yayoi Kusama - Infinity Room");
    let incoming = at("Yayoi Kusama: Infinity Rooms");
    let score = |e: &NewEvent| {
        matching::match_score(&MatchInput::from(&incoming), &MatchInput::from(e))
            .expect("both candidates match")
    };
    assert!(score(&newer).dice > score(&older).dice);
    assert_ne!(incoming.dedupe_key, older.dedupe_key);
    assert_ne!(incoming.dedupe_key, newer.dedupe_key);

    let o = repo::upsert_event(&pool, sp_src, &older, &raw("sp-older"))
        .await
        .unwrap();
    let n = repo::upsert_event(&pool, sp_src, &newer, &raw("sp-newer"))
        .await
        .unwrap();
    assert!(n.created);
    assert_ne!(o.event_id, n.event_id);

    let joined = repo::upsert_event(&pool, tm_src, &incoming, &raw("tm"))
        .await
        .unwrap();
    assert_eq!(joined.event_id, n.event_id);
    assert_eq!(event_count(&pool).await, 2);

    pool.close().await;
    db.drop_db().await;
}
