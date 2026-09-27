//! Multi-session events (#207) against a real database: sessions are
//! stored and replaced with the dates, and `at=`, `from`/`to`, `open_on`,
//! `when=weekend`, the calendar, the event page and its `.ics` use the
//! sessions, not the envelope (so the gaps between sessions are not "on").
//! The sessions straddle the BST → GMT change of 25 Oct 2026.

mod common;

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::SuggestionConfig;
use musenmingle::listing::parse_query_at;
use musenmingle::model::{Category, NewEvent, Price, RawEvent, Session, SourceKind};
use musenmingle::normalise::london_to_utc;
use musenmingle::repo;
use musenmingle::suggestions::Suggestions;
use sqlx::PgPool;
use tower::ServiceExt;

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn event(title: &str, starts_at: DateTime<Utc>, ends_at: DateTime<Utc>, all_day: bool) -> NewEvent {
    NewEvent {
        sessions: Vec::new(),
        title: title.into(),
        description: None,
        venue_name: Some("The Showroom".into()),
        address: None,
        lat: None,
        lng: None,
        starts_at,
        ends_at: Some(ends_at),
        all_day,
        price: Price::default(),
        url: Some("https://example.org/x".into()),
        image_url: None,
        category: Category::Workshop,
        tags: Vec::new(),
        dedupe_key: title.into(),
    }
}

fn session(start: DateTime<Utc>, end: DateTime<Utc>) -> Session {
    Session {
        starts_at: start,
        ends_at: Some(end),
    }
}

/// Two Tuesday sessions, 16:30–18:30 London: 20 Oct (BST) and 3 Nov (GMT).
fn series() -> NewEvent {
    let mut e = event(
        "series",
        t("2026-10-19T23:00:00Z"),
        t("2026-11-03T00:00:00Z"),
        true,
    );
    e.set_sessions(vec![
        session(t("2026-11-03T16:30:00Z"), t("2026-11-03T18:30:00Z")),
        session(t("2026-10-20T15:30:00Z"), t("2026-10-20T17:30:00Z")),
    ]);
    e
}

async fn ingest(pool: &PgPool, source: i64, e: &NewEvent) -> uuid::Uuid {
    let raw = RawEvent {
        source_event_id: e.title.clone(),
        source_url: None,
        payload: serde_json::json!({}),
    };
    repo::upsert_event(pool, source, e, &raw)
        .await
        .unwrap()
        .event_id
}

async fn titles(pool: &PgPool, raw: &str, now: &str) -> Vec<String> {
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

async fn get(app: &Router, uri: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn sessions_drive_listings_and_are_replaced_with_the_dates() {
    let Some(db) = TestDb::create("sessions_drive_listings").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::upsert_source(
        &pool,
        "sessions-test",
        SourceKind::Scraper,
        "https://example.org",
        60,
        true,
    )
    .await
    .unwrap()
    .id;
    let id = ingest(&pool, src, &series()).await;
    // The same dates as one all-day run, for contrast.
    ingest(
        &pool,
        src,
        &event(
            "range",
            t("2026-10-19T23:00:00Z"),
            t("2026-11-03T00:00:00Z"),
            true,
        ),
    )
    .await;

    let stored = repo::get_event(&pool, id).await.unwrap().unwrap();
    assert!(!stored.all_day);
    assert_eq!(stored.starts_at, t("2026-10-20T15:30:00Z"));
    assert_eq!(stored.ends_at, Some(t("2026-11-03T18:30:00Z")));
    let sessions = stored.sessions.expect("sessions").0;
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].starts_at, t("2026-10-20T15:30:00Z"));

    let now = "2026-09-27T12:00:00Z";
    // On now: during each session (BST, then GMT), not in the gap.
    assert_eq!(
        titles(&pool, "at=now", "2026-10-20T16:00:00Z").await,
        ["range", "series"]
    );
    assert_eq!(
        titles(&pool, "at=now", "2026-10-27T16:00:00Z").await,
        ["range"]
    );
    assert_eq!(
        titles(&pool, "at=now", "2026-11-03T18:00:00Z").await,
        ["range", "series"]
    );
    // Date windows: the gap has no session.
    assert_eq!(
        titles(&pool, "from=2026-10-21&to=2026-11-02", now).await,
        ["range"]
    );
    assert_eq!(
        titles(&pool, "from=2026-11-03&to=2026-11-03", now).await,
        ["range", "series"]
    );
    // Weekdays and weekends: sessions are on Tuesdays only.
    assert_eq!(
        titles(&pool, "open_on=tue&from=2026-10-01", now).await,
        ["range", "series"]
    );
    assert_eq!(
        titles(&pool, "open_on=wed&from=2026-10-01", now).await,
        ["range"]
    );
    assert_eq!(
        titles(&pool, "when=weekend&from=2026-10-01", now).await,
        ["range"]
    );

    // The source drops the session list: back to a plain run.
    ingest(
        &pool,
        src,
        &event(
            "series",
            t("2026-10-19T23:00:00Z"),
            t("2026-11-03T00:00:00Z"),
            true,
        ),
    )
    .await;
    let stored = repo::get_event(&pool, id).await.unwrap().unwrap();
    assert!(stored.sessions.is_none());
    assert!(stored.all_day);
    assert_eq!(
        titles(&pool, "at=now", "2026-10-27T16:00:00Z").await,
        ["range", "series"]
    );
    db.drop_db().await;
}

#[tokio::test]
async fn pages_show_the_next_session_and_place_each_session() {
    let Some(db) = TestDb::create("sessions_on_pages").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let src = repo::upsert_source(
        &pool,
        "sessions-pages",
        SourceKind::Scraper,
        "https://example.org",
        60,
        true,
    )
    .await
    .unwrap()
    .id;
    let today: NaiveDate = Utc::now()
        .with_timezone(&chrono_tz::Europe::London)
        .date_naive();
    let at = |days: i64, h: u32| {
        london_to_utc(
            (today + Duration::days(days)).and_time(NaiveTime::from_hms_opt(h, 0, 0).unwrap()),
        )
    };
    let mut e = event("Fortnightly Makers", at(10, 0), at(24, 0), true);
    e.set_sessions(vec![
        session(at(10, 18), at(10, 20)),
        session(at(24, 18), at(24, 20)),
    ]);
    let id = ingest(&pool, src, &e).await;
    let app = app(&pool);

    let (status, html) = get(&app, &format!("/events/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Next session: "), "{html}");
    assert!(html.contains(" · 2 sessions"));
    assert!(html.contains("class=\"sub sessions\""));

    let (_, json) = get(&app, &format!("/v1/events/{id}")).await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["sessions"].as_array().map(Vec::len), Some(2));

    let (_, ics) = get(&app, &format!("/events/{id}.ics")).await;
    assert_eq!(ics.matches("BEGIN:VEVENT").count(), 2);

    // Week view: the weeks of each session, not the week between them.
    let day = |d: i64| (today + Duration::days(d)).format("%Y-%m-%d").to_string();
    let (_, gap) = get(&app, &format!("/calendar?view=week&date={}", day(17))).await;
    assert!(!gap.contains("Fortnightly Makers"));
    let (_, first) = get(&app, &format!("/calendar?view=week&date={}", day(10))).await;
    assert!(first.contains("Fortnightly Makers"));
    let (_, second) = get(&app, &format!("/calendar?view=week&date={}", day(24))).await;
    assert!(second.contains("Fortnightly Makers"));
    db.drop_db().await;
}
