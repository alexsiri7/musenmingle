//! Per-source content policy (store_description / store_image, excerpts,
//! image provenance), the thumbnailer (wiremock image host) and how
//! thumbnails are served and credited on the pages and in the JSON.

mod common;

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use common::TestDb;
use sqlx::PgPool;
use thaleia::api::ApiSettings;
use thaleia::config::{RateLimitConfig, SuggestionConfig};
use thaleia::fetch::FetchContext;
use thaleia::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use thaleia::normalise::{EXCERPT_MAX_CHARS, dedupe_key};
use thaleia::repo::{self, SourcePolicy};
use thaleia::suggestions::Suggestions;
use thaleia::thumbs::{self, ThumbConfig};
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const POSTER: &[u8] = include_bytes!("fixtures/images/poster.jpg");

fn event(title: &str, days: i64) -> NewEvent {
    let starts_at = Utc::now() + Duration::days(days);
    NewEvent {
        title: title.into(),
        description: None,
        venue_name: Some("Hall".into()),
        address: None,
        lat: None,
        lng: None,
        starts_at,
        ends_at: None,
        price: Price::default(),
        url: None,
        image_url: None,
        category: Category::Talk,
        tags: vec![],
        dedupe_key: dedupe_key(title, starts_at, Some("Hall")),
    }
}

fn raw(id: &str, url: &str) -> RawEvent {
    RawEvent {
        source_event_id: id.into(),
        source_url: Some(url.into()),
        payload: serde_json::json!({ "id": id }),
    }
}

async fn source(pool: &PgPool, key: &str, kind: SourceKind, base: &str) -> i64 {
    repo::upsert_source(pool, key, kind, base, 1440, true)
        .await
        .unwrap()
        .id
}

async fn stored(pool: &PgPool, id: Uuid) -> (Option<String>, Option<String>, Option<i64>) {
    sqlx::query_as(
        "SELECT description, image_url, image_source_id FROM events.events WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn upsert_honours_policy_flags_and_cuts_descriptions_to_an_excerpt() {
    let Some(db) = TestDb::create("upsert_honours_policy_flags").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let open = source(
        &pool,
        "open-venue",
        SourceKind::Scraper,
        "https://open.test",
    )
    .await;
    let facts = source(
        &pool,
        "facts-only",
        SourceKind::Scraper,
        "https://facts.test",
    )
    .await;
    repo::set_source_policy(
        &pool,
        "facts-only",
        Some("Facts Only"),
        SourcePolicy {
            store_description: false,
            store_image: false,
        },
    )
    .await
    .unwrap();

    let long = format!(
        "{} The end. {}",
        "Words ".repeat(30),
        "More words ".repeat(60)
    );
    let mut a = event("Open talk", 3);
    a.description = Some(long.clone());
    a.image_url = Some("https://open.test/a.jpg".into());
    let oa = repo::upsert_event(&pool, open, &a, &raw("a", "https://open.test/a"))
        .await
        .unwrap();
    let (d, img, img_src) = stored(&pool, oa.event_id).await;
    let d = d.unwrap();
    assert!(d.chars().count() <= EXCERPT_MAX_CHARS, "{d}");
    assert!(d.ends_with("The end. …"), "{d}");
    assert_eq!(img.as_deref(), Some("https://open.test/a.jpg"));
    assert_eq!(img_src, Some(open));

    let mut b = event("Facts talk", 4);
    b.description = Some("Short.".into());
    b.image_url = Some("https://facts.test/b.jpg".into());
    let ob = repo::upsert_event(&pool, facts, &b, &raw("b", "https://facts.test/b"))
        .await
        .unwrap();
    assert_eq!(stored(&pool, ob.event_id).await, (None, None, None));
    let raws: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT source_event_id, raw FROM events.event_sources ORDER BY source_event_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        raws,
        [
            ("a".to_string(), serde_json::json!({ "id": "a" })),
            ("b".to_string(), repo::redacted_raw()),
        ],
        "a restricted source's raw payload is not kept"
    );

    // A short description is kept verbatim.
    let mut c = event("Short talk", 5);
    c.description = Some("Just this.".into());
    let oc = repo::upsert_event(&pool, open, &c, &raw("c", "https://open.test/c"))
        .await
        .unwrap();
    assert_eq!(
        stored(&pool, oc.event_id).await.0.as_deref(),
        Some("Just this.")
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn image_provenance_follows_the_image_that_wins_the_merge() {
    let Some(db) = TestDb::create("image_provenance_follows_merge").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let api = source(&pool, "some-api", SourceKind::Api, "https://api.test").await;
    let venue = source(
        &pool,
        "some-venue",
        SourceKind::Scraper,
        "https://venue.test",
    )
    .await;
    let mut from_api = event("Shared show", 6);
    from_api.image_url = Some("https://api.test/a.jpg".into());
    let mut from_venue = from_api.clone();
    from_venue.image_url = Some("https://venue.test/v.jpg".into());

    let o = repo::upsert_event(&pool, api, &from_api, &raw("x", "https://api.test/x"))
        .await
        .unwrap();
    assert_eq!(stored(&pool, o.event_id).await.2, Some(api));
    // The venue site wins images on merge: credit moves to it.
    repo::upsert_event(&pool, venue, &from_venue, &raw("y", "https://venue.test/y"))
        .await
        .unwrap();
    let s = stored(&pool, o.event_id).await;
    assert_eq!(
        (s.1.as_deref(), s.2),
        (Some("https://venue.test/v.jpg"), Some(venue))
    );
    // The API re-reporting its own image changes neither image nor credit.
    repo::upsert_event(&pool, api, &from_api, &raw("x", "https://api.test/x"))
        .await
        .unwrap();
    let s = stored(&pool, o.event_id).await;
    assert_eq!(
        (s.1.as_deref(), s.2),
        (Some("https://venue.test/v.jpg"), Some(venue))
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn enforce_content_policy_cleans_existing_rows() {
    let Some(db) = TestDb::create("enforce_content_policy_cleans_existing_rows").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let venue = source(&pool, "strict", SourceKind::Scraper, "https://strict.test").await;
    let o = repo::upsert_event(
        &pool,
        venue,
        &NewEvent {
            description: Some("Kept for now.".into()),
            image_url: Some("https://strict.test/i.jpg".into()),
            ..event("Old row", 2)
        },
        &raw("o", "https://strict.test/o"),
    )
    .await
    .unwrap();
    // A pre-policy long description, written directly.
    let long = "Lorem ipsum dolor sit amet ".repeat(40);
    let other = repo::upsert_event(
        &pool,
        venue,
        &event("Long row", 3),
        &raw("l", "https://strict.test/l"),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE events.events SET description = $2 WHERE id = $1")
        .bind(other.event_id)
        .bind(&long)
        .execute(&pool)
        .await
        .unwrap();
    let r = repo::enforce_content_policy(&pool).await.unwrap();
    assert_eq!(r.descriptions_trimmed, 1);
    assert_eq!(r.descriptions_cleared + r.images_cleared, 0);
    let trimmed = stored(&pool, other.event_id).await.0.unwrap();
    assert_eq!(trimmed, thaleia::normalise::excerpt(&long));

    // The source's terms turn out to forbid both: existing data goes.
    repo::set_source_policy(
        &pool,
        "strict",
        None,
        SourcePolicy {
            store_description: false,
            store_image: false,
        },
    )
    .await
    .unwrap();
    let r = repo::enforce_content_policy(&pool).await.unwrap();
    assert_eq!((r.descriptions_cleared, r.images_cleared), (2, 1));
    assert_eq!(r.raw_redacted, 2);
    assert_eq!(stored(&pool, o.event_id).await, (None, None, None));
    // Idempotent.
    assert_eq!(
        repo::enforce_content_policy(&pool).await.unwrap(),
        repo::PolicyReport::default()
    );
    pool.close().await;
    db.drop_db().await;
}

// ---------------------------------------------------------------- thumbnailer

struct ThumbSetup {
    db: TestDb,
    pool: PgPool,
    server: MockServer,
    event_id: Uuid,
    source_id: i64,
}

/// A venue whose one upcoming event has an image on a wiremock host.
async fn thumb_setup(name: &str, image_path: &str) -> Option<ThumbSetup> {
    let db = TestDb::create(name).await?;
    let pool = db.migrated_pool().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /private/\n"),
        )
        .mount(&server)
        .await;
    let source_id = source(&pool, "img-venue", SourceKind::Scraper, &server.uri()).await;
    repo::set_source_policy(
        &pool,
        "img-venue",
        Some("Image Venue"),
        SourcePolicy::default(),
    )
    .await
    .unwrap();
    let o = repo::upsert_event(
        &pool,
        source_id,
        &NewEvent {
            image_url: Some(format!("{}{image_path}", server.uri())),
            ..event("Poster show", 2)
        },
        &raw("p", "https://venue.test/whats-on/poster-show"),
    )
    .await
    .unwrap();
    Some(ThumbSetup {
        db,
        pool,
        server,
        event_id: o.event_id,
        source_id,
    })
}

fn ctx() -> FetchContext {
    FetchContext::new(RateLimitConfig::disabled()).unwrap()
}

#[derive(Debug, sqlx::FromRow)]
struct ThumbRow {
    source_id: Option<i64>,
    source_image_url: String,
    bytes: Option<Vec<u8>>,
    content_type: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    content_hash: Option<String>,
    etag: Option<String>,
    error: Option<String>,
}

async fn thumb_row(pool: &PgPool, id: Uuid) -> Option<ThumbRow> {
    sqlx::query_as(
        "SELECT source_id, source_image_url, bytes, content_type, width, height, content_hash,
                etag, error
         FROM events.thumbnails WHERE event_id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

fn image_response() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "image/jpeg")
        .insert_header("etag", "\"v1\"")
        .set_body_bytes(POSTER)
}

#[tokio::test]
async fn thumbnailer_fetches_once_resizes_and_refetches_only_on_url_change() {
    let Some(s) = thumb_setup("thumbnailer_fetches_once", "/img/poster.jpg").await else {
        return;
    };
    Mock::given(method("GET"))
        .and(path("/img/poster.jpg"))
        .respond_with(image_response())
        .expect(1)
        .mount(&s.server)
        .await;
    let cfg = ThumbConfig::default();
    let now = Utc::now();
    let r = thumbs::run(&s.pool, &ctx(), &cfg, now).await.unwrap();
    assert_eq!((r.made, r.failed), (1, 0));
    let row = thumb_row(&s.pool, s.event_id).await.unwrap();
    let bytes = row.bytes.clone().unwrap();
    assert_eq!(row.content_type.as_deref(), Some("image/jpeg"));
    assert_eq!((row.width, row.height), (Some(480), Some(336)));
    assert!(bytes.len() < 40 * 1024, "{} bytes", bytes.len());
    assert_eq!(row.source_id, Some(s.source_id));
    assert_eq!(row.etag.as_deref(), Some("\"v1\""));
    assert_eq!(row.error, None);
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (480, 336));

    // Second run: same URL, nothing fetched (wiremock expects exactly 1).
    let r = thumbs::run(&s.pool, &ctx(), &cfg, now).await.unwrap();
    assert_eq!(r, thumbs::ThumbReport::default());
    s.server.verify().await;

    // The venue changes the image: fetched again.
    Mock::given(method("GET"))
        .and(path("/img/poster-v2.jpg"))
        .respond_with(image_response())
        .expect(1)
        .mount(&s.server)
        .await;
    let new_url = format!("{}/img/poster-v2.jpg", s.server.uri());
    repo::upsert_event(
        &s.pool,
        s.source_id,
        &NewEvent {
            image_url: Some(new_url.clone()),
            ..event("Poster show", 2)
        },
        &raw("p", "https://venue.test/whats-on/poster-show"),
    )
    .await
    .unwrap();
    let r = thumbs::run(&s.pool, &ctx(), &cfg, now).await.unwrap();
    assert_eq!(r.made, 1);
    assert_eq!(
        thumb_row(&s.pool, s.event_id)
            .await
            .unwrap()
            .source_image_url,
        new_url
    );
    s.server.verify().await;
    s.pool.close().await;
    s.db.drop_db().await;
}

#[tokio::test]
async fn thumbnailer_skips_oversized_images_and_robots_disallowed_ones() {
    let Some(s) = thumb_setup("thumbnailer_skips_oversized", "/img/huge.jpg").await else {
        return;
    };
    Mock::given(method("GET"))
        .and(path("/img/huge.jpg"))
        .respond_with(image_response())
        .expect(1)
        .mount(&s.server)
        .await;
    let cfg = ThumbConfig {
        max_source_bytes: 10_000,
        ..ThumbConfig::default()
    };
    let now = Utc::now();
    let r = thumbs::run(&s.pool, &ctx(), &cfg, now).await.unwrap();
    assert_eq!((r.made, r.failed), (0, 1));
    let row = thumb_row(&s.pool, s.event_id).await.unwrap();
    assert!(row.bytes.is_none());
    assert!(row.error.unwrap().contains("larger than 10000 bytes"));
    // Not retried on the next run (same URL, within the retry window).
    let r = thumbs::run(&s.pool, &ctx(), &cfg, now).await.unwrap();
    assert_eq!(r, thumbs::ThumbReport::default());
    s.server.verify().await;

    // robots.txt disallows the image path: never fetched.
    Mock::given(method("GET"))
        .and(path("/private/p.jpg"))
        .respond_with(image_response())
        .expect(0)
        .mount(&s.server)
        .await;
    repo::upsert_event(
        &s.pool,
        s.source_id,
        &NewEvent {
            image_url: Some(format!("{}/private/p.jpg", s.server.uri())),
            ..event("Private poster", 3)
        },
        &raw("q", "https://venue.test/q"),
    )
    .await
    .unwrap();
    let r = thumbs::run(&s.pool, &ctx(), &ThumbConfig::default(), now)
        .await
        .unwrap();
    assert_eq!((r.made, r.failed), (0, 1));
    s.server.verify().await;
    s.pool.close().await;
    s.db.drop_db().await;
}

#[tokio::test]
async fn thumbnailer_caps_work_per_host() {
    let Some(s) = thumb_setup("thumbnailer_caps_work_per_host", "/img/0.jpg").await else {
        return;
    };
    for i in 0..5 {
        Mock::given(method("GET"))
            .and(path(format!("/img/{i}.jpg")))
            .respond_with(image_response())
            .mount(&s.server)
            .await;
    }
    for i in 1..5 {
        repo::upsert_event(
            &s.pool,
            s.source_id,
            &NewEvent {
                image_url: Some(format!("{}/img/{i}.jpg", s.server.uri())),
                ..event(&format!("Show {i}"), 2 + i)
            },
            &raw(&format!("s{i}"), "https://venue.test/s"),
        )
        .await
        .unwrap();
    }
    let cfg = ThumbConfig {
        per_host: 2,
        ..ThumbConfig::default()
    };
    let r = thumbs::run(&s.pool, &ctx(), &cfg, Utc::now())
        .await
        .unwrap();
    assert_eq!((r.made, r.deferred), (2, 3));
    let r = thumbs::run(&s.pool, &ctx(), &cfg, Utc::now())
        .await
        .unwrap();
    assert_eq!((r.made, r.deferred), (2, 1));
    s.pool.close().await;
    s.db.drop_db().await;
}

// ------------------------------------------------------------ serving & pages

fn app(pool: &PgPool) -> Router {
    thaleia::api::router(
        pool.clone(),
        Suggestions::new(
            SuggestionConfig {
                ip_salt: Some("salt".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap(),
        ApiSettings {
            github_repo: "alexsiri7/thaleia".into(),
            cors_origins: Vec::new(),
        },
    )
    .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 1], 4000))))
}

async fn get(
    app: &Router,
    uri: &str,
    if_none_match: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::get(uri);
    if let Some(v) = if_none_match {
        req = req.header(header::IF_NONE_MATCH, v);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), 4 << 20)
        .await
        .unwrap()
        .to_vec();
    (status, headers, body)
}

#[tokio::test]
async fn thumbnails_are_served_cached_and_credited_never_hotlinked() {
    let Some(s) = thumb_setup("thumbnails_are_served_and_credited", "/img/poster.jpg").await else {
        return;
    };
    Mock::given(method("GET"))
        .and(path("/img/poster.jpg"))
        .respond_with(image_response())
        .mount(&s.server)
        .await;
    thumbs::run(&s.pool, &ctx(), &ThumbConfig::default(), Utc::now())
        .await
        .unwrap();
    let row = thumb_row(&s.pool, s.event_id).await.unwrap();
    let hash = row.content_hash.unwrap();
    let thumb_url = thumbs::thumb_path(s.event_id, &hash);
    let app = app(&s.pool);

    // Hashed URL: immutable, with an ETag that revalidates.
    let (status, h, body) = get(&app, &thumb_url, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(
        h[header::CACHE_CONTROL],
        "public, max-age=604800, immutable"
    );
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(etag, format!("\"{hash}\""));
    assert_eq!(body, row.bytes.unwrap());
    let (status, h, body) = get(&app, &thumb_url, Some(&etag)).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert_eq!(h[header::ETAG], etag.as_str());
    assert!(body.is_empty());
    // Bare id: same bytes, not immutable.
    let (status, h, _) = get(&app, &format!("/thumbs/{}", s.event_id), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h[header::CACHE_CONTROL], "public, max-age=604800");
    // Stale hash, unknown id, junk: 404.
    for uri in [
        format!("/thumbs/{}-0000000000000000.jpg", s.event_id),
        format!("/thumbs/{}", Uuid::new_v4()),
        "/thumbs/nope".to_string(),
    ] {
        assert_eq!(
            get(&app, &uri, None).await.0,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }

    // Home page: our thumbnail with a credit linking to the event's page on
    // the source, the source CTA, and no external image anywhere.
    let (status, h, body) = get(&app, "/", None).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8(body).unwrap();
    let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("img-src 'self';"), "{csp}");
    assert!(
        html.contains(&format!(
            "<img src=\"{thumb_url}\" alt=\"\" loading=\"lazy\" decoding=\"async\" width=\"480\" height=\"336\">"
        )),
        "{html}"
    );
    assert!(
        html.contains(
            "<figcaption class=\"credit\">Image: <a href=\"https://venue.test/whats-on/poster-show\" rel=\"noopener\">Image Venue</a></figcaption>"
        ),
        "{html}"
    );
    assert!(html.contains("See it on Image Venue →"), "{html}");
    assert!(!html.contains(&s.server.uri()), "source image URL leaked");
    for img in html.split("<img ").skip(1) {
        assert!(img.starts_with("src=\"/thumbs/"), "{img}");
    }
    // Detail page too.
    let (_, _, body) = get(&app, &format!("/events/{}", s.event_id), None).await;
    let html = String::from_utf8(body).unwrap();
    assert!(
        html.contains(&format!("<img src=\"{thumb_url}\"")),
        "{html}"
    );
    assert!(html.contains("Image: <a href=\"https://venue.test/whats-on/poster-show\""));
    assert!(!html.contains(&s.server.uri()));

    // JSON: local thumbnail + credit, never the source image URL.
    let (_, _, body) = get(&app, &format!("/v1/events/{}", s.event_id), None).await;
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["thumbnail_url"], thumb_url.as_str());
    assert_eq!(
        json["image_credit"],
        serde_json::json!({
            "name": "Image Venue",
            "url": "https://venue.test/whats-on/poster-show",
        })
    );
    assert!(json.get("image_url").is_none(), "{json}");
    let (_, _, body) = get(&app, "/v1/events", None).await;
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(list["events"][0]["thumbnail_url"], thumb_url.as_str());
    assert!(list["events"][0].get("image_url").is_none());

    // If the source's policy changes to "no images", the thumbnail goes.
    repo::set_source_policy(
        &s.pool,
        "img-venue",
        Some("Image Venue"),
        SourcePolicy {
            store_description: true,
            store_image: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(get(&app, &thumb_url, None).await.0, StatusCode::NOT_FOUND);
    let r = repo::enforce_content_policy(&s.pool).await.unwrap();
    assert_eq!((r.images_cleared, r.thumbnails_deleted), (1, 1));
    let (_, _, body) = get(&app, "/", None).await;
    assert!(!String::from_utf8(body).unwrap().contains("<img"));
    s.pool.close().await;
    s.db.drop_db().await;
}
