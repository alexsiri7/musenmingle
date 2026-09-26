//! The server-rendered HTML pages (`/`, `/events/{id}`, `/sources`,
//! `POST /suggest`) against a real database (and a wiremock GitHub for the
//! suggestion form). Event dates are relative to now because `/` defaults to
//! upcoming events.

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
use musenmingle::github::{GitHubIssueFiler, IssueFiler};
use musenmingle::suggestions::Suggestions;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO: &str = "alexsiri7/musenmingle";

fn app_with(pool: &PgPool, config: SuggestionConfig, filer: Option<Box<dyn IssueFiler>>) -> Router {
    let config = SuggestionConfig {
        ip_salt: Some("test-salt".into()),
        ..config
    };
    let settings = ApiSettings {
        github_repo: REPO.into(),
        cors_origins: Vec::new(),
    };
    musenmingle::api::router(
        pool.clone(),
        Suggestions::new(config, filer).unwrap(),
        settings,
    )
    .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 1], 4000))))
}

fn app(pool: &PgPool) -> Router {
    app_with(pool, SuggestionConfig::default(), None)
}

struct Page {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn send(app: &Router, req: Request<Body>) -> Page {
    let resp = app.clone().oneshot(req).await.unwrap();
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

async fn get(app: &Router, uri: &str) -> Page {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

async fn post_form(app: &Router, body: &str) -> Page {
    send(
        app,
        Request::post("/suggest")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

fn assert_html(p: &Page) {
    assert_eq!(
        p.headers[header::CONTENT_TYPE],
        "text/html; charset=utf-8",
        "{}",
        p.body
    );
    let csp = p.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    for d in [
        "default-src 'self'",
        "script-src 'self'",
        "img-src 'self';",
        "style-src 'self'",
        "form-action 'self'",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(d), "{csp}");
    }
    assert!(!csp.contains("unsafe-inline"), "{csp}");
    assert!(p.body.starts_with("<!DOCTYPE html>"));
    assert!(
        p.body
            .contains("<a class=\"brand\" href=\"/\">Muse &amp; Mingle</a>")
    );
    // Exactly one script: the first-party, versioned app.js (no inline code).
    assert_eq!(p.body.matches("<script").count(), 1, "{}", p.body);
    assert!(
        p.body.contains("<script src=\"/static/app.js?v="),
        "{}",
        p.body
    );
    assert!(p.body.contains("\" defer></script>"));
    assert!(!p.body.contains(" onclick="));
    // Never link into the private repo (#57).
    assert!(!p.body.contains("github.com/alexsiri7/"), "{}", p.body);
    // Never hotlink: every image is one of our own thumbnails.
    for img in p.body.split("<img ").skip(1) {
        let tag = &img[..img.find('>').unwrap()];
        if let Some(at) = tag.find("src=\"") {
            assert!(tag[at..].starts_with("src=\"/thumbs/"), "{tag}");
        }
    }
    assert!(!p.body.contains("style=\""), "{}", p.body);
}

fn days(n: i64) -> DateTime<Utc> {
    Utc::now() + Duration::days(n)
}

fn date(t: DateTime<Utc>) -> String {
    t.with_timezone(&chrono_tz::Europe::London)
        .format("%Y-%m-%d")
        .to_string()
}

#[derive(Clone)]
struct Ev {
    title: String,
    starts_at: DateTime<Utc>,
    ends_at: Option<DateTime<Utc>>,
    category: &'static str,
    is_free: bool,
    at: Option<(f64, f64)>,
    venue: Option<&'static str>,
    description: Option<&'static str>,
    image_url: Option<&'static str>,
    url: Option<&'static str>,
}

impl Ev {
    fn new(title: &str, starts_at: DateTime<Utc>) -> Self {
        Ev {
            title: title.into(),
            starts_at,
            ends_at: None,
            category: "talk",
            is_free: false,
            at: None,
            venue: None,
            description: None,
            image_url: None,
            url: None,
        }
    }
}

async fn insert(pool: &PgPool, e: Ev) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events
            (title, starts_at, ends_at, category, is_free, lat, lng, dedupe_key,
             venue_name, description, image_url, url, price_min, price_max, currency)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $1, $8, $9, $10, $11,
                 CASE WHEN $5 THEN NULL ELSE 5 END, CASE WHEN $5 THEN NULL ELSE 12.50 END,
                 CASE WHEN $5 THEN NULL ELSE 'GBP' END)
         RETURNING id",
    )
    .bind(&e.title)
    .bind(e.starts_at)
    .bind(e.ends_at)
    .bind(e.category)
    .bind(e.is_free)
    .bind(e.at.map(|a| a.0))
    .bind(e.at.map(|a| a.1))
    .bind(e.venue)
    .bind(e.description)
    .bind(e.image_url)
    .bind(e.url)
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

/// Card titles on a page, in order.
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

#[tokio::test]
async fn home_lists_upcoming_events_escaped_with_safe_links() {
    let Some(db) = TestDb::create("home_lists_upcoming_events_escaped_with_safe_links").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let drawing = insert(
        &pool,
        Ev {
            venue: Some("Barbican"),
            image_url: Some("https://img.example.org/drawing.jpg"),
            ..Ev::new("Life drawing", days(2))
        },
    )
    .await;
    link(
        &pool,
        drawing,
        "barbican",
        "https://www.barbican.org.uk/life-drawing",
    )
    .await;
    let evil = insert(
        &pool,
        Ev {
            venue: Some("<img src=x onerror=alert(2)>"),
            image_url: Some("javascript:alert(3)"),
            ..Ev::new("<script>alert(1)</script>", days(3))
        },
    )
    .await;
    link(&pool, evil, "barbican", "javascript:alert(4)").await;
    insert(
        &pool,
        Ev {
            ends_at: Some(days(30)),
            category: "exhibition",
            is_free: true,
            ..Ev::new("Running show", days(-5))
        },
    )
    .await;
    insert(&pool, Ev::new("Last week's talk", days(-7))).await;
    let app = app(&pool);

    let p = get(&app, "/").await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_html(&p);
    assert!(
        p.body
            .contains("<title>Muse &amp; Mingle — What's on in London for creative people</title>")
    );
    assert!(
        p.body
            .contains("<h1>What's on in London for creative people</h1>")
    );
    assert_eq!(
        card_titles(&p.body),
        [
            "Running show",
            "Life drawing",
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        ]
    );
    assert!(!p.body.contains("<img src=x"), "{}", p.body);
    assert!(p.body.contains("&lt;img src=x onerror=alert(2)&gt;"));
    assert!(!p.body.contains("javascript:"), "{}", p.body);
    // The source's image is never hotlinked (no thumbnail made yet: none).
    assert!(!p.body.contains("img.example.org"), "{}", p.body);
    assert!(!p.body.contains("<img"), "{}", p.body);
    assert!(p.body.contains("Barbican"));
    assert!(p.body.contains("£5–£12.50"), "{}", p.body);
    assert!(p.body.contains(">Free</span>"));
    assert!(p.body.contains(">Exhibition</span>"));
    assert!(p.body.contains("Until <time"));
    assert!(p.body.contains(&format!("href=\"/events/{drawing}\"")));
    assert!(
        p.body
            .contains("<a class=\"button\" href=\"https://www.barbican.org.uk/life-drawing\" rel=\"noopener\">See it on Barbican →")
    );
    // Default "from" is today (London), shown in the form.
    assert!(p.body.contains(&format!(
        "name=\"from\" type=\"date\" value=\"{}\"",
        date(Utc::now())
    )));
    // Save toggles: rendered hidden (shown by app.js), with the snapshot.
    assert!(p.body.contains(&format!(
        "<button type=\"button\" class=\"save\" hidden aria-pressed=\"false\" data-save-id=\"{drawing}\" data-title=\"Life drawing\" data-venue=\"Barbican\""
    )), "{}", p.body);
    assert!(
        p.body
            .contains("data-title=\"&lt;script&gt;alert(1)&lt;/script&gt;\"")
    );
    assert!(p.body.contains(
        "<a href=\"/saved\">Saved <span class=\"count\" data-saved-count hidden>0</span></a>"
    ));
    // Footer suggestion form.
    assert!(
        p.body
            .contains("<form class=\"suggest\" method=\"post\" action=\"/suggest\">")
    );

    let js = get(&app, "/static/app.js?v=whatever").await;
    assert_eq!(js.status, StatusCode::OK);
    assert_eq!(
        js.headers[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        js.headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert!(js.body.contains("musenmingle.saved.v1"));

    let css = get(&app, "/static/style.css").await;
    assert_eq!(css.status, StatusCode::OK);
    assert_eq!(css.headers[header::CONTENT_TYPE], "text/css; charset=utf-8");

    // Site icons, from our own origin and linked from every page.
    assert!(
        p.body
            .contains(r#"<link rel="icon" href="/favicon.svg" type="image/svg+xml">"#)
    );
    assert!(
        p.body
            .contains(r#"<link rel="icon" href="/favicon.ico" sizes="32x32">"#)
    );
    for (path, ct) in [
        ("/favicon.svg", "image/svg+xml"),
        ("/favicon.ico", "image/x-icon"),
        ("/apple-touch-icon.png", "image/png"),
    ] {
        let icon = get(&app, path).await;
        assert_eq!(icon.status, StatusCode::OK, "{path}");
        assert_eq!(icon.headers[header::CONTENT_TYPE], ct, "{path}");
    }
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn home_filters_are_applied_and_reflected_in_the_form() {
    let Some(db) = TestDb::create("home_filters_are_applied_and_reflected_in_the_form").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let kings_cross = Some((51.5345, -0.1250));
    let barbican = Some((51.5202, -0.0938));
    let match_it = Ev {
        category: "workshop",
        is_free: true,
        at: kings_cross,
        ..Ev::new("Free KX workshop", days(3))
    };
    insert(&pool, match_it.clone()).await;
    insert(
        &pool,
        Ev {
            is_free: false,
            ..Ev::new("Paid KX workshop", days(3))
        }
        .with(match_it.clone()),
    )
    .await;
    insert(
        &pool,
        Ev {
            at: barbican,
            ..match_it.clone()
        }
        .titled("Free Barbican workshop"),
    )
    .await;
    insert(
        &pool,
        Ev {
            category: "talk",
            ..match_it.clone()
        }
        .titled("Free KX talk"),
    )
    .await;
    insert(
        &pool,
        Ev {
            starts_at: days(20),
            ..match_it.clone()
        }
        .titled("Free KX workshop later"),
    )
    .await;
    let app = app(&pool);

    let (from, to) = (date(days(1)), date(days(10)));
    let p = get(
        &app,
        &format!("/?from={from}&to={to}&category=workshop&free=true&near=kings-cross"),
    )
    .await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_eq!(card_titles(&p.body), ["Free KX workshop"]);
    assert!(
        p.body
            .contains(&format!("name=\"from\" type=\"date\" value=\"{from}\""))
    );
    assert!(
        p.body
            .contains(&format!("name=\"to\" type=\"date\" value=\"{to}\""))
    );
    assert!(
        p.body.contains("<option value=\"workshop\" selected>"),
        "{}",
        p.body
    );
    assert!(p.body.contains("<option value=\"kings-cross\" selected>"));
    assert!(p.body.contains("type=\"checkbox\" value=\"true\" checked"));
    assert!(p.body.contains(" km</span>"), "distance shown with near");

    // Checkbox "on" and empty fields (a plain browser submit) are accepted.
    let p = get(&app, &format!("/?from={from}&to=&category=&free=on&near=")).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    let mut titles = card_titles(&p.body);
    titles.sort();
    assert_eq!(
        titles,
        [
            "Free Barbican workshop",
            "Free KX talk",
            "Free KX workshop",
            "Free KX workshop later"
        ]
    );

    // Invalid filters: 400 page with the message, form still shown.
    for (q, msg) in [
        (format!("from={to}&to={from}"), "from must not be after to"),
        ("category=concert".into(), "unknown category"),
        ("near=mars".into(), "unknown area"),
        ("from=yesterday".into(), "from must be a date"),
    ] {
        let p = get(&app, &format!("/?{q}")).await;
        assert_eq!(p.status, StatusCode::BAD_REQUEST, "{q}");
        assert_html(&p);
        assert!(p.body.contains(msg), "{q}: {}", p.body);
        assert!(p.body.contains("<form class=\"filters\""));
    }
    pool.close().await;
    db.drop_db().await;
}

impl Ev {
    fn titled(mut self, title: &str) -> Self {
        self.title = title.into();
        self
    }

    /// `self`'s title and price with the rest of `base`.
    fn with(self, base: Ev) -> Self {
        Ev {
            title: self.title,
            is_free: self.is_free,
            ..base
        }
    }
}

#[tokio::test]
async fn home_filters_by_source_with_a_clearable_chip() {
    let Some(db) = TestDb::create("home_filters_by_source_with_a_clearable_chip").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let both = insert(&pool, Ev::new("On both", days(1))).await;
    link(&pool, both, "barbican", "https://www.barbican.org.uk/a").await;
    link(
        &pool,
        both,
        "somerset-house",
        "https://www.somersethouse.org.uk/a",
    )
    .await;
    let one = insert(&pool, Ev::new("Barbican only", days(2))).await;
    link(&pool, one, "barbican", "https://www.barbican.org.uk/b").await;
    let other = insert(&pool, Ev::new("Somerset only", days(3))).await;
    link(
        &pool,
        other,
        "somerset-house",
        "https://www.somersethouse.org.uk/c",
    )
    .await;
    let app = app(&pool);

    let p = get(&app, "/?source=barbican").await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_eq!(card_titles(&p.body), ["On both", "Barbican only"]);
    assert!(
        p.body.contains("From: <strong>Barbican</strong>"),
        "{}",
        p.body
    );
    assert!(
        p.body
            .contains("<input type=\"hidden\" name=\"source\" value=\"barbican\">")
    );
    // The chip clears the source but keeps the other filters.
    let start = p.body.find("<span class=\"chip\">").unwrap();
    let chip = &p.body[start..];
    let href_at = chip.find("href=\"").unwrap() + 6;
    let href = chip[href_at..href_at + chip[href_at..].find('"').unwrap()].replace("&amp;", "&");
    assert!(
        href.starts_with("/?from=") && !href.contains("source="),
        "{href}"
    );
    let p = get(&app, &href).await;
    assert_eq!(
        card_titles(&p.body),
        ["On both", "Barbican only", "Somerset only"]
    );

    let p = get(&app, "/?source=somerset-house").await;
    assert_eq!(card_titles(&p.body), ["On both", "Somerset only"]);
    let p = get(&app, "/?source=Bad%20Key").await;
    assert_eq!(p.status, StatusCode::BAD_REQUEST);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn home_paginates_with_a_more_link_keeping_filters() {
    let Some(db) = TestDb::create("home_paginates_with_a_more_link_keeping_filters").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let n = musenmingle::web::PAGE_SIZE + 3;
    for i in 0..n {
        insert(
            &pool,
            Ev {
                category: "workshop",
                ..Ev::new(&format!("Workshop {i:02}"), days(1) + Duration::hours(i))
            },
        )
        .await;
    }
    insert(&pool, Ev::new("A talk", days(1))).await;
    let app = app(&pool);

    let p = get(&app, "/?category=workshop").await;
    assert_eq!(
        card_titles(&p.body).len() as i64,
        musenmingle::web::PAGE_SIZE
    );
    let start = p.body.find("<a href=\"/?").expect("More link") + "<a href=\"".len();
    let href = p.body[start..start + p.body[start..].find('"').unwrap()].replace("&amp;", "&");
    assert!(
        href.contains("category=workshop") && href.contains("cursor="),
        "{href}"
    );
    let p2 = get(&app, &href).await;
    assert_eq!(p2.status, StatusCode::OK, "{}", p2.body);
    assert_eq!(
        card_titles(&p2.body),
        ["Workshop 24", "Workshop 25", "Workshop 26"]
    );
    assert!(!p2.body.contains("rel=\"next\""));
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn detail_page_shows_all_fields_and_404s_unknown_ids() {
    let Some(db) = TestDb::create("detail_page_shows_all_fields_and_404s_unknown_ids").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let id = insert(
        &pool,
        Ev {
            venue: Some("Barbican"),
            at: Some((51.5202, -0.0938)),
            description: Some("First <b>para</b>\nsecond line\n\nSecond para & more"),
            url: Some("https://www.barbican.org.uk/life-drawing"),
            image_url: Some("https://img.example.org/drawing.jpg"),
            ..Ev::new("<script>alert(1)</script> drawing", days(2))
        },
    )
    .await;
    link(
        &pool,
        id,
        "barbican",
        "https://www.barbican.org.uk/life-drawing",
    )
    .await;
    link(
        &pool,
        id,
        "ticketmaster",
        "https://www.ticketmaster.co.uk/x",
    )
    .await;
    let app = app(&pool);

    let p = get(&app, &format!("/events/{id}")).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_html(&p);
    assert!(
        p.body
            .contains("<h1>&lt;script&gt;alert(1)&lt;/script&gt; drawing</h1>")
    );
    assert!(p.body.contains(
        "<p>First &lt;b&gt;para&lt;/b&gt;<br>second line</p><p>Second para &amp; more</p>"
    ));
    assert!(p.body.contains(
        "https://www.openstreetmap.org/?mlat=51.5202&amp;mlon=-0.0938#map=17/51.5202/-0.0938"
    ));
    assert!(p.body.contains("rel=\"noopener\">Barbican</a>"));
    assert!(p.body.contains("See it on Barbican →"), "{}", p.body);
    assert!(!p.body.contains("img.example.org"), "{}", p.body);
    assert!(p.body.contains("href=\"https://www.ticketmaster.co.uk/x\""));
    assert!(p.body.contains("£5–£12.50"), "{}", p.body);
    assert!(p.body.contains("Talk"));
    assert!(p.body.contains(&format!(
        "class=\"save\" hidden aria-pressed=\"false\" data-save-id=\"{id}\""
    )));

    for uri in [
        format!("/events/{}", Uuid::new_v4()),
        "/events/not-a-uuid".to_string(),
    ] {
        let p = get(&app, &uri).await;
        assert_eq!(p.status, StatusCode::NOT_FOUND, "{uri}");
        assert_html(&p);
        assert!(p.body.contains("Event not found"));
    }
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn sources_page_renders_the_sources_api_data() {
    let Some(db) = TestDb::create("sources_page_renders_the_sources_api_data").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let source = musenmingle::repo::source_by_key(&pool, "barbican")
        .await
        .unwrap()
        .unwrap();
    let started_at: DateTime<Utc> = "2026-09-26T05:00:00Z".parse().unwrap();
    musenmingle::repo::record_run(
        &pool,
        &musenmingle::repo::NewRun {
            source_id: source.id,
            started_at,
            finished_at: started_at + Duration::seconds(2),
            events_found: 41,
            errors: 3,
            error_summary: None,
            ok: true,
        },
    )
    .await
    .unwrap();
    let app = app(&pool);

    let p = get(&app, "/sources").await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_html(&p);
    assert!(
        p.body
            .contains("<title>Sources · Muse &amp; Mingle</title>")
    );
    let row = &p.body[p
        .body
        .find("<a href=\"/?source=barbican\"")
        .expect("barbican row")..];
    let row = &row[..row.find("</tr>").unwrap()];
    assert!(row.contains("Sat 26 Sep 2026, 06:00"), "{row}");
    assert!(row.contains("<td>41</td><td>3</td>"), "{row}");
    assert!(row.contains("status-degraded"), "{row}");
    assert!(
        p.body.contains("<a href=\"/?source=ticketmaster\""),
        "{}",
        p.body
    );
    assert!(
        p.body
            .contains("<h2 id=\"refused\">Sites we couldn&#39;t use</h2>")
            || p.body
                .contains("<h2 id=\"refused\">Sites we couldn't use</h2>")
    );
    assert!(p.body.contains(">Southbank Centre</a>"));
    assert!(
        p.body
            .contains("it returns 403 to our crawler's User-Agent; we don't evade blocks")
    );
    assert!(
        p.body
            .contains("<time datetime=\"2026-09-25\">25 Sep 2026</time>")
    );
    // Never link into the private repo (#57).
    assert!(!p.body.contains("github.com/alexsiri7/"), "{}", p.body);
    assert!(p.body.contains("Never"));
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn suggestion_form_files_an_issue_and_reports_outcomes() {
    let Some(db) = TestDb::create("suggestion_form_files_an_issue_and_reports_outcomes").await
    else {
        return;
    };
    let pool = db.migrated_pool().await;
    let github = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues")))
        .and(body_partial_json(json!({
            "title": "New scraper: example-gallery.org.uk",
            "labels": ["new-scraper"],
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "number": 42, "title": "New scraper: example-gallery.org.uk"
        })))
        .expect(1)
        .mount(&github)
        .await;
    let filer: Box<dyn IssueFiler> =
        Box::new(GitHubIssueFiler::new(&github.uri(), REPO, "test-token").unwrap());
    let config = SuggestionConfig {
        per_hour: 4,
        per_day: 20,
        ..Default::default()
    };
    let app = app_with(&pool, config, Some(filer));

    let p = post_form(
        &app,
        "url=https%3A%2F%2Fwww.example-gallery.org.uk%2Fwhats-on&note=tiny+%3Cb%3Egallery",
    )
    .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.body);
    assert_html(&p);
    assert!(p.body.contains("<strong>example-gallery.org.uk</strong>"));
    // Never link into the private repo (#57).
    assert!(!p.body.contains("github.com/alexsiri7/"), "{}", p.body);
    assert!(
        p.body.contains("Thanks — we&#39;ll take a look at")
            || p.body.contains("Thanks — we'll take a look at")
    );
    let note: Option<String> =
        sqlx::query_scalar("SELECT note FROM events.site_suggestions ORDER BY id LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(note.as_deref(), Some("tiny <b>gallery"));

    // Same domain again (empty note): already suggested, no second issue.
    let p = post_form(&app, "url=http%3A%2F%2Fexample-gallery.org.uk%2F&note=").await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert!(p.body.contains("already been suggested"));

    // Invalid URL: validation message, escaped.
    let p = post_form(&app, "url=javascript%3Aalert(1)").await;
    assert_eq!(p.status, StatusCode::BAD_REQUEST, "{}", p.body);
    assert!(
        p.body.contains("only http and https URLs are accepted"),
        "{}",
        p.body
    );

    // A refused site: the reason, not filed.
    let p = post_form(
        &app,
        "url=https%3A%2F%2Fwww.southbankcentre.co.uk%2Fwhats-on",
    )
    .await;
    assert_eq!(p.status, StatusCode::CONFLICT, "{}", p.body);
    assert!(
        p.body.contains(
            "We looked at Southbank Centre on 25 September 2026 and couldn't include it: \
         it returns 403 to our crawler's User-Agent; we don't evade blocks."
        ),
        "{}",
        p.body
    );
    // Never link into the private repo (#57).
    assert!(!p.body.contains("github.com/alexsiri7/"), "{}", p.body);

    // Missing field: a friendly 400 page, not a plain-text rejection.
    let p = post_form(&app, "note=hi").await;
    assert_eq!(p.status, StatusCode::BAD_REQUEST);
    assert_html(&p);

    // Rate limit (4/hour; refused ones count, invalid ones do not): 429 + Retry-After.
    // (GitHub rejects this one: accepted but left pending for the ingest run.)
    let p = post_form(&app, "url=https%3A%2F%2Fanother-venue.org.uk").await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.body);
    assert!(p.body.contains("take a look at"), "{}", p.body);
    let p = post_form(&app, "url=https%3A%2F%2Fthird-venue.org.uk").await;
    assert_eq!(p.status, StatusCode::TOO_MANY_REQUESTS, "{}", p.body);
    assert!(p.headers.contains_key(header::RETRY_AFTER));
    assert!(p.body.contains("Too many suggestions"));
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn saved_page_is_a_script_enhanced_shell() {
    // No database needed: the page is static (a lazy pool never connects).
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap();
    let app = app(&pool);
    let p = get(&app, "/saved").await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_html(&p);
    assert!(
        p.body
            .contains("<title>Saved events · Muse &amp; Mingle</title>")
    );
    assert!(p.body.contains("only in this browser on this device"));
    assert!(p.body.contains("<noscript>"));
    assert!(
        p.body
            .contains("<p id=\"saved-empty\" class=\"empty\" hidden>")
    );
    assert!(p.body.contains(
        "<section id=\"saved-list\" class=\"cards\" aria-label=\"Saved events\"></section>"
    ));
    assert!(p.body.contains(
        "<button id=\"export-ics\" type=\"button\" hidden>Export saved as .ics</button>"
    ));
    // The card markup lives in one <template>, with the slots app.js fills.
    let tpl = &p.body[p
        .body
        .find("<template id=\"card-template\">")
        .expect("template")..];
    let tpl = &tpl[..tpl.find("</template>").unwrap()];
    for slot in [
        "image",
        "title",
        "when",
        "venue",
        "category",
        "price",
        "gone",
        "details",
        "details-title",
        "sources",
        "save-title",
    ] {
        assert!(
            tpl.contains(&format!("data-slot=\"{slot}\"")),
            "slot {slot}"
        );
    }
    assert!(tpl.contains("<article class=\"card\">") && tpl.contains("class=\"save\""));
}

#[tokio::test]
async fn about_page_states_our_approach_with_all_anchors() {
    let Some(db) = TestDb::create("about_page_states_our_approach").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let app = app(&pool);
    let p = get(&app, "/about").await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_html(&p);
    assert!(
        p.body
            .contains("<title>About &amp; our approach · Muse &amp; Mingle</title>")
    );
    for anchor in [
        "objective",
        "for-venues",
        "how-we-collect",
        "your-data",
        "contact",
    ] {
        assert!(
            p.body.contains(&format!("<section id=\"{anchor}\"")),
            "#{anchor}"
        );
    }
    for text in [
        "free, non-commercial",
        "No ads, no ticket sales, no affiliate links.",
        "robots.txt",
        "<strong>MuseNMingleBot</strong>",
        "one request every 2 seconds per website",
        "a short excerpt of the description",
        "Image: &lt;your venue&gt;",
        "See it on &lt;your venue&gt;",
        "within 7 days",
        "no tracking or analytics cookies",
        "salted hash of your IP address",
        "href=\"/sources#refused\"",
        "<a href=\"/contact\">use our contact form</a>",
    ] {
        assert!(p.body.contains(text), "missing {text:?}");
    }
    // No cookies are set, here or on the home page.
    assert!(p.headers.get(header::SET_COOKIE).is_none());
    // Header and footer link to it on every page.
    let home = get(&app, "/").await;
    assert_eq!(
        home.body
            .matches("<a href=\"/about\">About &amp; our approach</a>")
            .count(),
        2,
        "{}",
        home.body
    );
    assert!(home.headers.get(header::SET_COOKIE).is_none());
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn owner_request_is_a_refused_reason() {
    let Some(db) = TestDb::create("owner_request_is_a_refused_reason").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    sqlx::query(
        "INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on)
         VALUES ('owner.example', 'Owner Gallery', 'https://owner.example/', 'owner_request',
                 'the venue asked us not to list their events, so we don''t', '2026-10-01')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let p = get(&app(&pool), "/sources").await;
    assert!(p.body.contains("Owner Gallery"), "{}", p.body);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn retired_artrabbit_is_listed_only_as_refused() {
    let Some(db) = TestDb::create("retired_artrabbit_is_listed_only_as_refused").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let p = get(&app(&pool), "/sources").await;
    let (active, refused) = p.body.split_once("id=\"refused\"").unwrap();
    assert!(!active.contains("ArtRabbit"), "{active}");
    assert!(refused.contains("ArtRabbit"), "{refused}");
    assert!(refused.contains("even basic event facts"), "{refused}");
    pool.close().await;
    db.drop_db().await;
}

#[test]
fn venue_request_issue_template_exists() {
    let t = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/ISSUE_TEMPLATE/venue-request.md"
    ))
    .unwrap();
    assert!(t.starts_with("---\nname: Venue request\n"), "{t}");
    assert!(t.contains("\nlabels: venue-request\n"));
    assert!(t.contains("within 7 days"));
}
