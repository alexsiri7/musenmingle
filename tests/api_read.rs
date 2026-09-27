//! The read API (`GET /v1/events`, `GET /v1/events/{id}`, `GET /v1/sources`)
//! and CORS against a real database.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode, header};
use chrono::{DateTime, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::{SuggestionConfig, parse_cors_origins};
use musenmingle::suggestions::Suggestions;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn app(pool: &PgPool) -> Router {
    app_with_cors(
        pool,
        parse_cors_origins(Some("https://musenmingle.example")).unwrap(),
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
        github_repo: "alexsiri7/musenmingle".into(),
        cors_origins,
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
    price_min: Option<&'static str>,
    currency: Option<&'static str>,
    tags: &'static [&'static str],
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
            price_min: None,
            currency: None,
            tags: &[],
        }
    }
}

async fn insert(pool: &PgPool, e: Ev) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events
            (title, starts_at, ends_at, category, is_free, lat, lng, dedupe_key,
             price_min, currency, tags)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $1, $8::numeric, $9, $10::text[]) RETURNING id",
    )
    .bind(e.title)
    .bind(t(e.starts_at))
    .bind(e.ends_at.map(t))
    .bind(e.category)
    .bind(e.is_free)
    .bind(e.at.map(|a| a.0))
    .bind(e.at.map(|a| a.1))
    .bind(e.price_min)
    .bind(e.currency)
    .bind(e.tags)
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
            "all_day": false,
            "is_free": true,
            "price_min": "0",
            "price_max": "12.50",
            "currency": "GBP",
            "url": null,
            "thumbnail_url": null,
            "image_credit": null,
            "category": "talk",
            "tags": ["art", "drawing"],
            "medium_tags": [],
            "format_tags": [],
            "good_for": [],
            "vibe_tags": [],
            "is_opening": null,
            "ai": null,
            "sources": [
                {
                    "source": "barbican",
                    "display_name": "Barbican",
                    "url": "https://www.barbican.org.uk/life-drawing",
                    "first_seen_at": "2026-09-01T00:00:00Z",
                    "last_seen_at": "2026-09-01T00:00:00Z",
                },
                {
                    "source": "ticketmaster",
                    "display_name": "Ticketmaster",
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
async fn source_filter_matches_any_listing_of_an_event() {
    let Some(db) = TestDb::create("source_filter_matches_any_listing_of_an_event").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let both = insert(&pool, Ev::one_off("on both", "2026-10-01T18:00:00Z")).await;
    link(
        &pool,
        both,
        "barbican",
        "https://www.barbican.org.uk/a",
        "2026-09-01T00:00:00Z",
    )
    .await;
    link(
        &pool,
        both,
        "ticketmaster",
        "https://www.ticketmaster.co.uk/a",
        "2026-09-02T00:00:00Z",
    )
    .await;
    let barbican = insert(&pool, Ev::one_off("barbican only", "2026-10-02T18:00:00Z")).await;
    link(
        &pool,
        barbican,
        "barbican",
        "https://www.barbican.org.uk/b",
        "2026-09-01T00:00:00Z",
    )
    .await;
    let tm = insert(
        &pool,
        Ev::one_off("ticketmaster only", "2026-10-03T18:00:00Z"),
    )
    .await;
    link(
        &pool,
        tm,
        "ticketmaster",
        "https://www.ticketmaster.co.uk/c",
        "2026-09-01T00:00:00Z",
    )
    .await;
    insert(&pool, Ev::one_off("no source", "2026-10-04T18:00:00Z")).await;
    let app = app(&pool);

    for (q, expected) in [
        ("source=barbican", vec!["on both", "barbican only"]),
        ("source=ticketmaster", vec!["on both", "ticketmaster only"]),
        (
            "source=barbican&source=ticketmaster",
            vec!["on both", "barbican only", "ticketmaster only"],
        ),
        ("source=design-museum", vec![]),
        ("source=barbican&near=51.5,-0.1", vec![]),
    ] {
        let (status, body) = get(&app, &format!("/v1/events?{q}")).await;
        assert_eq!(status, StatusCode::OK, "{q}: {body}");
        assert_eq!(titles(&body), expected, "{q}");
    }
    let (status, _) = get(&app, "/v1/events?source=Not%20A%20Key").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn sources_lists_refused_sites() {
    let Some(db) = TestDb::create("sources_lists_refused_sites").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    // `js_only` is an accepted reason code (migration ..09).
    sqlx::query(
        "INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on)
         VALUES ('js.example', 'JS Site', 'https://js.example/', 'js_only', 'needs JS', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (status, mut body) = get(&app(&pool), "/v1/sources").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let refused = body["refused"].as_array_mut().unwrap();
    // Most recently checked first (later migrations add more refusals).
    let checked: Vec<&str> = refused
        .iter()
        .map(|r| r["checked_on"].as_str().unwrap())
        .collect();
    assert!(checked.is_sorted_by(|a, b| a >= b), "{checked:?}");
    assert_eq!(refused.last().unwrap()["reason_code"], "js_only");
    // The 2026-09-26 source survey (#47: 28 rows in migration 20260927970001, plus
    // National Gallery and White Cube) and venue-discovery pass (#104).
    let from_issue = |n: u32| {
        let url = format!("https://github.com/alexsiri7/musenmingle/issues/{n}");
        refused.iter().filter(|r| r["issue_url"] == url).count()
    };
    assert_eq!((from_issue(47), from_issue(104)), (30, 21));
    // The two refusals seeded by migration ..08.
    refused.retain(|r| r["checked_on"] == "2026-09-25");
    assert_eq!(
        body["refused"],
        json!([
            {
                "name": "CreativeMornings London",
                "domain": "creativemornings.com",
                "url": "https://creativemornings.com/cities/lon",
                "reason_code": "robots_disallowed",
                "reason_text": "robots.txt disallows /happening and the site answers our bot with an empty 202",
                "checked_on": "2026-09-25",
                "issue_url": "https://github.com/alexsiri7/musenmingle/issues/4",
            },
            {
                "name": "Southbank Centre",
                "domain": "southbankcentre.co.uk",
                "url": "https://www.southbankcentre.co.uk/whats-on",
                "reason_code": "bot_blocked",
                "reason_text": "it returns 403 to our crawler's User-Agent; we don't evade blocks",
                "checked_on": "2026-09-25",
                "issue_url": "https://github.com/alexsiri7/musenmingle/issues/6",
            },
        ])
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn ids_filter_returns_only_those_events() {
    let Some(db) = TestDb::create("ids_filter_returns_only_those_events").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let a = insert(&pool, Ev::one_off("a", "2026-10-01T18:00:00Z")).await;
    let b = insert(&pool, Ev::one_off("b", "2020-01-01T18:00:00Z")).await;
    insert(&pool, Ev::one_off("c", "2026-10-02T18:00:00Z")).await;
    let app = app(&pool);

    // Past events too (no implicit date window); unknown ids are just absent.
    let (status, body) = get(&app, &format!("/v1/events?ids={a},{b},{}", Uuid::new_v4())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(titles(&body), ["b", "a"]);
    // Combines with other filters.
    let (_, body) = get(&app, &format!("/v1/events?ids={a},{b}&from=2026-01-01")).await;
    assert_eq!(titles(&body), ["a"]);

    let too_many: Vec<String> = (0..101).map(|_| Uuid::new_v4().to_string()).collect();
    for q in [
        format!("ids={}", too_many.join(",")),
        "ids=nope".into(),
        "ids=".into(),
    ] {
        let (status, body) = get(&app, &format!("/v1/events?{q}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}: {body}");
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
        "when=night",
        "price_max=-1",
    ] {
        let (status, body) = get(&app, &format!("/v1/events?{q}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}: {body}");
        assert!(body["error"].is_string(), "{q}: {body}");
    }
    pool.close().await;
    db.drop_db().await;
}

async fn sorted_titles(app: &Router, query: &str) -> Vec<String> {
    let (status, body) = get(app, &format!("/v1/events?{query}")).await;
    assert_eq!(status, StatusCode::OK, "{query}: {body}");
    let mut titles: Vec<String> = titles(&body).into_iter().map(str::to_string).collect();
    titles.sort();
    titles
}

/// Each count reported by `GET /v1/events?{base}` is the number of events
/// listed when that option is added to `base`.
async fn assert_counts_match_results(app: &Router, base: &str) {
    let (_, body) = get(app, &format!("/v1/events?{base}")).await;
    let counts = &body["counts"];
    for (option, count) in [
        ("when=evening", &counts["when"]["evening"]),
        ("when=after_work", &counts["when"]["after_work"]),
        ("when=weekend", &counts["when"]["weekend"]),
        ("when=daytime", &counts["when"]["daytime"]),
        ("free=true", &counts["price"]["free"]),
        ("price_max=10", &counts["price"]["max_10"]),
        ("price_max=20", &counts["price"]["max_20"]),
    ] {
        let listed = sorted_titles(app, &format!("{base}&{option}&limit=100")).await;
        assert_eq!(
            count.as_u64(),
            Some(listed.len() as u64),
            "{base}&{option}: {counts}"
        );
    }
}

#[tokio::test]
async fn when_buckets_use_london_time() {
    let Some(db) = TestDb::create("when_buckets_use_london_time").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    // BST ends 2026-10-25 01:00Z.
    let exhibition = |title, tags| Ev {
        ends_at: Some("2026-10-04T23:00:00Z"),
        category: "exhibition",
        tags,
        // Thu 1 – Mon 5 October, London dates only.
        ..Ev::one_off(title, "2026-09-30T23:00:00Z")
    };
    for e in [
        Ev::one_off("BST 17:30Z Thu", "2026-10-01T17:30:00Z"),
        Ev::one_off("GMT 17:30Z Mon", "2026-11-02T17:30:00Z"),
        Ev::one_off("BST 16:30Z Sat", "2026-10-24T16:30:00Z"),
        Ev::one_off("GMT 18:00Z Fri", "2026-11-06T18:00:00Z"),
        Ev::one_off("BST 19:45Z Mon", "2026-10-05T19:45:00Z"),
        Ev::one_off("BST 23:00Z untimed", "2026-10-01T23:00:00Z"),
        Ev::one_off("GMT 23:00Z Thu", "2026-11-05T23:00:00Z"),
        exhibition("untimed exhibition", &[]),
        exhibition("late exhibition", &["late opening"]),
    ] {
        insert(&pool, e).await;
    }
    let app = app(&pool);
    for (when, expected) in [
        (
            "evening",
            vec![
                "BST 17:30Z Thu",
                "BST 19:45Z Mon",
                "GMT 18:00Z Fri",
                "GMT 23:00Z Thu",
                "late exhibition",
            ],
        ),
        (
            "after_work",
            vec![
                "BST 17:30Z Thu",
                "GMT 17:30Z Mon",
                "GMT 18:00Z Fri",
                "late exhibition",
            ],
        ),
        (
            "daytime",
            vec![
                "BST 16:30Z Sat",
                "BST 23:00Z untimed",
                "GMT 17:30Z Mon",
                "late exhibition",
                "untimed exhibition",
            ],
        ),
        (
            "weekend",
            vec!["BST 16:30Z Sat", "late exhibition", "untimed exhibition"],
        ),
    ] {
        assert_eq!(
            sorted_titles(&app, &format!("when={when}")).await,
            expected,
            "{when}"
        );
    }
    assert_counts_match_results(&app, "limit=1").await;
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn weekend_uses_the_clipped_london_date_range() {
    let Some(db) = TestDb::create("weekend_uses_the_clipped_london_date_range").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let run = |title, starts_at, ends_at| Ev {
        ends_at: Some(ends_at),
        category: "exhibition",
        ..Ev::one_off(title, starts_at)
    };
    for e in [
        run(
            "Mon–Fri run",
            "2026-10-18T23:00:00Z",
            "2026-10-22T23:00:00Z",
        ),
        run(
            "Fri–Sat run",
            "2026-10-22T23:00:00Z",
            "2026-10-23T23:00:00Z",
        ),
        Ev::one_off("UTC Friday, London Saturday", "2026-10-23T23:30:00Z"),
        run("long run", "2026-09-01T00:00:00Z", "2026-12-31T00:00:00Z"),
    ] {
        insert(&pool, e).await;
    }
    let app = app(&pool);
    assert_eq!(
        sorted_titles(&app, "when=weekend&from=2026-10-19&to=2026-10-31").await,
        ["Fri–Sat run", "UTC Friday, London Saturday", "long run"]
    );
    // Monday to Wednesday: the long run is open, but not on a weekend day.
    assert_eq!(
        sorted_titles(&app, "when=weekend&from=2026-10-26&to=2026-10-28").await,
        Vec::<String>::new()
    );
    assert_eq!(
        sorted_titles(&app, "from=2026-10-26&to=2026-10-28").await,
        ["long run"]
    );
    assert_counts_match_results(&app, "from=2026-10-19&to=2026-10-31").await;
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn price_max_and_counts() {
    let Some(db) = TestDb::create("price_max_and_counts").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let priced = |title, price_min, currency| Ev {
        price_min: Some(price_min),
        currency: Some(currency),
        ..Ev::one_off(title, "2026-10-01T18:00:00Z")
    };
    for e in [
        Ev {
            is_free: true,
            ..Ev::one_off("free", "2026-10-01T18:00:00Z")
        },
        priced("£5", "5", "GBP"),
        priced("£15", "15", "GBP"),
        priced("£25", "25", "GBP"),
        priced("€5", "5", "EUR"),
        // A price without a currency is taken to be in pounds.
        Ev {
            price_min: Some("7"),
            currency: None,
            ..Ev::one_off("no currency 7", "2026-10-01T18:00:00Z")
        },
        Ev::one_off("unknown", "2026-10-01T18:00:00Z"),
        // An untimed daytime event, so `when` facets differ.
        Ev {
            is_free: true,
            ..Ev::one_off("free daytime", "2026-10-01T23:00:00Z")
        },
    ] {
        insert(
            &pool,
            Ev {
                at: Some((51.5, -0.1)),
                ..e
            },
        )
        .await;
    }
    let app = app(&pool);
    assert_eq!(
        sorted_titles(&app, "price_max=10").await,
        ["free", "free daytime", "no currency 7", "£5"]
    );
    assert_eq!(
        sorted_titles(&app, "price_max=20").await,
        ["free", "free daytime", "no currency 7", "£15", "£5"]
    );
    assert_eq!(
        sorted_titles(&app, "price_max=10&when=evening").await,
        ["free", "no currency 7", "£5"]
    );

    let counts = |query: &'static str| {
        let app = app.clone();
        async move { get(&app, &format!("/v1/events?{query}")).await.1["counts"].clone() }
    };
    let price = json!({"free": 2, "max_10": 4, "max_20": 5, "unknown": 1});
    assert_eq!(counts("").await["price"], price);
    // Price counts ignore the active price filter.
    assert_eq!(counts("free=true").await["price"], price);
    assert_eq!(counts("price_max=10").await["price"], price);
    // … but apply the active `when`.
    assert_eq!(
        counts("when=evening").await["price"],
        json!({"free": 1, "max_10": 3, "max_20": 4, "unknown": 1})
    );
    // `when` counts apply the active price filter.
    assert_eq!(
        counts("").await["when"],
        json!({"evening": 7, "after_work": 7, "weekend": 0, "daytime": 1})
    );
    assert_eq!(
        counts("price_max=10").await["when"],
        json!({"evening": 3, "after_work": 3, "weekend": 0, "daytime": 1})
    );
    // Counts respect `near`.
    assert_eq!(counts("near=51.5,-0.1").await, counts("").await);
    assert_eq!(
        counts("near=51.6,-0.1&radius_km=2").await,
        json!({
            "when": {"evening": 0, "after_work": 0, "weekend": 0, "daytime": 0},
            "price": {"free": 0, "max_10": 0, "max_20": 0, "unknown": 0},
        })
    );
    for base in ["limit=1", "when=evening", "price_max=10", "near=51.5,-0.1"] {
        assert_counts_match_results(&app, base).await;
    }
    pool.close().await;
    db.drop_db().await;
}

async fn run(pool: &PgPool, key: &str, started_at: &str, events_found: i32, errors: i32, ok: bool) {
    let started_at = t(started_at);
    let source = musenmingle::repo::source_by_key(pool, key)
        .await
        .unwrap()
        .unwrap();
    musenmingle::repo::record_run(
        pool,
        &musenmingle::repo::NewRun {
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
            musenmingle::repo::source_by_key(&pool, key)
                .await
                .unwrap()
                .unwrap()
                .id
        }
    };
    musenmingle::repo::insert_health_issue(
        &pool,
        source_id("whitechapel-gallery").await,
        32,
        "errors",
    )
    .await
    .unwrap();
    let ticketmaster_id = source_id("ticketmaster").await;
    musenmingle::repo::insert_health_issue(&pool, ticketmaster_id, 31, "errors")
        .await
        .unwrap();
    let skipped_at = t("2026-09-26T06:30:00Z");
    musenmingle::repo::record_skip(
        &pool,
        ticketmaster_id,
        "TICKETMASTER_API_KEY not set",
        skipped_at,
    )
    .await
    .unwrap();
    // Unconfigured without any run.
    musenmingle::repo::record_skip(
        &pool,
        source_id("somerset-house").await,
        "no implementation for this source key",
        skipped_at,
    )
    .await
    .unwrap();
    let serpentine = source_id("serpentine-galleries").await;
    musenmingle::repo::insert_health_issue(&pool, serpentine, 7, "old")
        .await
        .unwrap();
    musenmingle::repo::close_health_issues(&pool, serpentine)
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
            "display_name": "Barbican",
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
        "https://github.com/alexsiri7/musenmingle/issues/32"
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
        "https://github.com/alexsiri7/musenmingle/issues/31"
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
        allow_origin("https://musenmingle.example").await.as_deref(),
        Some("https://musenmingle.example")
    );
    assert_eq!(allow_origin("https://evil.example").await, None);

    // Preflight for the suggestions form.
    let resp = app
        .clone()
        .oneshot(
            Request::options("/v1/suggestions")
                .header(header::ORIGIN, "https://musenmingle.example")
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
        "https://musenmingle.example"
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
