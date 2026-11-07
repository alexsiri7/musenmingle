//! The William Morris Society identity migration (#403) drops the dead
//! `/events/<slug>/` links stored under the old ids, so the next run links
//! an upcoming event under its ticket-shop URL instead.

mod common;

use chrono::{DateTime, Duration, Utc};
use common::{TestDb, fixture};
use musenmingle::db;
use musenmingle::normalise::dedupe_key;
use musenmingle::repo;
use musenmingle::sources::Source;
use musenmingle::sources::william_morris_society::{
    WilliamMorrisSociety, card_event, parse_listing,
};
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

/// The last migration before `20261107100001_william_morris_society_ticket_links.sql`.
const BEFORE: i64 = 20261020100001;

const SITE: &str = "https://williammorrissociety.org";
const LATES_ID: &str = "williammorrissociety.digitickets.co.uk/event-tickets/71493?catID=64741";

/// An event stored by the old scraper, linked as `events/<slug>`.
async fn insert_old(pool: &PgPool, title: &str, starts_at: DateTime<Utc>, slug: &str) -> Uuid {
    let url = format!("{SITE}/events/{slug}/");
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO events.events (title, venue_name, starts_at, category, dedupe_key, url)
         VALUES ($1, 'William Morris Society', $2, 'community', $3, $4)
         RETURNING id",
    )
    .bind(title)
    .bind(starts_at)
    .bind(dedupe_key(title, starts_at, Some("William Morris Society")))
    .bind(&url)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO events.event_sources (event_id, source_id, source_event_id, source_url, raw)
         SELECT $1, id, $2, $3, '{}' FROM events.sources WHERE key = 'william-morris-society'",
    )
    .bind(id)
    .bind(format!("events/{slug}"))
    .bind(&url)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn url_of(pool: &PgPool, id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT url FROM events.events WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn links_of(pool: &PgPool, id: Uuid) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT source_event_id, source_url FROM events.event_sources
          WHERE event_id = $1 ORDER BY source_event_id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn old_links_give_way_to_the_ticket_shop() {
    let Some(tdb) = TestDb::create("william_morris_society_ticket_links").await else {
        return;
    };
    let pool = db::connect(&tdb.url()).await.unwrap();
    db::ensure_schema(&pool).await.unwrap();
    db::migrator().run_to(BEFORE, &pool).await.unwrap();

    // The fixture's dates are fixed; the migration tells upcoming from past
    // by the clock, so the listing is moved a month ahead.
    let s = WilliamMorrisSociety::new(SITE.parse().unwrap());
    let page: Url = format!("{SITE}/whats-on/").parse().unwrap();
    let raw = parse_listing(&fixture("scrapers/william-morris-society/whats-on.html"))
        .cards
        .iter()
        .map(|c| card_event(c, &page).unwrap())
        .find(|r| r.source_event_id == LATES_ID)
        .unwrap();
    let mut lates = s.normalise(&raw).unwrap().unwrap();
    let shift = Utc::now() + Duration::days(30) - lates.starts_at;
    lates.starts_at += shift;
    lates.ends_at = lates.ends_at.map(|e| e + shift);
    lates.dedupe_key = dedupe_key(&lates.title, lates.starts_at, lates.venue_name.as_deref());

    let upcoming = insert_old(
        &pool,
        &lates.title,
        lates.starts_at,
        "morris-by-moonlight-museum-lates-2",
    )
    .await;
    let past = insert_old(
        &pool,
        "Embroidery Workshop",
        Utc::now() - Duration::days(30),
        "embroidery-workshop",
    )
    .await;

    db::migrate(&pool).await.unwrap();

    assert_eq!(url_of(&pool, upcoming).await, None);
    assert_eq!(links_of(&pool, upcoming).await, vec![]);
    assert_eq!(url_of(&pool, past).await, None);
    assert_eq!(
        links_of(&pool, past).await,
        vec![("events/embroidery-workshop".to_string(), None)]
    );

    let src = repo::source_by_key(&pool, "william-morris-society")
        .await
        .unwrap()
        .unwrap();
    let out = repo::upsert_event(&pool, src.id, &lates, &raw)
        .await
        .unwrap();
    assert_eq!(out.event_id, upcoming);
    let shop = format!("https://{LATES_ID}");
    assert_eq!(url_of(&pool, upcoming).await, Some(shop.clone()));
    assert_eq!(
        links_of(&pool, upcoming).await,
        vec![(LATES_ID.to_string(), Some(shop))]
    );

    pool.close().await;
    tdb.drop_db().await;
}
