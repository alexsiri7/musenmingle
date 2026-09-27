//! Issue #208: AI enrichment sees a listing's full page text while the ingest
//! run holds it, but the text never reaches the database (only its hash),
//! and a changed page makes the event due again (not every run).

mod common;

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use common::TestDb;
use musenmingle::enrich::input::PageText;
use musenmingle::enrich::requesty::Requesty;
use musenmingle::enrich::{self, EnrichConfig, Enricher, PageTexts};
use musenmingle::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use musenmingle::notify::Ntfy;
use musenmingle::repo;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

const SENTINEL: &str = "Zephyrine Quillfeather";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap()
}

/// A description well over the 300-character excerpt, with `tail` (and the
/// sentinel speaker) only after the cut.
fn description(tail: &str) -> String {
    let head = "A group show of paintings and drawings about rivers and the city. ".repeat(6);
    format!("{head}The talk is given by {SENTINEL}. {tail}")
}

fn event(title: &str, desc: &str) -> NewEvent {
    NewEvent {
        sessions: Vec::new(),
        title: title.into(),
        description: Some(desc.into()),
        venue_name: Some("Gallery".into()),
        address: None,
        lat: None,
        lng: None,
        starts_at: Utc.with_ymd_and_hms(2026, 10, 10, 18, 0, 0).unwrap(),
        ends_at: None,
        all_day: false,
        price: Price::default(),
        url: None,
        image_url: None,
        category: Category::Talk,
        tags: Vec::new(),
        dedupe_key: title.to_lowercase(),
    }
}

async fn ingest(pool: &PgPool, source: i64, e: &NewEvent) -> (Uuid, Option<PageText>) {
    let raw = RawEvent {
        source_event_id: e.title.clone(),
        source_url: None,
        payload: json!({}),
    };
    let (o, page) = repo::upsert_listing(pool, source, e, &raw).await.unwrap();
    (o.event_id, page)
}

async fn mount_chat(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|req: &MockRequest| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let events: Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let results: Vec<Value> = events["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    // The speaker is only in the page text: grounding must
                    // accept it (and would reject it without the text).
                    let artists = if e.get("page_text").is_some() {
                        json!([SENTINEL])
                    } else {
                        json!([])
                    };
                    json!({
                        "id": e["id"], "medium_tags": ["painting"], "format_tags": ["talk"],
                        "good_for": [], "vibe_tags": [], "artists": artists,
                        "is_opening": false, "opening_evidence": null, "grounding": "listing",
                        "whats_cool": "A talk on painting rivers and the city.",
                        "one_liner": "Talk on paintings of rivers", "confidence": 0.8
                    })
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "role": "assistant",
                                 "content": json!({ "results": results }).to_string() },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 1000, "completion_tokens": 200 }
            }))
        })
        .mount(server)
        .await;
}

fn enricher(requesty: &MockServer, ntfy: &MockServer) -> Enricher {
    Enricher {
        client: Requesty::new(&requesty.uri(), "test-key").unwrap(),
        notifier: Box::new(Ntfy::new(&ntfy.uri(), "topic").unwrap()),
        config: EnrichConfig {
            embed_model: None,
            call_timeout: Duration::from_secs(5),
            run_budget: Duration::from_secs(60),
            ..Default::default()
        },
    }
}

/// Every text-like column of every table in the `events` schema that
/// contains `needle`, as "table.column".
async fn columns_containing(pool: &PgPool, needle: &str) -> Vec<String> {
    let cols: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.table_name::text, c.column_name::text
           FROM information_schema.columns c
           JOIN information_schema.tables t
             ON t.table_schema = c.table_schema AND t.table_name = c.table_name
          WHERE c.table_schema = 'events' AND t.table_type = 'BASE TABLE'
            AND (c.data_type IN ('text', 'character varying', 'json', 'jsonb')
                 OR c.udt_name IN ('_text', '_varchar'))",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(cols.len() > 20, "the scan sees the schema");
    let mut hits = Vec::new();
    for (t, c) in cols {
        let sql = format!(
            "SELECT count(*) FROM events.\"{t}\" WHERE \"{c}\"::text LIKE '%' || $1 || '%'"
        );
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(needle)
            .fetch_one(pool)
            .await
            .unwrap();
        if n > 0 {
            hits.push(format!("{t}.{c}"));
        }
    }
    hits
}

async fn page_hash(pool: &PgPool, id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT page_text_hash FROM events.events WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn full_text_reaches_the_model_but_never_the_database() {
    let Some(db) = TestDb::create("full_text_reaches_the_model_but_never_the_database").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::upsert_source(
        &pool,
        "page-text-test",
        SourceKind::Scraper,
        "https://example.org",
        60,
        true,
    )
    .await
    .unwrap();
    let (id, page) = ingest(
        &pool,
        src.id,
        &event("River talk", &description("Open daily.")),
    )
    .await;
    let page = page.expect("the listing's text goes to enrichment");
    assert!(page.as_str().contains(SENTINEL));
    assert_eq!(page_hash(&pool, id).await, Some(page.hash()));

    let (requesty, ntfy) = (MockServer::start().await, MockServer::start().await);
    mount_chat(&requesty).await;
    let e = enricher(&requesty, &ntfy);
    let pages: PageTexts = [(id, page.clone())].into();
    let r = e.run(&pool, now(), &pages).await.unwrap();
    assert_eq!((r.queued, r.enriched, r.failed), (1, 1, 0), "{r:?}");

    let sent = requesty.received_requests().await.unwrap();
    assert_eq!(sent.len(), 1);
    let body = String::from_utf8(sent[0].body.clone()).unwrap();
    assert!(body.contains("page_text"), "the model sees the page text");

    // The text (past the excerpt) is nowhere in the database: not in the
    // event, its listing, the enrichment, the failures or the call ledger.
    // (The artist name the model returned is stored, as before, in the
    // enrichment output; the sentinel check uses the text after it.)
    assert_eq!(
        columns_containing(&pool, "Open daily.").await,
        Vec::<String>::new()
    );
    let (stored_hash,): (Option<String>,) =
        sqlx::query_as("SELECT page_text_hash FROM events.enrichments WHERE event_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_hash, Some(page.hash()));
    db.drop_db().await;
}

#[tokio::test]
async fn a_changed_page_makes_the_event_due_but_an_unchanged_one_does_not() {
    let Some(db) =
        TestDb::create("a_changed_page_makes_the_event_due_but_an_unchanged_one_does_not").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::upsert_source(
        &pool,
        "page-text-test",
        SourceKind::Scraper,
        "https://example.org",
        60,
        true,
    )
    .await
    .unwrap();
    let (requesty, ntfy) = (MockServer::start().await, MockServer::start().await);
    mount_chat(&requesty).await;
    let e = enricher(&requesty, &ntfy);

    let v1 = event("River talk", &description("Open Wed–Sun 11–6."));
    let (id, page) = ingest(&pool, src.id, &v1).await;
    let pages: PageTexts = [(id, page.unwrap())].into();
    assert_eq!(e.run(&pool, now(), &pages).await.unwrap().enriched, 1);

    // Re-scraped, same text: nothing to do.
    let (_, page) = ingest(&pool, src.id, &v1).await;
    let pages: PageTexts = [(id, page.unwrap())].into();
    assert_eq!(e.run(&pool, now(), &pages).await.unwrap().queued, 0);

    // The page changes after the excerpt: new hash, same stored facts, so
    // the note is not withdrawn...
    let before = page_hash(&pool, id).await;
    let v2 = event("River talk", &description("Open Thu–Sun 12–6."));
    let (_, page) = ingest(&pool, src.id, &v2).await;
    let page = page.unwrap();
    assert_ne!(page_hash(&pool, id).await, before, "hash follows the text");
    let s = enrich::sync(&pool, now()).await.unwrap();
    assert_eq!(s.stale_cleared, 0);
    // ...and a run without the text (another tick) waits for it...
    assert_eq!(
        e.run(&pool, now(), &PageTexts::new()).await.unwrap().queued,
        0
    );
    // ...while the run that scraped it re-enriches.
    let pages: PageTexts = [(id, page)].into();
    assert_eq!(e.run(&pool, now(), &pages).await.unwrap().enriched, 1);
    assert_eq!(requesty.received_requests().await.unwrap().len(), 2);

    // An event without a note doesn't wait for its text: it gets an
    // excerpt-only note now (upgraded when a run has the text).
    let (other, _) = ingest(&pool, src.id, &event("Canal talk", &description("x."))).await;
    let r = e.run(&pool, now(), &PageTexts::new()).await.unwrap();
    assert_eq!((r.queued, r.enriched), (1, 1), "{r:?}");
    assert_eq!(page_hash(&pool, other).await.map(|h| h.len()), Some(64));
    db.drop_db().await;
}

#[tokio::test]
async fn a_merged_event_follows_one_listing_s_text() {
    let Some(db) = TestDb::create("a_merged_event_follows_one_listing_s_text").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let venue = repo::upsert_source(
        &pool,
        "venue-site",
        SourceKind::Scraper,
        "https://venue.example.org",
        60,
        true,
    )
    .await
    .unwrap();
    let api = repo::upsert_source(
        &pool,
        "ticket-api",
        SourceKind::Api,
        "https://api.example.org",
        60,
        true,
    )
    .await
    .unwrap();
    let a = event("River talk", &description("Venue's own words."));
    let b = event(
        "River talk",
        &format!("Tickets on sale now. {}", description("Seller words.")),
    );
    let (id, _) = ingest(&pool, venue.id, &a).await;
    let first = page_hash(&pool, id).await;
    for _ in 0..2 {
        let (id_b, page_b) = ingest(&pool, api.id, &b).await;
        assert_eq!(id_b, id, "merged");
        assert!(page_b.is_none(), "not the text behind the stored excerpt");
        assert_eq!(page_hash(&pool, id).await, first, "stable across sources");
        let (_, page_a) = ingest(&pool, venue.id, &a).await;
        assert!(page_a.is_some());
        assert_eq!(page_hash(&pool, id).await, first);
    }
    db.drop_db().await;
}
