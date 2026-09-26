//! The read API (`GET /v1/events`, `GET /v1/events/{id}`, `GET /v1/sources`)
//! and CORS against a real database.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode, header};
use chrono::{DateTime, Utc};
use common::TestDb;
use serde_json::{Value, json};
use sqlx::PgPool;
use thaleia::api::ApiSettings;
use thaleia::config::{SuggestionConfig, parse_cors_origins};
use thaleia::suggestions::Suggestions;
use tower::ServiceExt;
use uuid::Uuid;

fn app(pool: &PgPool) -> Router {
    app_with_cors(
        pool,
        parse_cors_origins(Some("https://thaleia.example")).unwrap(),
    )
}

fn app_with_cors(pool: &PgPool, cors_origins: Vec<HeaderValue>) -> Router {
    let suggestions = Suggestions::new(
        SuggestionConfig {
            ip_salt: Some("salt".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let settings = ApiSettings {
        github_repo: "alexsiri7/thaleia".into(),
        cors_origins,
    };
    thaleia::api::router(pool.clone(), suggestions, settings)
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

fn titles(body: &Value) -> Vec<&str> {
    body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("no events in {body}"))
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect()
}

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// An event with only the fields a test cares about.
struct Ev {
    title: &'static str,
    starts_at: &'static str,
    ends_at: Option<&'static str>,
    category: &'static str,
    is_free: bool,
    at: Option<(f64, f64)>,
}

impl Ev {
    fn one_off(title: &'static str, starts_at: &'static str) -> Self {
        Ev {
            title,
            starts_at,
            ends_at: None,
            category: "talk",
            is_free: false,
            at: None,
        }
    }
}

async fn insert(pool: &PgPool, e: Ev) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events
            (title, starts_at, ends_at, category, is_free, lat, lng, dedupe_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $1) RETURNING id",
    )
    .bind(e.title)
    .bind(t(e.starts_at))
    .bind(e.ends_at.map(t))
    .bind(e.category)
    .bind(e.is_free)
    .bind(e.at.map(|a| a.0))
    .bind(e.at.map(|a| a.1))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn link(pool: &PgPool, event: Uuid, source_key: &str, url: &str, first_seen: &str) {
    sqlx::query(
        "INSERT INTO events.event_sources
            (event_id, source_id, source_event_id, source_url, raw, first_seen_at, last_seen_at)
         SELECT $1, id, $3, $3, '{}', $4, $4 FROM events.sources WHERE key = $2",
    )
    .bind(event)
    .bind(source_key)
    .bind(url)
    .bind(t(first_seen))
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn date_window_uses_overlap_for_ranged_events_and_london_days() {
    let Some(db) =
        TestDb::create("date_window_uses_overlap_for_ranged_events_and_london_days").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    // Window: 2026-10-01..=2026-10-03 London (BST) = [09-30T23:00Z, 10-03T23:00Z).
    let exhibition = |title, starts_at, ends_at| Ev {
        ends_at: Some(ends_at),
        category: "exhibition",
        ..Ev::one_off(title, starts_at)
    };
    for e in [
        exhibition(
            "spans window",
            "2026-09-01T09:00:00Z",
            "2026-12-01T00:00:00Z",
        ),
        // Closing day (stored as London midnight of the last day) is the window's first day.
        exhibition(
            "closes on first day",
            "2026-09-02T09:00:00Z",
            "2026-09-30T23:00:00Z",
        ),
        exhibition(
            "closed day before",
            "2026-09-03T09:00:00Z",
            "2026-09-29T23:00:00Z",
        ),
        exhibition(
            "opens on last day",
            "2026-10-03T09:00:00Z",
            "2026-11-01T00:00:00Z",
        ),
        exhibition(
            "opens after",
            "2026-10-03T23:00:00Z",
            "2026-11-01T00:00:00Z",
        ),
        // Overlap applies to any event with an end, not just exhibitions.
        Ev {
            ends_at: Some("2026-10-01T00:30:00Z"),
            ..Ev::one_off("talk into Oct 1", "2026-09-30T22:00:00Z")
        },
        Ev::one_off("00:30 London on Oct 1", "2026-09-30T23:30:00Z"),
        Ev::one_off("22:30 London on Sep 30", "2026-09-30T21:30:00Z"),
        Ev::one_off("inside", "2026-10-02T18:00:00Z"),
        Ev::one_off("00:30 London on Oct 4", "2026-10-03T23:30:00Z"),
    ] {
        insert(&pool, e).await;
    }
    let app = app(&pool);

    let (status, body) = get(&app, "/v1/events?from=2026-10-01&to=2026-10-03").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        titles(&body),
        [
            "spans window",
            "closes on first day",
            "talk into Oct 1",
            "00:30 London on Oct 1",
            "inside",
            "opens on last day",
        ]
    );
    assert_eq!(body["next_cursor"], Value::Null);

    let (_, body) = get(&app, "/v1/events?from=2026-10-03").await;
    assert_eq!(
        titles(&body),
        [
            "spans window",
            "opens on last day",
            "opens after",
            "00:30 London on Oct 4"
        ]
    );
    let (_, body) = get(&app, "/v1/events?to=2026-09-30").await;
    assert_eq!(
        titles(&body),
        [
            "spans window",
            "closes on first day",
            "closed day before",
            "22:30 London on Sep 30",
            "talk into Oct 1",
        ]
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn category_and_free_filters_combine() {
    let Some(db) = TestDb::create("category_and_free_filters_combine").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    for (title, category, is_free) in [
        ("free talk", "talk", true),
        ("paid talk", "talk", false),
        ("free workshop", "workshop", true),
        ("free expo", "expo", true),
    ] {
        let ev = Ev {
            category,
            is_free,
            ..Ev::one_off(title, "2026-10-01T18:00:00Z")
        };
        insert(&pool, ev).await;
    }
    let app = app(&pool);
    let sorted = |body: &Value| {
        let mut v: Vec<String> = titles(body).into_iter().map(String::from).collect();
        v.sort();
        v
    };

    let (_, body) = get(&app, "/v1/events?category=talk").await;
    assert_eq!(sorted(&body), ["free talk", "paid talk"]);
    let (_, body) = get(&app, "/v1/events?category=talk&category=workshop").await;
    assert_eq!(sorted(&body), ["free talk", "free workshop", "paid talk"]);
    let (_, body) = get(&app, "/v1/events?free=true").await;
    assert_eq!(sorted(&body), ["free expo", "free talk", "free workshop"]);
    let (_, body) = get(&app, "/v1/events?free=true&category=talk&category=expo").await;
    assert_eq!(sorted(&body), ["free expo", "free talk"]);
    let (_, body) = get(&app, "/v1/events?free=false").await;
    assert_eq!(titles(&body).len(), 4);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn near_filters_by_radius_and_sorts_by_distance() {
    let Some(db) = TestDb::create("near_filters_by_radius_and_sorts_by_distance").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    // Centre: Trafalgar Square (51.5080, -0.1281).
    for (title, at) in [
        ("tate modern ~2.0 km", Some((51.5076, -0.0994))),
        ("barbican ~2.7 km", Some((51.5202, -0.0938))),
        ("national gallery ~0.1 km", Some((51.5089, -0.1283))),
        ("kew ~12 km", Some((51.4787, -0.2956))),
        ("somewhere unknown", None),
        // Inside the 5 km bounding box but ~6.4 km away (diagonal corner).
        ("box corner ~6.3 km", Some((51.5480, -0.0640))),
    ] {
        let ev = Ev {
            at,
            ..Ev::one_off(title, "2026-10-01T18:00:00Z")
        };
        insert(&pool, ev).await;
    }
    let app = app(&pool);

    let (status, body) = get(&app, "/v1/events?near=51.5080,-0.1281").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        titles(&body),
        [
            "national gallery ~0.1 km",
            "tate modern ~2.0 km",
            "barbican ~2.7 km"
        ]
    );
    let d: Vec<f64> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["distance_km"].as_f64().unwrap())
        .collect();
    assert!((d[0] - 0.1).abs() < 0.05, "{d:?}");
    assert!((d[1] - 1.99).abs() < 0.1, "{d:?}");
    assert!((d[2] - 2.73).abs() < 0.1, "{d:?}");

    let (_, body) = get(&app, "/v1/events?near=51.5080,-0.1281&radius_km=20").await;
    assert_eq!(
        titles(&body),
        [
            "national gallery ~0.1 km",
            "tate modern ~2.0 km",
            "barbican ~2.7 km",
            "box corner ~6.3 km",
            "kew ~12 km",
        ]
    );
    // Without `near` there is no distance and all events are listed.
    let (_, body) = get(&app, "/v1/events").await;
    assert_eq!(titles(&body).len(), 6);
    assert!(body["events"][0].get("distance_km").is_none());
    pool.close().await;
    db.drop_db().await;
}

async fn walk_pages(app: &Router, first: &str) -> Vec<String> {
    let mut seen = Vec::new();
    let mut uri = first.to_string();
    loop {
        let (status, body) = get(app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let page = titles(&body);
        assert!(page.len() <= 2, "{body}");
        seen.extend(page.into_iter().map(String::from));
        match body["next_cursor"].as_str() {
            Some(c) => uri = format!("{first}&cursor={c}"),
            None => return seen,
        }
    }
}

#[tokio::test]
async fn cursor_pagination_walks_every_event_once() {
    let Some(db) = TestDb::create("cursor_pagination_walks_every_event_once").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    // Ties on starts_at and distance are broken by id.
    for (title, starts_at, lat) in [
        ("a", "2026-10-01T18:00:00Z", 51.500),
        ("b", "2026-10-01T18:00:00Z", 51.501),
        ("c", "2026-10-01T18:00:00Z", 51.501),
        ("d", "2026-10-02T18:00:00Z", 51.502),
        ("e", "2026-10-03T18:00:00Z", 51.503),
    ] {
        let ev = Ev {
            at: Some((lat, -0.1)),
            ..Ev::one_off(title, starts_at)
        };
        insert(&pool, ev).await;
    }
    let app = app(&pool);

    let by_start = walk_pages(&app, "/v1/events?limit=2").await;
    let (_, all) = get(&app, "/v1/events").await;
    assert_eq!(by_start, titles(&all));
    let mut sorted = by_start.clone();
    sorted.sort();
    assert_eq!(sorted, ["a", "b", "c", "d", "e"]);
    assert_eq!(by_start[3..], ["d", "e"]);

    let by_distance = walk_pages(&app, "/v1/events?limit=2&near=51.500,-0.1").await;
    let (_, all) = get(&app, "/v1/events?near=51.500,-0.1").await;
    assert_eq!(by_distance, titles(&all));
    assert_eq!(by_distance.len(), 5);
    assert_eq!(by_distance[0], "a");

    // A cursor from one sort cannot be used with the other.
    let (_, page) = get(&app, "/v1/events?limit=2").await;
    let cursor = page["next_cursor"].as_str().unwrap();
    let (status, body) = get(
        &app,
        &format!("/v1/events?limit=2&near=51.5,-0.1&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].is_string());
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn event_by_id_includes_every_source_link_and_unknown_ids_are_404() {
    let Some(db) =
        TestDb::create("event_by_id_includes_every_source_link_and_unknown_ids_are_404").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let id = insert(
        &pool,
        Ev {
            ends_at: Some("2026-10-01T20:00:00Z"),
            is_free: true,
            at: Some((51.5, -0.1)),
            ..Ev::one_off("Life drawing", "2026-10-01T18:00:00Z")
        },
    )
    .await;
    sqlx::query(
        "UPDATE events.events SET venue_name = 'Barbican', price_min = 0, price_max = 12.5,
             currency = 'GBP', tags = '{art,drawing}' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    link(
        &pool,
        id,
        "barbican",
        "https://www.barbican.org.uk/life-drawing",
        "2026-09-01T00:00:00Z",
    )
    .await;
    link(
        &pool,
        id,
        "ticketmaster",
        "https://www.ticketmaster.co.uk/x",
        "2026-09-02T00:00:00Z",
    )
    .await;
    let app = app(&pool);

    let (status, body) = get(&app, &format!("/v1/events/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({
            "id": id,
            "title": "Life drawing",
            "description": null,
            "venue_name": "Barbican",
            "address": null,
            "lat": 51.5,
            "lng": -0.1,
            "starts_at": "2026-10-01T18:00:00Z",
            "ends_at": "2026-10-01T20:00:00Z",
            "is_free": true,
            "price_min": "0",
            "price_max": "12.50",
            "currency": "GBP",
            "url": null,
            "image_url": null,
            "category": "talk",
            "tags": ["art", "drawing"],
            "sources": [
                {
                    "source": "barbican",
                    "url": "https://www.barbican.org.uk/life-drawing",
                    "first_seen_at": "2026-09-01T00:00:00Z",
                    "last_seen_at": "2026-09-01T00:00:00Z",
                },
                {
                    "source": "ticketmaster",
                    "url": "https://www.ticketmaster.co.uk/x",
                    "first_seen_at": "2026-09-02T00:00:00Z",
                    "last_seen_at": "2026-09-02T00:00:00Z",
                },
            ],
        })
    );
    // The list carries the same links.
    let (_, list) = get(&app, "/v1/events").await;
    assert_eq!(list["events"][0], body);

    for unknown in [Uuid::new_v4().to_string(), "not-a-uuid".into()] {
        let (status, body) = get(&app, &format!("/v1/events/{unknown}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
        assert_eq!(body, json!({ "error": "not found" }));
    }
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn invalid_list_parameters_are_400() {
    let Some(db) = TestDb::create("invalid_list_parameters_are_400").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app(&pool);
    for q in [
        "from=01-10-2026",
        "category=concert",
        "near=51.5",
        "radius_km=2",
        "limit=101",
        "cursor=nonsense",
        "colour=red",
    ] {
        let (status, body) = get(&app, &format!("/v1/events?{q}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}: {body}");
        assert!(body["error"].is_string(), "{q}: {body}");
    }
    pool.close().await;
    db.drop_db().await;
}

async fn run(pool: &PgPool, key: &str, started_at: &str, events_found: i32, errors: i32, ok: bool) {
    let started_at = t(started_at);
    let source = thaleia::repo::source_by_key(pool, key)
        .await
        .unwrap()
        .unwrap();
    thaleia::repo::record_run(
        pool,
        &thaleia::repo::NewRun {
            source_id: source.id,
            started_at,
            finished_at: started_at + chrono::Duration::milliseconds(1500),
            events_found,
            errors,
            error_summary: None,
            ok,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn sources_report_last_run_and_health() {
    let Some(db) = TestDb::create("sources_report_last_run_and_health").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    // Healthy: the latest run is clean although an earlier one failed.
    run(&pool, "barbican", "2026-09-25T06:00:00Z", 0, 1, false).await;
    run(&pool, "barbican", "2026-09-26T06:00:00Z", 40, 0, true).await;
    // Degraded: latest run had errors, no open issue.
    run(&pool, "design-museum", "2026-09-26T06:00:00Z", 12, 2, true).await;
    // Broken: open issue (a closed one on another source does not count).
    run(
        &pool,
        "whitechapel-gallery",
        "2026-09-26T06:00:00Z",
        0,
        1,
        false,
    )
    .await;
    // Unconfigured wins over broken: an issue is open, but ingest now skips it.
    run(&pool, "ticketmaster", "2026-09-26T06:00:00Z", 0, 1, false).await;
    let source_id = |key: &'static str| {
        let pool = pool.clone();
        async move {
            thaleia::repo::source_by_key(&pool, key)
                .await
                .unwrap()
                .unwrap()
                .id
        }
    };
    thaleia::repo::insert_health_issue(&pool, source_id("whitechapel-gallery").await, 32, "errors")
        .await
        .unwrap();
    let ticketmaster_id = source_id("ticketmaster").await;
    thaleia::repo::insert_health_issue(&pool, ticketmaster_id, 31, "errors")
        .await
        .unwrap();
    let skipped_at = t("2026-09-26T06:30:00Z");
    thaleia::repo::record_skip(
        &pool,
        ticketmaster_id,
        "TICKETMASTER_API_KEY not set",
        skipped_at,
    )
    .await
    .unwrap();
    // Unconfigured without any run.
    thaleia::repo::record_skip(
        &pool,
        source_id("somerset-house").await,
        "no implementation for this source key",
        skipped_at,
    )
    .await
    .unwrap();
    let serpentine = source_id("serpentine-galleries").await;
    thaleia::repo::insert_health_issue(&pool, serpentine, 7, "old")
        .await
        .unwrap();
    thaleia::repo::close_health_issues(&pool, serpentine)
        .await
        .unwrap();
    let app = app(&pool);

    let (status, body) = get(&app, "/v1/sources").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sources = body["sources"].as_array().unwrap();
    let keys: Vec<&str> = sources.iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert!(keys.is_sorted(), "{keys:?}");
    let source = |key: &str| {
        sources
            .iter()
            .find(|s| s["key"] == key)
            .unwrap_or_else(|| panic!("no {key} in {body}"))
    };
    assert_eq!(
        source("barbican"),
        &json!({
            "key": "barbican",
            "kind": "scraper",
            "interval_minutes": 1440,
            "enabled": true,
            "last_run": {
                "started_at": "2026-09-26T06:00:00Z",
                "events_found": 40,
                "errors": 0,
                "duration_ms": 1500,
                "ok": true,
            },
            "skip": null,
            "status": "healthy",
            "issue_url": null,
        })
    );
    assert_eq!(source("design-museum")["status"], "degraded");
    assert_eq!(source("design-museum")["issue_url"], Value::Null);
    // Pending: never ran (its only issue is closed).
    assert_eq!(source("serpentine-galleries")["status"], "pending");
    assert_eq!(source("serpentine-galleries")["last_run"], Value::Null);
    assert_eq!(source("serpentine-galleries")["skip"], Value::Null);
    let whitechapel = source("whitechapel-gallery");
    assert_eq!(whitechapel["status"], "broken");
    assert_eq!(
        whitechapel["issue_url"],
        "https://github.com/alexsiri7/thaleia/issues/32"
    );
    let somerset = source("somerset-house");
    assert_eq!(somerset["status"], "unconfigured");
    assert_eq!(somerset["last_run"], Value::Null);
    assert_eq!(
        somerset["skip"],
        json!({
            "at": "2026-09-26T06:30:00Z",
            "reason": "no implementation for this source key",
        })
    );
    let ticketmaster = source("ticketmaster");
    assert_eq!(ticketmaster["kind"], "api");
    assert_eq!(ticketmaster["status"], "unconfigured");
    assert_eq!(
        ticketmaster["skip"]["reason"],
        "TICKETMASTER_API_KEY not set"
    );
    assert_eq!(ticketmaster["last_run"]["ok"], false);
    assert_eq!(
        ticketmaster["issue_url"],
        "https://github.com/alexsiri7/thaleia/issues/31"
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn cors_unset_allows_no_origin() {
    let Some(db) = TestDb::create("cors_unset_allows_no_origin").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app_with_cors(&pool, parse_cors_origins(None).unwrap());
    let resp = app
        .clone()
        .oneshot(
            Request::get("/v1/sources")
                .header(header::ORIGIN, "https://anything.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        None
    );

    let resp = app
        .oneshot(
            Request::options("/v1/suggestions")
                .header(header::ORIGIN, "https://anything.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        None
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn cors_allows_only_configured_origins() {
    let Some(db) = TestDb::create("cors_allows_only_configured_origins").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app(&pool);
    let allow_origin = |origin: &'static str| {
        let app = app.clone();
        async move {
            let resp = app
                .oneshot(
                    Request::get("/v1/sources")
                        .header(header::ORIGIN, origin)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .map(|v| v.to_str().unwrap().to_string())
        }
    };
    assert_eq!(
        allow_origin("https://thaleia.example").await.as_deref(),
        Some("https://thaleia.example")
    );
    assert_eq!(allow_origin("https://evil.example").await, None);

    // Preflight for the suggestions form.
    let resp = app
        .clone()
        .oneshot(
            Request::options("/v1/suggestions")
                .header(header::ORIGIN, "https://thaleia.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers();
    assert_eq!(
        h[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://thaleia.example"
    );
    assert!(
        h[header::ACCESS_CONTROL_ALLOW_METHODS]
            .to_str()
            .unwrap()
            .contains("POST")
    );
    assert!(
        h[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .contains("content-type")
    );
    pool.close().await;
    db.drop_db().await;
}
