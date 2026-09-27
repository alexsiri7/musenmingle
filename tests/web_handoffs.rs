//! Share, Directions and Add-to-calendar hand-offs (issue #76) on event
//! cards and the detail page, `GET /events/{id}.ics` and the Open Graph
//! tags, against a real database.

mod common;

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Duration, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::SuggestionConfig;
use musenmingle::suggestions::Suggestions;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn app(pool: &PgPool) -> Router {
    let config = SuggestionConfig {
        ip_salt: Some("test-salt".into()),
        ..SuggestionConfig::default()
    };
    let settings = ApiSettings {
        github_repo: "alexsiri7/musenmingle".into(),
        cors_origins: Vec::new(),
    };
    musenmingle::api::router(
        pool.clone(),
        Suggestions::new(config, None).unwrap(),
        settings,
    )
    .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 1], 4000))))
}

struct Page {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn get(app: &Router, uri: &str) -> Page {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    Page {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn insert(
    pool: &PgPool,
    title: &str,
    starts_at: DateTime<Utc>,
    venue: Option<&str>,
    at: Option<(f64, f64)>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events
            (title, starts_at, ends_at, category, lat, lng, dedupe_key, venue_name,
             address, description)
         VALUES ($1, $2, $2 + interval '90 minutes', 'talk', $3, $4, $1, $5,
                 CASE WHEN $5 IS NULL THEN NULL ELSE 'Silk St, London EC2Y 8DS' END,
                 'An evening of talks, drinks; and \\ more.')
         RETURNING id",
    )
    .bind(title)
    .bind(starts_at)
    .bind(at.map(|a| a.0))
    .bind(at.map(|a| a.1))
    .bind(venue)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn link(pool: &PgPool, event: Uuid, source_key: &str, url: &str) {
    sqlx::query(
        "INSERT INTO events.event_sources
            (event_id, source_id, source_event_id, source_url, raw, first_seen_at, last_seen_at)
         SELECT $1, id, $3, $3, '{}', now(), now() FROM events.sources WHERE key = $2",
    )
    .bind(event)
    .bind(source_key)
    .bind(url)
    .execute(pool)
    .await
    .unwrap();
}

fn days(n: i64) -> DateTime<Utc> {
    (Utc::now() + Duration::days(n))
        .date_naive()
        .and_hms_opt(18, 30, 0)
        .unwrap()
        .and_utc()
}

#[tokio::test]
async fn detail_page_offers_share_directions_and_calendar() {
    let Some(db) = TestDb::create("detail_page_offers_share_directions_and_calendar").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let id = insert(
        &pool,
        "Glass & light, a talk",
        days(3),
        Some("Barbican Centre"),
        Some((51.5202, -0.0938)),
    )
    .await;
    link(&pool, id, "barbican", "https://www.barbican.org.uk/glass").await;
    let app = app(&pool);
    let p = get(&app, &format!("/events/{id}")).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    let b = &p.body;

    // Open Graph: absolute canonical URL on our host, our name, an excerpt.
    let canonical = format!("/events/{id}\"");
    let og_url = b
        .split("<meta property=\"og:url\" content=\"")
        .nth(1)
        .expect("og:url");
    assert!(og_url.starts_with("https://"), "{og_url}");
    assert!(og_url[..og_url.find('"').unwrap() + 1].ends_with(&canonical));
    assert!(b.contains("<meta property=\"og:site_name\" content=\"Muse &amp; Mingle\">"));
    assert!(b.contains("<meta property=\"og:title\" content=\"Glass &amp; light, a talk\">"));
    assert!(b.contains(
        "<meta property=\"og:description\" content=\"An evening of talks, drinks; and \\ more.\">"
    ));
    assert!(b.contains("<link rel=\"canonical\" href=\"https://"));

    // Share: hidden until app.js finds navigator.share or the clipboard.
    assert!(b.contains("<button type=\"button\" class=\"share\" hidden data-share-url=\"https://"));
    assert!(b.contains("data-share-title=\"Glass &amp; light, a talk\""));
    assert!(b.contains("data-share-text=\"Barbican Centre · "));
    assert!(b.contains("<span>Share</span>"));

    // Directions: Google by default, Apple for app.js to swap in.
    assert!(b.contains(
        "href=\"https://www.google.com/maps/dir/?api=1&amp;destination=51.5202%2C-0.0938&amp;travelmode=transit\" \
         data-apple-href=\"https://maps.apple.com/?daddr=51.5202%2C-0.0938&amp;dirflg=r\" rel=\"noopener noreferrer\""
    ), "{b}");
    assert!(b.contains(
        "href=\"https://www.google.com/maps/search/?api=1&amp;query=51.5202%2C-0.0938\" \
         data-apple-href=\"https://maps.apple.com/?q=Barbican+Centre&amp;ll=51.5202%2C-0.0938\""
    ));
    assert!(b.contains("View on OpenStreetMap</a>"));

    // Calendar: the .ics and Google Calendar, both plain links.
    assert!(b.contains(&format!("href=\"/events/{id}.ics\"")));
    assert!(b.contains(
        "href=\"https://calendar.google.com/calendar/render?action=TEMPLATE&amp;text=Glass+%26+light%2C+a+talk&amp;dates="
    ));
    // No tracking parameters on any hand-off.
    for bad in ["utm_", "fbclid", "gclid"] {
        assert!(!b.contains(bad), "{bad}");
    }
    assert_eq!(b.matches("<script").count(), 1);
    assert!(!b.contains(" onclick="));
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn ics_download_is_a_single_valid_vevent() {
    let Some(db) = TestDb::create("ics_download_is_a_single_valid_vevent").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let id = insert(
        &pool,
        "Glass & light, a talk",
        days(3),
        Some("Barbican Centre"),
        Some((51.5202, -0.0938)),
    )
    .await;
    link(&pool, id, "barbican", "https://www.barbican.org.uk/glass").await;
    let app = app(&pool);
    let p = get(&app, &format!("/events/{id}.ics")).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_eq!(
        p.headers[header::CONTENT_TYPE],
        "text/calendar; charset=utf-8"
    );
    assert_eq!(
        p.headers[header::CONTENT_DISPOSITION],
        "attachment; filename=\"glass-light-a-talk.ics\""
    );
    let body = &p.body;
    assert!(body.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
    assert!(body.ends_with("END:VEVENT\r\nEND:VCALENDAR\r\n"));
    assert_eq!(body.matches("BEGIN:VEVENT").count(), 1);
    for line in body.split("\r\n") {
        assert!(line.len() <= 75, "{line:?}");
        assert!(!line.contains('\n'));
    }
    let unfolded = body.replace("\r\n ", "");
    assert!(unfolded.contains(&format!("\r\nUID:{id}@musenmingle.interstellarai.net\r\n")));
    let start = days(3).format("%Y%m%dT%H%M%SZ").to_string();
    assert!(unfolded.contains(&format!("\r\nDTSTART:{start}\r\n")));
    assert!(unfolded.contains("\r\nSUMMARY:Glass & light\\, a talk\r\n"));
    assert!(unfolded.contains("\r\nLOCATION:Barbican Centre\\, Silk St\\, London EC2Y 8DS\r\n"));
    assert!(unfolded.contains("DESCRIPTION:An evening of talks\\, drinks\\; and \\\\ more."));
    assert!(unfolded.contains("\\n\\nvia Muse & Mingle: https://"));
    assert!(unfolded.contains("\r\nURL:https://www.barbican.org.uk/glass\r\n"));

    for uri in [
        format!("/events/{}.ics", Uuid::new_v4()),
        "/events/nope.ics".to_string(),
    ] {
        assert_eq!(get(&app, &uri).await.status, StatusCode::NOT_FOUND, "{uri}");
    }
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn cards_have_compact_hidden_share_and_plain_links() {
    let Some(db) = TestDb::create("cards_have_compact_hidden_share_and_plain_links").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let placed = insert(&pool, "Placed", days(1), Some("Barbican Centre"), None).await;
    let nowhere = insert(&pool, "Nowhere", days(2), None, None).await;
    let app = app(&pool);
    let p = get(&app, "/").await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    let b = &p.body;
    assert!(b.contains(
        "<button type=\"button\" class=\"share icon-action\" hidden data-share-url=\"https://"
    ));
    assert!(b.contains("<span class=\"vh\">Share: Placed</span>"));
    // No coordinates: directions use the address.
    assert!(b.contains(
        "href=\"https://www.google.com/maps/dir/?api=1&amp;destination=Barbican+Centre%2C+Silk+St%2C+London+EC2Y+8DS&amp;travelmode=transit\""
    ));
    assert!(b.contains("<span class=\"vh\">Directions to Placed</span>"));
    // No place at all: no directions, but the calendar link stays.
    assert!(!b.contains("Directions to Nowhere"));
    for id in [placed, nowhere] {
        assert!(b.contains(&format!(
            "<a class=\"icon-action\" href=\"/events/{id}.ics\">"
        )));
    }
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn about_says_the_hand_offs_carry_no_tracking() {
    let Some(db) = TestDb::create("about_says_the_hand_offs_carry_no_tracking").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let p = get(&app(&pool), "/about").await;
    let your_data = &p.body[p.body.find("id=\"your-data\"").unwrap()..];
    assert!(your_data.contains("we add no tracking parameters"));
    pool.close().await;
    db.drop_db().await;
}
