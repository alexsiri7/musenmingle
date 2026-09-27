//! `sort=` on `GET /v1/events` (issue #74): each sort's order, ties, cursor
//! pagination per sort, the `nearest` fallback and the daily shuffle,
//! against a real database.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::{SuggestionConfig, parse_cors_origins};
use musenmingle::listing::{Cursor, EventOrder, parse_query_at};
use musenmingle::repo;
use musenmingle::suggestions::Suggestions;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn app(pool: &PgPool) -> Router {
    let suggestions = Suggestions::new(
        SuggestionConfig {
            ip_salt: Some("salt".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let settings = ApiSettings {
        github_repo: "alexsiri7/musenmingle".into(),
        cors_origins: parse_cors_origins(None).unwrap(),
    };
    musenmingle::api::router(pool.clone(), suggestions, settings)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn titles(body: &Value) -> Vec<String> {
    body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("no events in {body}"))
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect()
}

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

struct Ev {
    title: &'static str,
    starts_at: &'static str,
    ends_at: Option<&'static str>,
    all_day: bool,
    at: Option<(f64, f64)>,
}

fn ev(title: &'static str, starts_at: &'static str) -> Ev {
    Ev {
        title,
        starts_at,
        ends_at: None,
        all_day: false,
        at: None,
    }
}

async fn insert(pool: &PgPool, e: Ev) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events
            (title, starts_at, ends_at, all_day, category, lat, lng, dedupe_key)
         VALUES ($1, $2, $3, $4, 'talk', $5, $6, $1) RETURNING id",
    )
    .bind(e.title)
    .bind(t(e.starts_at))
    .bind(e.ends_at.map(t))
    .bind(e.all_day)
    .bind(e.at.map(|a| a.0))
    .bind(e.at.map(|a| a.1))
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A listing of `event` on any seeded source, first seen at `first_seen`.
async fn link(pool: &PgPool, event: Uuid, n: i64, first_seen: &str) {
    sqlx::query(
        "INSERT INTO events.event_sources
            (event_id, source_id, source_event_id, source_url, raw, first_seen_at, last_seen_at)
         SELECT $1, id, $2, NULL, '{}', $3, $3 FROM events.sources ORDER BY id OFFSET $4 LIMIT 1",
    )
    .bind(event)
    .bind(format!("{event}-{n}"))
    .bind(t(first_seen))
    .bind(n)
    .execute(pool)
    .await
    .unwrap();
}

async fn list(pool: &PgPool, raw: &str, now: &str) -> Vec<String> {
    let q = parse_query_at(raw, t(now)).unwrap();
    repo::list_events(pool, &q)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.event.title)
        .collect()
}

async fn walk_pages(app: &Router, first: &str) -> Vec<String> {
    let mut seen = Vec::new();
    let mut uri = first.to_string();
    loop {
        let (status, body) = get(app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let page = titles(&body);
        assert!(page.len() <= 2, "{body}");
        seen.extend(page);
        match body["next_cursor"].as_str() {
            Some(c) => uri = format!("{first}&cursor={c}"),
            None => return seen,
        }
    }
}

#[tokio::test]
async fn ending_uses_the_effective_end_and_hides_ended_events() {
    let Some(db) = TestDb::create("ending_uses_the_effective_end_and_hides_ended_events").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    // "Now" is 13:00 London (12:00 UTC) on Saturday 10 October 2026.
    let now = "2026-10-10T12:00:00Z";
    for e in [
        // All-day exhibition whose last day is today: open until midnight.
        Ev {
            ends_at: Some("2026-10-09T23:00:00Z"),
            all_day: true,
            ..ev("last day today", "2026-08-31T23:00:00Z")
        },
        // Tonight's talk: closes at 18:00 + 3 h.
        ev("talk tonight", "2026-10-10T18:00:00Z"),
        // This morning's talk: still inside its 3 hours until 13:00.
        ev("talk this morning", "2026-10-10T10:00:00Z"),
        // Ended at 11:00 (08:00 + 3 h).
        ev("talk ended", "2026-10-10T08:00:00Z"),
        // Timed range: ends at its end.
        Ev {
            ends_at: Some("2026-10-20T17:00:00Z"),
            ..ev("run to the 20th", "2026-10-01T09:00:00Z")
        },
        // All-day single day yesterday: over at midnight.
        Ev {
            all_day: true,
            ..ev("all day yesterday", "2026-10-08T23:00:00Z")
        },
        // All-day ending across the clocks change (last day Sun 25 Oct, the
        // BST -> GMT day): over at 00:00 GMT on the 26th.
        Ev {
            ends_at: Some("2026-10-24T23:00:00Z"),
            all_day: true,
            ..ev("ends on clocks day", "2026-09-30T23:00:00Z")
        },
        // Same effective end as "run to the 20th": ties break by id.
        Ev {
            ends_at: Some("2026-10-20T17:00:00Z"),
            ..ev("also to the 20th", "2026-10-02T09:00:00Z")
        },
    ] {
        insert(&pool, e).await;
    }
    let q = parse_query_at("sort=ending", t(now)).unwrap();
    let rows = repo::list_events(&pool, &q).await.unwrap();
    let got: Vec<&str> = rows.iter().map(|r| r.event.title.as_str()).collect();
    assert_eq!(
        &got[..3],
        ["talk this morning", "talk tonight", "last day today"]
    );
    let mut tied: Vec<&str> = got[3..5].to_vec();
    tied.sort();
    assert_eq!(tied, ["also to the 20th", "run to the 20th"]);
    assert!(rows[3].event.id < rows[4].event.id, "ties break by id");
    assert_eq!(got[5], "ends on clocks day");
    assert_eq!(got.len(), 6, "{got:?}");
    assert_eq!(rows[2].sort_at, Some(t("2026-10-10T23:00:00Z")));
    assert_eq!(rows[5].sort_at, Some(t("2026-10-26T00:00:00Z")));

    // Pages: a cursor walk gives the same order.
    let mut walked = Vec::new();
    let mut raw = "sort=ending&limit=2".to_string();
    loop {
        let q = parse_query_at(&raw, t(now)).unwrap();
        let mut page = repo::list_events(&pool, &q).await.unwrap();
        let more = page.len() > 2;
        page.truncate(2);
        walked.extend(page.iter().map(|r| r.event.title.clone()));
        if !more {
            break;
        }
        let last = page.last().unwrap();
        let c = Cursor::End(last.sort_at.unwrap(), last.event.id).encode();
        raw = format!("sort=ending&limit=2&cursor={c}");
    }
    assert_eq!(walked, got);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn added_is_first_seen_newest_first() {
    let Some(db) = TestDb::create("added_is_first_seen_newest_first").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let old = insert(&pool, ev("old listing", "2026-10-01T18:00:00Z")).await;
    link(&pool, old, 0, "2026-09-01T10:00:00Z").await;
    // Seen again by a second source yesterday: still "new to us" in September.
    link(&pool, old, 1, "2026-09-26T10:00:00Z").await;
    let newest = insert(&pool, ev("newest", "2026-12-01T18:00:00Z")).await;
    link(&pool, newest, 0, "2026-09-27T09:00:00Z").await;
    let middle = insert(&pool, ev("middle", "2026-10-05T18:00:00Z")).await;
    link(&pool, middle, 0, "2026-09-20T09:00:00Z").await;
    let now = "2026-09-27T12:00:00Z";
    assert_eq!(
        list(&pool, "sort=added", now).await,
        ["newest", "middle", "old listing"]
    );
    let app = app(&pool);
    assert_eq!(
        walk_pages(&app, "/v1/events?sort=added&limit=2").await,
        ["newest", "middle", "old listing"]
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn surprise_is_stable_for_a_day_and_pages_across_midnight() {
    let Some(db) = TestDb::create("surprise_is_stable_for_a_day_and_pages_across_midnight").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let names = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p",
    ];
    for n in names {
        insert(&pool, ev(n, "2026-10-01T18:00:00Z")).await;
    }
    let day1 = "2026-10-10T12:00:00Z";
    let day1_late = "2026-10-10T22:59:00Z"; // 23:59 BST: still the 10th
    let day2 = "2026-10-10T23:01:00Z"; // 00:01 BST on the 11th
    let order1 = list(&pool, "sort=surprise", day1).await;
    assert_eq!(order1.len(), names.len());
    assert_eq!(list(&pool, "sort=surprise", day1_late).await, order1);
    let order2 = list(&pool, "sort=surprise", day2).await;
    assert_ne!(order1, order2, "a new London day reshuffles");
    let mut sorted = order2.clone();
    sorted.sort();
    assert_eq!(sorted, names);

    // Page 1 on the 10th, the rest after midnight: the cursor keeps the
    // 10th's order, so nothing is repeated or skipped.
    let mut walked = Vec::new();
    let mut raw = "sort=surprise&limit=5".to_string();
    let mut now = day1;
    loop {
        let q = parse_query_at(&raw, t(now)).unwrap();
        let EventOrder::Shuffled { seed, .. } = q.order else {
            panic!("expected shuffle")
        };
        let mut page = repo::list_events(&pool, &q).await.unwrap();
        let more = page.len() > 5;
        page.truncate(5);
        walked.extend(page.iter().map(|r| r.event.title.clone()));
        if !more {
            break;
        }
        let c = Cursor::Shuffle(seed, page.last().unwrap().event.id).encode();
        raw = format!("sort=surprise&limit=5&cursor={c}");
        now = day2;
    }
    assert_eq!(walked, order1);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn every_sort_paginates_and_nearest_falls_back_without_near() {
    let Some(db) = TestDb::create("every_sort_paginates_and_nearest_falls_back_without_near").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    // Far in the future, so "ending" (which uses the real clock here) keeps them.
    for (title, starts_at, ends_at, lat) in [
        ("a", "2030-10-01T18:00:00Z", None, 51.500),
        ("b", "2030-10-01T18:00:00Z", None, 51.501),
        (
            "c",
            "2030-10-01T18:00:00Z",
            Some("2030-12-01T18:00:00Z"),
            51.501,
        ),
        ("d", "2030-10-02T18:00:00Z", None, 51.502),
        (
            "e",
            "2030-10-03T18:00:00Z",
            Some("2030-10-03T19:00:00Z"),
            51.503,
        ),
        ("far", "2030-09-01T18:00:00Z", None, 52.5),
    ] {
        let id = insert(
            &pool,
            Ev {
                ends_at,
                at: Some((lat, -0.1)),
                ..ev(title, starts_at)
            },
        )
        .await;
        link(&pool, id, 0, "2026-09-20T09:00:00Z").await;
    }
    let app = app(&pool);
    for sort in ["soonest", "nearest", "ending", "added", "surprise"] {
        let base = format!("/v1/events?sort={sort}&near=51.5,-0.1");
        let (status, all) = get(&app, &base).await;
        assert_eq!(status, StatusCode::OK, "{all}");
        assert_eq!(all["sort"], sort);
        let walked = walk_pages(&app, &format!("{base}&limit=2")).await;
        assert_eq!(walked, titles(&all), "{sort}");
        let mut sorted = walked.clone();
        sorted.sort();
        // The area filters every sort, not only nearest.
        assert_eq!(sorted, ["a", "b", "c", "d", "e"], "{sort}");
    }
    let (_, soonest) = get(&app, "/v1/events?sort=soonest&near=51.5,-0.1").await;
    assert_eq!(titles(&soonest)[3..], ["d", "e"]);
    assert!(soonest["events"][0]["distance_km"].is_number());
    let (_, ending) = get(&app, "/v1/events?sort=ending&near=51.5,-0.1").await;
    assert_eq!(titles(&ending)[4], "c", "the long run closes last");

    // No sort + near keeps the old default: nearest first.
    let (_, body) = get(&app, "/v1/events?near=51.5,-0.1").await;
    assert_eq!(body["sort"], "nearest");
    assert_eq!(titles(&body)[0], "a");
    assert!(body.get("sort_fallback").is_none());

    // nearest without near: soonest, and says so.
    let (status, body) = get(&app, "/v1/events?sort=nearest").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sort"], "soonest");
    assert_eq!(body["sort_fallback"]["requested"], "nearest");
    assert_eq!(titles(&body)[0], "far");
    let walked = walk_pages(&app, "/v1/events?sort=nearest&limit=2").await;
    assert_eq!(walked, titles(&body));

    // A cursor from one sort is refused by another; unknown sorts are 400.
    let (_, page) = get(&app, "/v1/events?sort=ending&limit=2").await;
    let cursor = page["next_cursor"].as_str().unwrap();
    for other in ["soonest", "added", "surprise"] {
        let (status, _) = get(
            &app,
            &format!("/v1/events?sort={other}&limit=2&cursor={cursor}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{other}");
    }
    let (status, body) = get(&app, "/v1/events?sort=popular").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("surprise"));
    pool.close().await;
    db.drop_db().await;
}
