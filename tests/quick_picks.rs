//! Home-page quick-pick chips (`pick=`, #82) against a real database: each
//! chip's count equals what its link lists, the opening / tonight / last
//! chance / hands-on rules, zero chips hidden, and the 5-minute cache.
//! Dates are relative to today in London.

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
use musenmingle::normalise::london_to_utc;
use musenmingle::suggestions::Suggestions;
use sqlx::PgPool;
use tower::ServiceExt;

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

fn today() -> NaiveDate {
    Utc::now()
        .with_timezone(&chrono_tz::Europe::London)
        .date_naive()
}

/// London wall-clock time on `today + days`; `None` = midnight (untimed).
fn at(days: i64, hm: Option<(u32, u32)>) -> DateTime<Utc> {
    let d = today() + Duration::days(days);
    let t = hm.map_or(NaiveTime::MIN, |(h, m)| {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    });
    london_to_utc(d.and_time(t))
}

#[derive(Default)]
struct Ev {
    title: &'static str,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    all_day: bool,
    category: &'static str,
    is_free: bool,
    tags: Vec<&'static str>,
    format_tags: Vec<&'static str>,
    is_opening: Option<bool>,
}

async fn insert(pool: &PgPool, e: Ev) {
    sqlx::query(
        "INSERT INTO events.events
            (title, starts_at, ends_at, all_day, category, is_free, tags, format_tags,
             is_opening, dedupe_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $1)",
    )
    .bind(e.title)
    .bind(e.starts_at.unwrap())
    .bind(e.ends_at)
    .bind(e.all_day)
    .bind(if e.category.is_empty() {
        "talk"
    } else {
        e.category
    })
    .bind(e.is_free)
    .bind(&e.tags)
    .bind(&e.format_tags)
    .bind(e.is_opening)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed(pool: &PgPool) {
    let evs = [
        Ev {
            title: "Evening talk",
            starts_at: Some(at(0, Some((19, 0)))),
            is_free: true,
            ..Ev::default()
        },
        Ev {
            title: "Morning talk",
            starts_at: Some(at(0, Some((10, 0)))),
            ..Ev::default()
        },
        Ev {
            title: "Late opening show",
            starts_at: Some(at(-10, None)),
            ends_at: Some(at(30, None)),
            all_day: true,
            category: "exhibition",
            tags: vec!["late opening"],
            ..Ev::default()
        },
        Ev {
            title: "New show",
            starts_at: Some(at(2, None)),
            ends_at: Some(at(60, None)),
            all_day: true,
            category: "exhibition",
            ..Ev::default()
        },
        Ev {
            title: "Private view: Spring",
            starts_at: Some(at(3, Some((18, 0)))),
            category: "community",
            ..Ev::default()
        },
        Ev {
            title: "PVC and plastics talk",
            starts_at: Some(at(3, Some((12, 0)))),
            ..Ev::default()
        },
        Ev {
            title: "Closing show",
            starts_at: Some(at(-40, None)),
            ends_at: Some(at(3, None)),
            all_day: true,
            category: "exhibition",
            is_free: true,
            ..Ev::default()
        },
        Ev {
            title: "Two-day fair",
            starts_at: Some(at(1, None)),
            ends_at: Some(at(2, None)),
            all_day: true,
            category: "expo",
            ..Ev::default()
        },
        Ev {
            title: "Life drawing drop-in",
            starts_at: Some(at(4, Some((14, 0)))),
            category: "community",
            ..Ev::default()
        },
        Ev {
            title: "Pottery session",
            starts_at: Some(at(5, Some((11, 0)))),
            category: "workshop",
            ..Ev::default()
        },
        Ev {
            title: "Print studio evening",
            starts_at: Some(at(6, Some((18, 30)))),
            category: "community",
            format_tags: vec!["hands_on"],
            ..Ev::default()
        },
        Ev {
            title: "Launch party (AI says opening)",
            starts_at: Some(at(2, Some((19, 0)))),
            category: "community",
            is_opening: Some(true),
            ..Ev::default()
        },
        Ev {
            title: "Far show",
            starts_at: Some(at(10, None)),
            ends_at: Some(at(40, None)),
            all_day: true,
            category: "exhibition",
            ..Ev::default()
        },
        Ev {
            title: "Last week's workshop",
            starts_at: Some(at(-5, Some((11, 0)))),
            category: "workshop",
            ..Ev::default()
        },
    ];
    for e in evs {
        insert(pool, e).await;
    }
}

/// (label, href, count, active) of each chip on a page.
fn chips(body: &str) -> Vec<(String, String, i64, bool)> {
    let Some(start) = body.find("<p class=\"chip-row quick-picks\">") else {
        return Vec::new();
    };
    let row = &body[start..start + body[start..].find("</p>").unwrap()];
    row.split("<a class=\"pill\" href=\"")
        .skip(1)
        .map(|a| {
            let href = a[..a.find('"').unwrap()].replace("&amp;", "&");
            let active = a[..a.find('>').unwrap()].contains("aria-current=\"true\"");
            let text = &a[a.find('>').unwrap() + 1..];
            let label = text[..text.find('<').unwrap()].to_string();
            let n = &text[text.find("<span class=\"pill-count\">").unwrap() + 25..];
            let count = n[..n.find('<').unwrap()].parse().unwrap();
            (label, href, count, active)
        })
        .collect()
}

fn card_titles(body: &str) -> Vec<String> {
    body.split("<article class=\"card\">")
        .skip(1)
        .map(|card| {
            let h2 = &card[card.find("<h2><a href=\"").unwrap()..];
            let start = h2.find("\">").unwrap() + 2;
            h2[start..start + h2[start..].find("</a>").unwrap()].to_string()
        })
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn chip_counts_match_the_listings_they_open() {
    let Some(db) = TestDb::create("chip_counts_match_the_listings_they_open").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    seed(&pool).await;
    let app = app(&pool);
    let (status, home) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK, "{home}");
    let chips = chips(&home);
    let labels: Vec<&str> = chips.iter().map(|c| c.0.as_str()).collect();
    // "This weekend" may be empty depending on today's weekday; the rest
    // always have events in the fixtures.
    for l in [
        "Tonight",
        "Free",
        "Openings this week",
        "Last chance",
        "Hands-on",
        "Talks",
    ] {
        assert!(labels.contains(&l), "{l} missing from {labels:?}");
    }
    assert!(chips.iter().all(|c| c.2 > 0 && !c.3), "{chips:?}");
    for (label, href, count, _) in &chips {
        let (status, page) = get(&app, href).await;
        assert_eq!(status, StatusCode::OK, "{href}: {page}");
        let titles = card_titles(&page);
        assert_eq!(titles.len() as i64, *count, "{label} {href}: {titles:?}");
        // The chip is shown as selected there, and links back to `/`.
        let here = self::chips(&page);
        let me = here.iter().find(|c| &c.0 == label).unwrap();
        assert!(me.3, "{label} not active on {href}");
        assert_eq!(me.1, "/");
        assert!(page.contains("(selected; select again to clear)"));
    }

    let titles_of = |href: &'static str| {
        let app = app.clone();
        async move { sorted(card_titles(&get(&app, href).await.1)) }
    };
    assert_eq!(
        titles_of("/?pick=tonight").await,
        ["Evening talk", "Late opening show"]
    );
    assert_eq!(
        titles_of("/?pick=openings").await,
        [
            "Launch party (AI says opening)",
            "New show",
            "Private view: Spring"
        ]
    );
    assert_eq!(
        titles_of("/?pick=last_chance&sort=ending").await,
        ["Closing show", "Two-day fair"]
    );
    assert_eq!(
        titles_of("/?pick=hands_on").await,
        [
            "Life drawing drop-in",
            "Pottery session",
            "Print studio evening"
        ]
    );
    // The JSON API takes the same parameter.
    let (s, json) = get(&app, "/v1/events?pick=openings").await;
    assert_eq!(s, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["events"].as_array().unwrap().len(), 3);
    // Other filters combine with a pick, and the form keeps it.
    let (_, p) = get(&app, "/?pick=openings&category=exhibition").await;
    assert_eq!(card_titles(&p), ["New show"]);
    assert!(p.contains("<input type=\"hidden\" name=\"pick\" value=\"openings\">"));
    let (s, _) = get(&app, "/?pick=everything").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&app, "/v1/events?pick=everything").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    db.drop_db().await;
}

#[tokio::test]
async fn zero_chips_hide_and_counts_are_cached() {
    let Some(db) = TestDb::create("zero_chips_hide_and_counts_are_cached").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app(&pool);
    let (_, home) = get(&app, "/").await;
    assert!(!home.contains("Quick picks:"), "no events, no chips");
    insert(
        &pool,
        Ev {
            title: "Only talk",
            starts_at: Some(at(2, Some((12, 0)))),
            ..Ev::default()
        },
    )
    .await;
    // Still the cached (empty) counts within 5 minutes.
    let (_, home) = get(&app, "/").await;
    assert!(!home.contains("Quick picks:"));
    // A new router (cache) counts again: only "Talks" (and maybe "This
    // weekend") has an event.
    let (_, home) = get(&self::app(&pool), "/").await;
    let labels: Vec<String> = chips(&home).into_iter().map(|c| c.0).collect();
    assert!(labels.contains(&"Talks".to_string()), "{labels:?}");
    assert!(!labels.contains(&"Tonight".to_string()));
    assert!(!labels.contains(&"Free".to_string()));
    assert!(!labels.contains(&"Hands-on".to_string()));
    db.drop_db().await;
}

/// (title, starts, ends, all_day, opening_hours)
type Row = (
    &'static str,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    bool,
    Option<serde_json::Value>,
);

/// `pick=open_now` (#206): started, not ended, and inside today's hours;
/// an all-day or untimed event with unknown hours is left out.
#[tokio::test]
async fn open_now_chip_lists_what_is_open_at_this_minute() {
    let Some(db) = TestDb::create("open_now_chip_lists_what_is_open_at_this_minute").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let now = Utc::now();
    let today = now.with_timezone(&chrono_tz::Europe::London).date_naive();
    let dow = chrono::Datelike::weekday(&today).number_from_monday() as i64;
    let other_days: Vec<i64> = (1..=7).filter(|d| *d != dow).collect();
    let rows: Vec<Row> = vec![
        (
            "Talk on now",
            now - Duration::minutes(30),
            Some(now + Duration::hours(1)),
            false,
            None,
        ),
        (
            "Talk later",
            now + Duration::hours(2),
            Some(now + Duration::hours(3)),
            false,
            None,
        ),
        (
            "Talk over",
            now - Duration::hours(3),
            Some(now - Duration::hours(1)),
            false,
            None,
        ),
        (
            "Show open all day",
            at(-10, None),
            Some(at(30, None)),
            true,
            Some(
                serde_json::json!([{"days": [1, 2, 3, 4, 5, 6, 7], "opens": "00:00", "closes": "23:59"}]),
            ),
        ),
        (
            "Show closed today",
            at(-10, None),
            Some(at(30, None)),
            true,
            Some(serde_json::json!([{"days": other_days, "opens": "00:00", "closes": "23:59"}])),
        ),
        (
            "Show hours unknown",
            at(-10, None),
            Some(at(30, None)),
            true,
            None,
        ),
    ];
    for (title, s, e, all_day, hours) in rows {
        sqlx::query(
            "INSERT INTO events.events
                (title, starts_at, ends_at, all_day, category, opening_hours, dedupe_key)
             VALUES ($1, $2, $3, $4, 'exhibition', $5, $1)",
        )
        .bind(title)
        .bind(s)
        .bind(e)
        .bind(all_day)
        .bind(hours)
        .execute(&pool)
        .await
        .unwrap();
    }
    let app = app(&pool);
    let (_, home) = get(&app, "/").await;
    let open = chips(&home)
        .into_iter()
        .find(|c| c.0 == "Open now")
        .expect("Open now chip");
    assert_eq!(open.1, "/?pick=open_now");
    let (status, page) = get(&app, "/?pick=open_now").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let titles = sorted(card_titles(&page));
    // "Show open all day" is open unless this runs in the day's last minute.
    let late = now
        .with_timezone(&chrono_tz::Europe::London)
        .format("%H:%M")
        .to_string()
        == "23:59";
    let want: Vec<&str> = if late {
        vec!["Talk on now"]
    } else {
        vec!["Show open all day", "Talk on now"]
    };
    assert_eq!(titles, want);
    assert_eq!(open.2, titles.len() as i64);
    let (s, json) = get(&app, "/v1/events?pick=open_now").await;
    assert_eq!(s, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["events"].as_array().unwrap().len(), titles.len());
    db.drop_db().await;
}
