//! The map (#71): `at=now|today` filter semantics, venue coordinate
//! backfill, `/map` without JavaScript, its headers, and the self-hosted
//! tiles (HTTP range requests) and map assets.

mod common;

use std::time::Duration as StdDuration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::SuggestionConfig;
use musenmingle::listing::parse_query_at;
use musenmingle::model::{Category, NewEvent, Price, RawEvent};
use musenmingle::repo;
use musenmingle::suggestions::Suggestions;
use musenmingle::web::map::{MAP_CSP, MAP_PERMISSIONS_POLICY};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn router(pool: PgPool, tiles: Option<&std::path::Path>) -> Router {
    let suggestions = Suggestions::new(
        SuggestionConfig {
            ip_salt: Some("salt".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let settings = ApiSettings {
        github_repo: "owner/repo".into(),
        cors_origins: Vec::new(),
    };
    musenmingle::api::router_with_tiles(pool, suggestions, settings, tiles)
}

/// A database that is never reached (for routes that don't query it).
fn offline_pool() -> PgPool {
    PgPoolOptions::new()
        .acquire_timeout(StdDuration::from_millis(300))
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap()
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, bytes.to_vec())
}

async fn get(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

async fn insert(
    pool: &PgPool,
    title: &str,
    starts: &str,
    ends: Option<&str>,
    all_day: bool,
    at: Option<(f64, f64)>,
) {
    sqlx::query(
        "INSERT INTO events.events (title, starts_at, ends_at, all_day, category, lat, lng, dedupe_key)
         VALUES ($1, $2, $3, $4, 'exhibition', $5, $6, $1)",
    )
    .bind(title)
    .bind(t(starts))
    .bind(ends.map(t))
    .bind(all_day)
    .bind(at.map(|a| a.0))
    .bind(at.map(|a| a.1))
    .execute(pool)
    .await
    .unwrap();
}

async fn titles_at(pool: &PgPool, raw: &str, now: &str) -> Vec<String> {
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
async fn at_now_and_today_follow_london_days_and_ranges() {
    let Some(db) = TestDb::create("at_now_and_today_follow_london_days_and_ranges").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    // Sat 3 Oct 2026 (BST). All-day rows are London midnights, end inclusive.
    let evs: [(&str, &str, Option<&str>, bool); 10] = [
        ("all-day today", "2026-10-02T23:00:00Z", None, true),
        (
            "run ends today",
            "2026-09-01T23:00:00Z",
            Some("2026-10-02T23:00:00Z"),
            true,
        ),
        (
            "run ended yesterday",
            "2026-09-01T23:00:00Z",
            Some("2026-10-01T23:00:00Z"),
            true,
        ),
        (
            "timed run on",
            "2025-02-10T10:00:00Z",
            Some("2027-01-01T10:00:00Z"),
            false,
        ),
        ("talk in 2h", "2026-10-03T15:00:00Z", None, false),
        ("talk in 4h", "2026-10-03T17:00:00Z", None, false),
        ("talk started 30m ago", "2026-10-03T12:30:00Z", None, false),
        ("talk started 2h ago", "2026-10-03T11:00:00Z", None, false),
        (
            "workshop ended",
            "2026-10-03T09:00:00Z",
            Some("2026-10-03T12:00:00Z"),
            false,
        ),
        ("all-day tomorrow", "2026-10-03T23:00:00Z", None, true),
    ];
    for (title, s, e, all_day) in evs {
        insert(&pool, title, s, e, all_day, Some((51.51, -0.12))).await;
    }
    let now = "2026-10-03T13:00:00Z"; // 14:00 London
    assert_eq!(
        titles_at(&pool, "at=now", now).await,
        [
            "all-day today",
            "run ends today",
            "talk in 2h",
            "talk started 30m ago",
            "timed run on"
        ]
    );
    assert_eq!(
        titles_at(&pool, "at=now&within_hours=5", now).await,
        [
            "all-day today",
            "run ends today",
            "talk in 2h",
            "talk in 4h",
            "talk started 30m ago",
            "timed run on"
        ]
    );
    assert_eq!(
        titles_at(&pool, "at=today", now).await,
        [
            "all-day today",
            "run ends today",
            "talk in 2h",
            "talk in 4h",
            "talk started 30m ago",
            "timed run on"
        ]
    );
    // With `near`, the same window, nearest first.
    assert_eq!(
        titles_at(&pool, "at=now&near=51.51,-0.12&radius_km=1", now)
            .await
            .len(),
        5
    );

    // Sun 25 Oct 2026 has 25 hours (BST ends): an all-day event that day is
    // on until London midnight (00:00 GMT on the 26th), not 24 h after its start.
    insert(
        &pool,
        "all-day on the long day",
        "2026-10-24T23:00:00Z",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(
        titles_at(&pool, "at=now", "2026-10-25T23:30:00Z").await,
        ["all-day on the long day", "timed run on"]
    );
    db.drop_db().await;
}

fn new_event(venue: &str, at: Option<(f64, f64)>) -> NewEvent {
    let starts_at = t("2026-10-10T18:00:00Z");
    let title = format!("A talk at {venue}");
    NewEvent {
        dedupe_key: musenmingle::normalise::dedupe_key(&title, starts_at, Some(venue)),
        title,
        description: None,
        venue_name: Some(venue.into()),
        address: None,
        lat: at.map(|a| a.0),
        lng: at.map(|a| a.1),
        starts_at,
        ends_at: None,
        all_day: false,
        price: Price::default(),
        url: None,
        image_url: None,
        category: Category::Talk,
        tags: vec![],
    }
}

#[tokio::test]
async fn venues_fill_missing_coordinates_at_upsert() {
    let Some(db) = TestDb::create("venues_fill_missing_coordinates_at_upsert").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let source = repo::source_by_key(&pool, "horse-hospital")
        .await
        .unwrap()
        .unwrap()
        .id;
    let coords = |venue: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (Option<f64>, Option<f64>)>(
                "SELECT lat, lng FROM events.events WHERE venue_name = $1",
            )
            .bind(venue)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let raw = |id: &str| RawEvent {
        source_event_id: id.into(),
        source_url: None,
        payload: serde_json::json!({}),
    };
    // Seeded venue, matched on the normalised name ("The" dropped, any case).
    repo::upsert_event(
        &pool,
        source,
        &new_event("the HORSE hospital", None),
        &raw("a"),
    )
    .await
    .unwrap();
    assert_eq!(
        coords("the HORSE hospital").await,
        (Some(51.5227858), Some(-0.1243617))
    );
    // A source's own coordinates win.
    repo::upsert_event(
        &pool,
        source,
        &new_event("Housmans Bookshop", Some((51.6, -0.2))),
        &raw("b"),
    )
    .await
    .unwrap();
    assert_eq!(coords("Housmans Bookshop").await, (Some(51.6), Some(-0.2)));
    // Unknown venues stay without coordinates.
    repo::upsert_event(&pool, source, &new_event("Nowhere Hall", None), &raw("c"))
        .await
        .unwrap();
    assert_eq!(coords("Nowhere Hall").await, (None, None));
    // Rows stored before their venue was added get it the next time the
    // source lists them (the refresh path).
    sqlx::query(
        "INSERT INTO events.venues (name, lat, lng, coords_source) VALUES ('Nowhere Hall', 51.5, -0.1, 'test')",
    )
    .execute(&pool)
    .await
    .unwrap();
    repo::upsert_event(&pool, source, &new_event("Nowhere Hall", None), &raw("c"))
        .await
        .unwrap();
    assert_eq!(coords("Nowhere Hall").await, (Some(51.5), Some(-0.1)));
    db.drop_db().await;
}

#[tokio::test]
async fn map_page_lists_events_without_javascript() {
    let Some(db) = TestDb::create("map_page_lists_events_without_javascript").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let now = Utc::now();
    let iso = |d: chrono::Duration| (now + d).to_rfc3339();
    // On now, at Somerset House (Central); one far away; one tomorrow.
    insert(
        &pool,
        "Ongoing show",
        &iso(chrono::Duration::days(-3)),
        Some(&iso(chrono::Duration::days(20))),
        false,
        Some((51.5110, -0.1171)),
    )
    .await;
    insert(
        &pool,
        "Far away show",
        &iso(chrono::Duration::days(-3)),
        Some(&iso(chrono::Duration::days(20))),
        false,
        Some((51.40, -0.30)),
    )
    .await;
    insert(
        &pool,
        "Next week talk",
        &iso(chrono::Duration::days(7)),
        None,
        false,
        Some((51.5110, -0.1171)),
    )
    .await;
    let app = router(pool.clone(), None);
    let (status, headers, body) = get(&app, "/map").await;
    let body = String::from_utf8(body).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers[header::CONTENT_SECURITY_POLICY], MAP_CSP);
    assert_eq!(headers["permissions-policy"], MAP_PERMISSIONS_POLICY);
    assert!(!MAP_CSP.contains("unsafe-inline") && !MAP_CSP.contains("http"));
    assert!(body.contains("Ongoing show"), "{body}");
    assert!(!body.contains("Far away show"));
    assert!(!body.contains("Next week talk"));
    assert!(body.contains("km from area centre"));
    assert!(body.contains("Open today · check opening hours"));
    // JS-only controls are hidden; the map needs tiles, which this app lacks.
    assert!(
        body.contains("id=\"locate\" class=\"locate\" hidden"),
        "{body}"
    );
    assert!(!body.contains("id=\"map-pane\""));
    // Area presets and "All today" are plain links.
    assert!(body.contains("href=\"/map?area=east\""));
    assert!(body.contains("href=\"/map?area=central&amp;span=today\""));
    // No third-party hosts anywhere but links out.
    for external in ["googleapis", "unpkg", "cdn.", "tile.openstreetmap"] {
        assert!(!body.contains(external), "{external}");
    }

    // Other pages keep geolocation off.
    let (_, headers, _) = get(&app, "/saved").await;
    assert_eq!(
        headers["permissions-policy"],
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()"
    );
    assert_ne!(headers[header::CONTENT_SECURITY_POLICY], MAP_CSP);

    // With tiles, the (hidden until JS) map pane and its scripts are there.
    let dir = tempdir("map_page");
    let tiles = dir.join("london.pmtiles");
    std::fs::write(&tiles, fake_pmtiles()).unwrap();
    let app = router(pool.clone(), Some(&tiles));
    let (_, _, body) = get(&app, "/map?area=central").await;
    let body = String::from_utf8(body).unwrap();
    assert!(
        body.contains("id=\"map-pane\" hidden data-tiles=\"/tiles/london.pmtiles?v="),
        "{body}"
    );
    assert!(body.contains("<script type=\"module\" src=\"/static/map/map.mjs?v="));
    assert!(body.contains("https://www.openstreetmap.org/copyright"));
    db.drop_db().await;
}

fn fake_pmtiles() -> Vec<u8> {
    let mut v = b"PMTiles".to_vec();
    v.extend((0..1000u32).map(|i| (i % 251) as u8));
    v
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("map_near_{name}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn tiles_are_served_with_range_requests() {
    let dir = tempdir("tiles");
    let path = dir.join("london.pmtiles");
    let bytes = fake_pmtiles();
    std::fs::write(&path, &bytes).unwrap();
    let app = router(offline_pool(), Some(&path));

    let range = |r: &'static str| {
        Request::get("/tiles/london.pmtiles")
            .header(header::RANGE, r)
            .body(Body::empty())
            .unwrap()
    };
    let (status, headers, body) = send(&app, range("bytes=0-6")).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, b"PMTiles");
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 0-6/1007");
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=3600");
    let (status, _, body) = send(&app, range("bytes=1000-")).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, &bytes[1000..]);
    let (status, headers, _) = send(&app, range("bytes=5000-6000")).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes */1007");
    let (status, headers, body) = get(&app, "/tiles/london.pmtiles").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, bytes);
    assert_eq!(headers[header::CONTENT_TYPE], "application/vnd.pmtiles");
    // The page links a versioned URL, cached for a year.
    let v = {
        use sha2::{Digest, Sha256};
        let h = Sha256::digest(&bytes);
        h[..6]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let (status, headers, _) = get(&app, &format!("/tiles/london.pmtiles?v={v}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );

    // No tiles configured (or a missing file): 404.
    let missing = dir.join("missing.pmtiles");
    for app in [
        router(offline_pool(), None),
        router(offline_pool(), Some(&missing)),
    ] {
        let (status, _, _) = get(&app, "/tiles/london.pmtiles").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn map_assets_and_glyphs_are_self_hosted() {
    let app = router(offline_pool(), None);
    for (uri, ty) in [
        (
            "/static/map/maplibre-gl-6.11.2/maplibre-gl.mjs",
            "text/javascript; charset=utf-8",
        ),
        (
            "/static/map/maplibre-gl-6.11.2/maplibre-gl-worker.mjs",
            "text/javascript; charset=utf-8",
        ),
        (
            "/static/map/pmtiles-4.5.0/pmtiles.js",
            "text/javascript; charset=utf-8",
        ),
        ("/static/map/map.mjs", "text/javascript; charset=utf-8"),
        ("/static/map/style-light.json", "application/json"),
        ("/static/map/style-dark.json", "application/json"),
    ] {
        let (status, headers, body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(headers[header::CONTENT_TYPE], ty, "{uri}");
        // The worker's own CSP lets it import its shared module (same origin only).
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            musenmingle::web::map::ASSET_CSP,
            "{uri}"
        );
        assert!(!body.is_empty(), "{uri}");
    }
    // The styles point only at our own tiles and glyphs.
    for style in ["style-light.json", "style-dark.json"] {
        let (_, _, body) = get(&app, &format!("/static/map/{style}")).await;
        let s = String::from_utf8(body).unwrap();
        assert!(!s.contains("http"), "{style} references a remote URL");
        assert!(
            !s.contains("sprite") && !s.contains("icon-image"),
            "{style}"
        );
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(
            v["sources"]["protomaps"]["url"],
            "pmtiles:///tiles/london.pmtiles"
        );
    }
    let (status, _, body) = get(&app, "/static/map-glyphs/Noto%20Sans%20Regular/0-255.pbf").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.len() > 10_000);
    // Scripts we don't ship get an empty glyph range, not an error.
    let (status, _, body) = get(
        &app,
        "/static/map-glyphs/Noto%20Sans%20Devanagari%20Regular%20v1/2304-2559.pbf",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    let (status, _, _) = get(&app, "/static/map/../Cargo.toml").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn map_list_leads_to_our_detail_page() {
    let Some(db) = TestDb::create("map_list_leads_to_our_detail_page").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let source = repo::source_by_key(&pool, "horse-hospital")
        .await
        .unwrap()
        .unwrap()
        .id;
    let now = Utc::now();
    let mut ev = new_event("Somerset House", Some((51.5110, -0.1171)));
    ev.starts_at = now - chrono::Duration::days(3);
    ev.ends_at = Some(now + chrono::Duration::days(20));
    ev.category = Category::Exhibition;
    ev.url = Some("https://venue.test/whats-on/show".into());
    let raw = RawEvent {
        source_event_id: "show".into(),
        source_url: Some("https://venue.test/whats-on/show".into()),
        payload: serde_json::json!({}),
    };
    repo::upsert_event(&pool, source, &ev, &raw).await.unwrap();
    let app = router(pool.clone(), None);
    let (status, _, body) = get(&app, "/map?area=central").await;
    let body = String::from_utf8(body).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let links = &body[body.find("class=\"near-links\"").expect("near-links")..];
    let details = links.find("href=\"/events/").expect("detail link");
    let venue = links
        .find("<a class=\"near-source\" href=\"https://venue.test/whats-on/show\" rel=\"noopener\">See it on ")
        .expect("venue link");
    assert!(details < venue, "{links}");
    assert!(
        links.starts_with("class=\"near-links\"><a class=\"arrow-link\" href=\"/events/"),
        "{links}"
    );
    db.drop_db().await;
}
