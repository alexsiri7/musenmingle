//! AI enrichment + embeddings against a real database and a wiremock
//! Requesty (chat completions + embeddings) and ntfy: batching and id
//! mapping, the single retry, give-ups, input-hash re-enrichment, spend
//! caps, the credits-exhausted alert, embed-after-enrich ordering, "More like
//! this", the tag filters/facets and the labelled AI note on the pages.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::SuggestionConfig;
use musenmingle::enrich::requesty::Requesty;
use musenmingle::enrich::{EnrichConfig, Enricher, embed, store};
use musenmingle::notify::Ntfy;
use musenmingle::suggestions::Suggestions;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap()
}

/// Insert an upcoming event listed by `source`; returns its id.
async fn event(pool: &PgPool, title: &str, source: &str, starts: DateTime<Utc>) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO events.events (title, description, venue_name, starts_at, category, dedupe_key)
         VALUES ($1, 'Paintings and drawings.', 'Gallery', $2, 'exhibition', $1) RETURNING id",
    )
    .bind(title)
    .bind(starts)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO events.event_sources (event_id, source_id, source_event_id, raw)
         SELECT $1, id, $2, '{}' FROM events.sources WHERE key = $3",
    )
    .bind(id)
    .bind(title)
    .bind(source)
    .execute(pool)
    .await
    .unwrap();
    id
}

fn good(id: &str, title: &str) -> Value {
    json!({
        "id": id,
        "medium_tags": ["painting"],
        "format_tags": [],
        "good_for": ["solo"],
        "vibe_tags": [],
        "artists": [],
        "is_opening": false,
        "opening_evidence": null,
        "grounding": "listing",
        "whats_cool": format!("Notes on {title}."),
        "one_liner": format!("About {title}"),
        "confidence": 0.8
    })
}

fn bad(id: &str) -> Value {
    let mut v = good(id, "x");
    v["medium_tags"] = json!(["pottery"]);
    v
}

/// The `(batch id, title)` pairs a chat request carries, and whether it is
/// the retry (it has the reminder message).
fn batch_of(req: &MockRequest) -> (Vec<(String, String)>, bool) {
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    let msgs = body["messages"].as_array().unwrap();
    let events: Value = serde_json::from_str(msgs[1]["content"].as_str().unwrap()).unwrap();
    let pairs = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["id"].as_str().unwrap().to_string(),
                e["title"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    (pairs, msgs.len() == 3)
}

fn chat_reply(results: Vec<Value>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "message": { "role": "assistant", "content": json!({ "results": results }).to_string() },
            "finish_reason": "stop"
        }],
        // $4/M in + $20/M out = $0.008 per call.
        "usage": {
            "prompt_tokens": 1000, "completion_tokens": 200,
            "prompt_tokens_details": { "cached_tokens": 0, "caching_tokens": 0 }
        }
    }))
}

/// Answers every chat call with valid results (reversed, to prove the id
/// mapping), except titles starting "Bad", which get an off-vocabulary tag.
async fn mount_chat(server: &MockServer, calls: Arc<AtomicUsize>) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(move |req: &MockRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            let (pairs, _retry) = batch_of(req);
            let results = pairs
                .iter()
                .rev()
                .map(|(id, title)| {
                    if title.starts_with("Bad") {
                        bad(id)
                    } else {
                        good(id, title)
                    }
                })
                .collect();
            chat_reply(results)
        })
        .mount(server)
        .await;
}

/// Embeddings: element 0 is the input's length, element 1 its position;
/// the data comes back reversed with explicit indexes.
async fn mount_embeddings(server: &MockServer, calls: Arc<AtomicUsize>) {
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &MockRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let inputs = body["input"].as_array().unwrap();
            let data: Vec<Value> = inputs
                .iter()
                .enumerate()
                .rev()
                .map(|(i, t)| {
                    let mut v = vec![0.0f32; embed::EMBED_DIMS];
                    v[0] = t.as_str().unwrap().len() as f32;
                    v[1] = i as f32 + 1.0;
                    json!({ "index": i, "embedding": v })
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({
                "data": data, "usage": { "prompt_tokens": 100 }
            }))
        })
        .mount(server)
        .await;
}

fn enricher(requesty: &MockServer, ntfy: &MockServer, config: EnrichConfig) -> Enricher {
    Enricher {
        client: Requesty::new(&requesty.uri(), "test-key").unwrap(),
        notifier: Box::new(Ntfy::new(&ntfy.uri(), "topic").unwrap()),
        config,
    }
}

fn config() -> EnrichConfig {
    EnrichConfig {
        batch_size: 5,
        embed_model: None,
        call_timeout: Duration::from_secs(5),
        run_budget: Duration::from_secs(60),
        ..Default::default()
    }
}

async fn spend(pool: &PgPool) -> (i64, Decimal) {
    sqlx::query_as("SELECT count(*), COALESCE(sum(cost_usd), 0) FROM events.enrichment_calls")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn batches_map_results_back_by_id_and_record_spend() {
    let Some(db) = TestDb::create("batches_map_results_back_by_id_and_record_spend").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let mut ids = Vec::new();
    for i in 0..12 {
        ids.push(event(&pool, &format!("Event {i:02}"), "barbican", now()).await);
    }
    let (requesty, ntfy) = (MockServer::start().await, MockServer::start().await);
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&requesty, calls.clone()).await;

    let e = enricher(&requesty, &ntfy, config());
    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!(
        (r.queued, r.enriched, r.failed, r.calls),
        (12, 12, 0, 3),
        "{r:?}"
    );
    assert_eq!(r.cost_usd, Decimal::new(24, 3));
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    for (i, id) in ids.iter().enumerate() {
        let (cool, medium, model): (Option<String>, Vec<String>, Option<String>) = sqlx::query_as(
            "SELECT whats_cool, medium_tags, ai_model FROM events.events WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            cool.as_deref(),
            Some(format!("Notes on Event {i:02}.").as_str())
        );
        assert_eq!(medium, ["painting"]);
        assert_eq!(model.as_deref(), Some("anthropic/claude-opus-5-5"));
    }
    let (rows, total) = spend(&pool).await;
    assert_eq!((rows, total), (3, Decimal::new(24, 3)));
    let per_event: Decimal = sqlx::query_scalar("SELECT cost_usd FROM events.enrichments LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(per_event, Decimal::new(16, 4), "$0.008 over a batch of 5");

    // Nothing changed: nothing is re-sent.
    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!((r.queued, r.calls), (0, 0));
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn invalid_results_are_retried_once_then_given_up_until_the_input_changes() {
    let Some(db) = TestDb::create("invalid_results_are_retried_once").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let good_id = event(&pool, "Good show", "barbican", now()).await;
    let bad_id = event(&pool, "Bad show", "barbican", now()).await;
    let (requesty, ntfy) = (MockServer::start().await, MockServer::start().await);
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&requesty, calls.clone()).await;
    let e = enricher(&requesty, &ntfy, config());

    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!((r.enriched, r.failed, r.calls), (1, 1, 2), "{r:?}");
    // The retry carried the validator's complaint and only the bad event.
    let reqs = requesty.received_requests().await.unwrap();
    let (retry_batch, is_retry) = batch_of(&reqs[1]);
    assert!(is_retry);
    assert_eq!(retry_batch, [("e1".to_string(), "Bad show".to_string())]);
    let body: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(
        body["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("\"pottery\" is not in the vocabulary")
    );
    let failed: (String, i32) = sqlx::query_as(
        "SELECT error, prompt_version FROM events.enrichment_failures WHERE event_id = $1",
    )
    .bind(bad_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(failed.0.contains("pottery"), "{failed:?}");
    // No partial write for the bad event.
    let (cool, medium): (Option<String>, Vec<String>) =
        sqlx::query_as("SELECT whats_cool, medium_tags FROM events.events WHERE id = $1")
            .bind(bad_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((cool, medium), (None, vec![]));
    let good_enriched: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM events.enrichments WHERE event_id = $1)")
            .bind(good_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(good_enriched);

    // Given up: not re-sent on the next tick ...
    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!((r.queued, r.calls), (0, 0));
    // ... until its input changes.
    sqlx::query("UPDATE events.events SET title = 'Better show' WHERE id = $1")
        .bind(bad_id)
        .execute(&pool)
        .await
        .unwrap();
    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!((r.queued, r.enriched, r.calls), (1, 1, 1), "{r:?}");
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn an_input_change_withdraws_the_note_and_re_enriches() {
    let Some(db) = TestDb::create("an_input_change_withdraws_the_note").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let id = event(&pool, "Old title", "design-museum", now()).await;
    let (requesty, ntfy) = (MockServer::start().await, MockServer::start().await);
    mount_chat(&requesty, Arc::new(AtomicUsize::new(0))).await;
    let e = enricher(&requesty, &ntfy, config());
    musenmingle::enrich::sync(&pool, now()).await.unwrap();
    let medium: Vec<String> =
        sqlx::query_scalar("SELECT medium_tags FROM events.events WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(medium, ["design"], "the source's default tag, before AI");

    e.run(&pool, now()).await.unwrap();
    let row = || async {
        sqlx::query_as::<_, (Option<String>, Vec<String>, Option<String>)>(
            "SELECT whats_cool, medium_tags, ai_grounding FROM events.events WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let (cool, medium, _) = row().await;
    assert_eq!(cool.as_deref(), Some("Notes on Old title."));
    assert_eq!(medium, ["design", "painting"], "AI tags plus the default");
    let old_hash: String =
        sqlx::query_scalar("SELECT input_hash FROM events.enrichments WHERE event_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();

    // The venue's excerpt goes (e.g. a content-policy change): the note it
    // was written from is withdrawn at once ...
    sqlx::query("UPDATE events.events SET description = NULL, title = 'New title' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let s = musenmingle::enrich::sync(&pool, now()).await.unwrap();
    assert_eq!(s.stale_cleared, 1);
    assert_eq!(row().await, (None, vec!["design".to_string()], None));
    // ... and rewritten from the new facts.
    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!(r.enriched, 1);
    assert_eq!(row().await.0.as_deref(), Some("Notes on New title."));
    let new_hash: String =
        sqlx::query_scalar("SELECT input_hash FROM events.enrichments WHERE event_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(old_hash, new_hash);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn spend_caps_stop_calls_before_they_are_made() {
    let Some(db) = TestDb::create("spend_caps_stop_calls_before_they_are_made").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    for i in 0..10 {
        event(&pool, &format!("Event {i:02}"), "barbican", now()).await;
    }
    let (requesty, ntfy) = (MockServer::start().await, MockServer::start().await);
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&requesty, calls.clone()).await;

    // $0.99 already spent today (London) against a $1 daily cap.
    let rec = store::CallRecord {
        model: "anthropic/claude-opus-5-5".into(),
        cost_usd: Decimal::new(99, 2),
        ok: true,
        ..Default::default()
    };
    store::record_call(&pool, &rec, now() - chrono::Duration::hours(2))
        .await
        .unwrap();
    let e = enricher(&requesty, &ntfy, config());
    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        r.stopped.as_deref().unwrap().starts_with("daily cap"),
        "{r:?}"
    );

    // Yesterday's spend does not count (London day), but the per-run cap
    // stops the second call: it allows exactly one pessimistic estimate.
    sqlx::query("UPDATE events.enrichment_calls SET called_at = $1")
        .bind(now() - chrono::Duration::days(1))
        .execute(&pool)
        .await
        .unwrap();
    let cands = store::candidates(&pool, now()).await.unwrap();
    let price = store::model_price(&pool, "anthropic/claude-opus-5-5")
        .await
        .unwrap()
        .unwrap();
    let batch: Vec<(String, &musenmingle::enrich::input::EventFacts)> = cands[..5]
        .iter()
        .enumerate()
        .map(|(i, c)| (format!("e{}", i + 1), &c.facts))
        .collect();
    let body = musenmingle::enrich::chat_body(&config(), &batch, None, true);
    let chars = body["messages"].to_string().len() + body["response_format"].to_string().len();
    let estimate =
        musenmingle::enrich::estimate_cost(chars, musenmingle::enrich::max_tokens(5), &price);
    let e = enricher(
        &requesty,
        &ntfy,
        EnrichConfig {
            run_cap_usd: estimate + Decimal::new(1, 3),
            ..config()
        },
    );
    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(r.enriched, 5);
    assert!(
        r.stopped.as_deref().unwrap().starts_with("run cap"),
        "{r:?}"
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn a_model_that_keeps_data_is_never_called() {
    let Some(db) = TestDb::create("a_model_that_keeps_data_is_never_called").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    event(&pool, "Event", "barbican", now()).await;
    let (requesty, ntfy) = (MockServer::start().await, MockServer::start().await);
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&requesty, calls.clone()).await;
    for model in ["anthropic/claude-sonnet-5", "not/in-the-price-table"] {
        let e = enricher(
            &requesty,
            &ntfy,
            EnrichConfig {
                model: model.into(),
                ..config()
            },
        );
        let r = e.run(&pool, now()).await.unwrap();
        assert!(r.stopped.is_some(), "{model}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    pool.close().await;
    db.drop_db().await;
}

async fn ntfy_messages(ntfy: &MockServer) -> Vec<(String, String, String)> {
    ntfy.received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| {
            let h = |n: &str| {
                r.headers
                    .get(n)
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default()
            };
            (
                h("title"),
                h("priority"),
                String::from_utf8(r.body.clone()).unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn exhausted_credits_pause_notify_once_a_day_and_recover() {
    let Some(db) = TestDb::create("exhausted_credits_pause_notify").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    for i in 0..7 {
        event(&pool, &format!("Event {i}"), "barbican", now()).await;
    }
    let (requesty, ntfy) = (MockServer::start().await, MockServer::start().await);
    Mock::given(method("POST"))
        .and(path("/topic"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&ntfy)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(402)
                .set_body_json(json!({ "error": { "message": "organization balance exhausted" } })),
        )
        .mount(&requesty)
        .await;
    let e = enricher(
        &requesty,
        &ntfy,
        EnrichConfig {
            embed_model: Some("openai/text-embedding-3-small".into()),
            ..config()
        },
    );

    // Stops after the first refused call (no retry loop, no embeddings).
    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!(r.calls, 1);
    assert_eq!(r.stopped.as_deref(), Some("Requesty credits exhausted"));
    assert_eq!(requesty.received_requests().await.unwrap().len(), 1);
    let sent = ntfy_messages(&ntfy).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "Muse & Mingle: Requesty credits exhausted");
    assert_eq!(sent[0].1, "high");
    assert_eq!(
        sent[0].2,
        "AI enrichment paused since 1 Oct 2026 10:00 BST; top up at https://app.requesty.ai — it resumes automatically."
    );
    let state = store::alert_state(&pool, "requesty_credits")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.active_since, Some(now()));

    // Later the same day: still exhausted, no second message.
    e.run(&pool, now() + chrono::Duration::hours(3))
        .await
        .unwrap();
    assert_eq!(ntfy_messages(&ntfy).await.len(), 1);
    // The next London day: one reminder, still "since" the first time.
    e.run(&pool, now() + chrono::Duration::days(1))
        .await
        .unwrap();
    let sent = ntfy_messages(&ntfy).await;
    assert_eq!(sent.len(), 2);
    assert!(sent[1].2.contains("since 1 Oct 2026 10:00 BST"), "{sent:?}");

    // Topped up: the next call succeeds, the state clears, one OK message.
    requesty.reset().await;
    mount_chat(&requesty, Arc::new(AtomicUsize::new(0))).await;
    let r = e
        .run(&pool, now() + chrono::Duration::days(1))
        .await
        .unwrap();
    assert_eq!(r.enriched, 7, "{r:?}");
    let sent = ntfy_messages(&ntfy).await;
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[2].0, "Muse & Mingle: Requesty credits OK again");
    assert_eq!(sent[2].2, "Requesty credits OK again — enrichment resumed.");
    let state = store::alert_state(&pool, "requesty_credits")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.active_since, None);
    // Quiet afterwards.
    e.run(&pool, now() + chrono::Duration::days(2))
        .await
        .unwrap();
    assert_eq!(ntfy_messages(&ntfy).await.len(), 3);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn embeddings_follow_enrichment_and_track_the_text() {
    let Some(db) = TestDb::create("embeddings_follow_enrichment").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    if !common::has_pgvector(&pool).await || !store::embeddings_available(&pool).await.unwrap() {
        common::notice("SKIPPING embeddings_follow_enrichment: no pgvector on the test server");
        pool.close().await;
        db.drop_db().await;
        return;
    }
    let good_id = event(&pool, "Good show", "barbican", now()).await;
    let bad_id = event(&pool, "Bad show", "barbican", now()).await;
    let past_id = event(
        &pool,
        "Past show",
        "barbican",
        now() - chrono::Duration::days(5),
    )
    .await;
    let (requesty, ntfy) = (MockServer::start().await, MockServer::start().await);
    let chat_calls = Arc::new(AtomicUsize::new(0));
    let embed_calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&requesty, chat_calls.clone()).await;
    mount_embeddings(&requesty, embed_calls.clone()).await;
    let with_embeddings = EnrichConfig {
        embed_model: Some("openai/text-embedding-3-small".into()),
        ..config()
    };

    // Nothing enriched yet and no budget: nothing is embedded either (wait
    // for the enrichment rather than embed facts twice).
    let broke = enricher(
        &requesty,
        &ntfy,
        EnrichConfig {
            daily_cap_usd: Decimal::ZERO,
            ..with_embeddings.clone()
        },
    );
    let r = broke.run(&pool, now()).await.unwrap();
    assert_eq!((r.calls, r.embedded), (0, 0), "{r:?}");

    let e = enricher(&requesty, &ntfy, with_embeddings);
    let r = e.run(&pool, now()).await.unwrap();
    // Good enriched; Bad given up after its retry -> embedded from facts only.
    assert_eq!((r.enriched, r.failed, r.embedded), (1, 1, 2), "{r:?}");
    assert_eq!(embed_calls.load(Ordering::SeqCst), 1);
    let rows: Vec<(Uuid, bool, String, f32)> = sqlx::query_as(
        "SELECT event_id, facts_only, text_hash, (embedding::real[])[1]
         FROM events.event_embeddings ORDER BY facts_only",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|r| r.0 != past_id),
        "past events are not embedded"
    );
    let cands = store::candidates(&pool, now()).await.unwrap();
    for (id, facts_only, hash, first) in &rows {
        let c = cands.iter().find(|c| c.facts.id == *id).unwrap();
        let text = embed::embed_text(&c.facts, c.enriched_output.as_ref().map(|o| &o.0));
        assert_eq!(
            *hash,
            embed::text_hash(&text),
            "stored under the right event"
        );
        assert_eq!(*first as usize, text.len(), "vector mapped back by index");
        assert_eq!(*facts_only, *id == bad_id);
        if *id == good_id {
            assert!(text.contains("Media: painting") && text.contains("Notes on Good show."));
        }
    }

    // Unchanged: no new embedding calls.
    e.run(&pool, now()).await.unwrap();
    assert_eq!(embed_calls.load(Ordering::SeqCst), 1);
    // A changed title: re-enriched, then re-embedded with the new text.
    sqlx::query("UPDATE events.events SET title = 'Good show, again' WHERE id = $1")
        .bind(good_id)
        .execute(&pool)
        .await
        .unwrap();
    musenmingle::enrich::sync(&pool, now()).await.unwrap();
    let r = e.run(&pool, now()).await.unwrap();
    assert_eq!((r.enriched, r.embedded), (1, 1), "{r:?}");
    let hash: String =
        sqlx::query_scalar("SELECT text_hash FROM events.event_embeddings WHERE event_id = $1")
            .bind(good_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(hash, rows.iter().find(|r| r.0 == good_id).unwrap().2);
    pool.close().await;
    db.drop_db().await;
}

fn app(pool: &PgPool) -> Router {
    let settings = ApiSettings {
        github_repo: "alexsiri7/musenmingle".into(),
        cors_origins: Vec::new(),
    };
    let suggestions = Suggestions::new(
        SuggestionConfig {
            ip_salt: Some("salt".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    musenmingle::api::router(pool.clone(), suggestions, settings)
        .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 1], 4000))))
}

async fn get(app: &Router, uri: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn set_embedding(pool: &PgPool, id: Uuid, v: [f32; 3]) {
    let mut full = vec![0.0f32; embed::EMBED_DIMS];
    full[..3].copy_from_slice(&v);
    store::save_embedding(
        pool,
        &store::NewEmbedding {
            event_id: id,
            model: "m",
            embed_version: 1,
            text_hash: "h",
            facts_only: false,
            embedding: &full,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn more_like_this_excludes_the_event_itself_and_past_events() {
    let Some(db) = TestDb::create("more_like_this_excludes_self_and_past").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    if !store::embeddings_available(&pool).await.unwrap() {
        common::notice("SKIPPING more_like_this: no pgvector on the test server");
        pool.close().await;
        db.drop_db().await;
        return;
    }
    let soon = Utc::now() + chrono::Duration::days(3);
    let target = event(&pool, "Target", "barbican", soon).await;
    let near = event(&pool, "Near", "barbican", soon).await;
    let far = event(&pool, "Far", "barbican", soon).await;
    let past = event(
        &pool,
        "Past twin",
        "barbican",
        Utc::now() - chrono::Duration::days(10),
    )
    .await;
    set_embedding(&pool, target, [1.0, 0.0, 0.0]).await;
    set_embedding(&pool, near, [0.9, 0.1, 0.0]).await;
    set_embedding(&pool, far, [0.0, 0.0, 1.0]).await;
    set_embedding(&pool, past, [1.0, 0.0, 0.0]).await;
    sqlx::query("UPDATE events.events SET medium_tags = '{painting}', format_tags = '{talk}' WHERE id = ANY($1)")
        .bind([target, near])
        .execute(&pool)
        .await
        .unwrap();

    let app = app(&pool);
    let (status, body) = get(&app, &format!("/v1/events/{target}/similar")).await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_str(&body).unwrap();
    let titles: Vec<&str> = body["similar"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Near", "Far"]);
    assert_eq!(
        body["similar"][0]["shared_tags"],
        json!(["painting", "talk"])
    );

    let (_, html) = get(&app, &format!("/events/{target}")).await;
    assert!(html.contains("More like this"), "{html}");
    assert!(html.contains("Similar because: shared painting, talk"));
    assert!(!html.contains("Past twin"));

    // The internal hybrid-search step: nearest to a query vector, filtered.
    // Closest to Far, then Target (Near is further from it than Target is,
    // so there is no tie to break by id).
    let mut q = vec![0.0f32; embed::EMBED_DIMS];
    q[..3].copy_from_slice(&[0.5, -0.5, 1.0]);
    let hits = musenmingle::repo::semantic_candidates(
        &pool,
        &q,
        &musenmingle::listing::EventFilter {
            from: Some(Utc::now()),
            ..Default::default()
        },
        2,
    )
    .await
    .unwrap();
    assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [far, target]);
    assert!((hits[0].1 - 1.0 / 1.5f64.sqrt()).abs() < 1e-6, "{hits:?}");
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn tag_filters_facets_and_the_labelled_ai_note() {
    let Some(db) = TestDb::create("tag_filters_facets_and_the_labelled_ai_note").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let soon = Utc::now() + chrono::Duration::days(3);
    let photo = event(&pool, "Photo talk", "barbican", soon).await;
    let paint = event(&pool, "Paint show", "barbican", soon).await;
    let plain = event(&pool, "Plain listing", "barbican", soon).await;
    sqlx::query(
        "UPDATE events.events SET medium_tags = '{photography}', format_tags = '{talk}',
             good_for = '{friends}', is_opening = true, whats_cool = 'Film cameras explained.',
             one_liner = 'Talk on analogue photography', ai_grounding = 'listing',
             ai_model = 'anthropic/claude-opus-5-5', ai_enriched_at = now()
         WHERE id = $1",
    )
    .bind(photo)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE events.events SET medium_tags = '{painting,photography}', good_for = '{solo}' WHERE id = $1")
        .bind(paint)
        .execute(&pool)
        .await
        .unwrap();
    let app = app(&pool);

    let (_, body) = get(&app, "/v1/events?medium=painting&medium=drawing").await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["events"].as_array().unwrap().len(), 1);
    assert_eq!(body["events"][0]["title"], "Paint show");
    assert!(body.get("facets").is_none());

    let (_, body) = get(
        &app,
        "/v1/events?medium=photography&good_for=friends&facets=true",
    )
    .await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["events"].as_array().unwrap().len(), 1);
    let e = &body["events"][0];
    assert_eq!(e["title"], "Photo talk");
    assert_eq!(e["is_opening"], true);
    assert_eq!(e["ai"]["label"], "AI-generated");
    assert_eq!(e["ai"]["whats_cool"], "Film cameras explained.");
    // Each facet ignores its own selection: both photography events have a
    // good_for value, and only "friends" matches medium=photography's other filter.
    assert_eq!(
        body["facets"]["good_for"],
        json!({ "friends": 1, "solo": 1 })
    );
    assert_eq!(body["facets"]["medium"], json!({ "photography": 1 }));
    assert_eq!(body["facets"]["format"], json!({ "talk": 1 }));

    let (status, _) = get(&app, "/v1/events?medium=pottery").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Pages: the one-liner on cards, counts in the filter, the labelled note.
    let (_, home) = get(&app, "/").await;
    assert!(
        home.contains(
            r#"<p class="one-liner" title="AI-written summary">Talk on analogue photography</p>"#
        ),
        "{home}"
    );
    assert!(home.contains("Photography (2)"), "{home}");
    assert!(home.contains(r#"<span class="badge opening">Opening</span>"#));
    let (_, filtered) = get(&app, "/?medium=painting").await;
    assert!(filtered.contains("Paint show") && !filtered.contains("Photo talk"));
    let (_, detail) = get(&app, &format!("/events/{photo}")).await;
    assert!(detail.contains("What's cool"), "{detail}");
    assert!(detail.contains(r#"<a class="ai-label" href="/about#ai">✨ AI note</a>"#));
    assert!(detail.contains("It is not the venue's description"));
    assert!(detail.contains(r#"href="/?medium=photography""#));
    let (_, detail) = get(&app, &format!("/events/{plain}")).await;
    assert!(!detail.contains("AI note"));
    let (_, about) = get(&app, "/about").await;
    assert!(about.contains(r#"<section id="ai""#));
    assert!(about.contains("zero data retention"));
    assert!(about.contains("Requesty itself may keep a log of our requests"));
    assert!(!about.contains("We don't use AI"));
    pool.close().await;
    db.drop_db().await;
}
