//! Acceptance tests for spec change #313 (memberships: find events where a
//! membership like the National Art Pass gets you in free or cheaper), one
//! per scenario of `openspec/specs/memberships/spec.md`, through the public
//! interface only: an ingest run with a fake source, then the read API and
//! the HTML pages. Each is ignored until its implementing issue (#315 model
//! and matching, #316 filter, web and API) makes it pass; run them with
//! `cargo test --test memberships -- --ignored`.

mod common;

use std::net::SocketAddr;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode, header};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::{RateLimitConfig, SuggestionConfig};
use musenmingle::fetch::FetchContext;
use musenmingle::health::{HealthChecker, HealthConfig};
use musenmingle::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use musenmingle::normalise::{dedupe_key, london_to_utc};
use musenmingle::repo;
use musenmingle::runner::Runner;
use musenmingle::sources::{SkipReason, Source, SourceError};
use musenmingle::suggestions::Suggestions;
use serde_json::Value;
use sqlx::{AssertSqlSafe, PgPool};
use tower::ServiceExt;

const PROVIDER: &str = "https://www.artfund.org/national-art-pass";
const TATE_OFFER: &str = "50% off exhibitions";
/// Where the Tate Modern offer came from, as a seed migration records it.
const TATE_SOURCE: &str = "https://www.tate.org.uk/visit/tate-modern 2026-10-10";

/// A source that lists the given events.
struct FakeSource {
    events: Vec<NewEvent>,
}

#[async_trait]
impl Source for FakeSource {
    fn key(&self) -> &str {
        "fake"
    }

    async fn fetch(&self, _ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        Ok(self
            .events
            .iter()
            .map(|e| RawEvent {
                source_event_id: e.title.clone(),
                source_url: e.url.clone(),
                payload: serde_json::json!({}),
            })
            .collect())
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        Ok(self
            .events
            .iter()
            .find(|e| e.title == raw.source_event_id)
            .cloned())
    }
}

/// A fresh database where `fake` is the only enabled source.
async fn setup(name: &str) -> Option<(TestDb, PgPool)> {
    let db = TestDb::create(name).await?;
    let pool = db.migrated_pool().await;
    sqlx::query("UPDATE events.sources SET enabled = false")
        .execute(&pool)
        .await
        .unwrap();
    repo::upsert_source(
        &pool,
        "fake",
        SourceKind::Scraper,
        "https://fake.test",
        60,
        true,
    )
    .await
    .unwrap();
    Some((db, pool))
}

/// An ingest run (`run`th of the test, two hours apart so the source is due
/// again) that lists `events`, followed by the post-run syncs.
async fn ingest_run(pool: &PgPool, events: &[NewEvent], run: i64) {
    let events = events.to_vec();
    let runner = Runner {
        pool: pool.clone(),
        ctx: FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap(),
        factory: Box::new(move |row| {
            if row.key == "fake" {
                Ok(Box::new(FakeSource {
                    events: events.clone(),
                }))
            } else {
                Err(SkipReason::UnknownKey)
            }
        }),
        health: HealthChecker::new(HealthConfig::default(), None),
        source_timeout: std::time::Duration::from_secs(30),
        enrich: None,
        qa: None,
        venues: None,
        form_issues: Default::default(),
    };
    runner
        .run_once(Utc::now() + Duration::hours(2 * run))
        .await
        .unwrap();
}

/// Record a venue's offer the way the seed migration (#317) does.
async fn record_offer(pool: &PgPool, venue: &str, offer: &str, source_note: &str) {
    let n = sqlx::query(
        "INSERT INTO events.venue_memberships (venue_id, membership, offer, source_note)
         SELECT id, 'art_pass', $2, $3 FROM events.venues WHERE name = $1",
    )
    .bind(venue)
    .bind(offer)
    .bind(source_note)
    .execute(pool)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(n, 1, "venue {venue} not found");
}

fn today() -> NaiveDate {
    Utc::now()
        .with_timezone(&chrono_tz::Europe::London)
        .date_naive()
}

/// London midnight on `today + days`.
fn day(days: i64) -> DateTime<Utc> {
    london_to_utc((today() + Duration::days(days)).and_time(NaiveTime::MIN))
}

/// A current exhibition at `venue` (an all-day run from ten days ago).
fn exhibition(title: &str, venue: &str, at: (f64, f64)) -> NewEvent {
    let starts_at = day(-10);
    NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(title, starts_at, Some(venue)),
        title: title.into(),
        description: None,
        venue_name: Some(venue.into()),
        address: None,
        lat: Some(at.0),
        lng: Some(at.1),
        starts_at,
        ends_at: Some(day(30)),
        all_day: true,
        price: Price::default(),
        url: Some(format!(
            "https://fake.test/{}",
            title.to_lowercase().replace(' ', "-")
        )),
        image_url: None,
        category: Category::Exhibition,
        tags: Vec::new(),
    }
}

const TATE_MODERN: (f64, f64) = (51.5076, -0.0994);
/// A venue on no membership's list.
const ELSEWHERE: &str = "Hoxton Community Hall";
const ELSEWHERE_AT: (f64, f64) = (51.5310, -0.0800);

/// An event at a venue on no list whose stored text names the National Art
/// Pass. The stored text has no price-notes field (`model::Price` holds only
/// amounts), so the spec's "price notes" line is the stored excerpt.
fn text_match(title: &str) -> NewEvent {
    NewEvent {
        description: Some("Free for National Art Pass holders.".into()),
        ..exhibition(title, ELSEWHERE, ELSEWHERE_AT)
    }
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

async fn get(app: &Router, uri: &str) -> (StatusCode, HeaderMap, String) {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The API's events for `query`, by title.
async fn api_events(app: &Router, query: &str) -> Vec<Value> {
    let (status, _, body) = get(app, &format!("/v1/events?{query}")).await;
    assert_eq!(status, StatusCode::OK, "{query}: {body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let mut events = v["events"].as_array().unwrap().clone();
    events.sort_by_key(|e| e["title"].as_str().unwrap().to_string());
    events
}

/// An event's `art_pass` entry in its `memberships`, if it matches.
fn art_pass(event: &Value) -> Option<&Value> {
    event["memberships"]
        .as_array()
        .unwrap_or_else(|| panic!("no memberships in {event}"))
        .iter()
        .find(|m| m["key"] == "art_pass")
}

fn card_titles(body: &str) -> Vec<String> {
    let mut titles: Vec<String> = body
        .split("<article class=\"card\">")
        .skip(1)
        .map(|card| {
            let h2 = &card[card.find("<h2><a href=\"").unwrap()..];
            let start = h2.find("\">").unwrap() + 2;
            h2[start..start + h2[start..].find("</a>").unwrap()].to_string()
        })
        .collect();
    titles.sort();
    titles
}

fn sort_form(body: &str) -> &str {
    let at = body
        .find("<form class=\"sort-form\"")
        .unwrap_or_else(|| panic!("sort form in {body}"));
    &body[at..at + body[at..].find("</form>").unwrap()]
}

#[tokio::test]
#[ignore = "acceptance test for #313; passes once #315 and #316 land"]
async fn the_national_art_pass_is_known() {
    let Some((db, pool)) = setup("the_national_art_pass_is_known").await else {
        return;
    };
    ingest_run(&pool, &[text_match("Prints and drawings")], 0).await;
    let app = app(&pool);

    // WHEN the memberships are listed (the website's Membership filter)
    let (status, _, home) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK, "{home}");
    let select = &home[home
        .find("<select id=\"membership\" name=\"membership\"")
        .unwrap_or_else(|| panic!("membership select in {home}"))..];
    let select = &select[..select.find("</select>").unwrap()];
    // THEN the National Art Pass is among them with the key art_pass, its
    // display name and its provider link
    assert!(
        select.contains("<option value=\"art_pass\">National Art Pass"),
        "{select}"
    );
    let events = api_events(&app, "").await;
    let m = art_pass(&events[0]).unwrap_or_else(|| panic!("{}", events[0]));
    assert_eq!(m["name"], "National Art Pass");
    assert_eq!(m["url"], PROVIDER);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
#[ignore = "acceptance test for #313; passes once #315 and #316 land"]
async fn a_participating_venue() {
    let Some((db, pool)) = setup("a_participating_venue").await else {
        return;
    };
    // GIVEN Tate Modern is recorded for the National Art Pass with the offer
    // "50% off exhibitions" and its source
    ingest_run(
        &pool,
        &[exhibition("Electric Dreams", "Tate Modern", TATE_MODERN)],
        0,
    )
    .await;
    record_offer(&pool, "Tate Modern", TATE_OFFER, TATE_SOURCE).await;
    let app = app(&pool);
    let events = api_events(&app, "").await;
    let slug = events[0]["venue_slug"].as_str().unwrap().to_string();

    // WHEN the venue's memberships are read (its page)
    let (status, _, page) = get(&app, &format!("/venues/{slug}")).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    // THEN the National Art Pass is returned with that offer and source
    assert!(
        page.contains(&format!("National Art Pass: {TATE_OFFER}")),
        "{page}"
    );
    assert!(
        page.contains("https://www.tate.org.uk/visit/tate-modern"),
        "{page}"
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
#[ignore = "acceptance test for #313; passes once #315 lands"]
async fn matched_by_venue() {
    let Some((db, pool)) = setup("matched_by_venue").await else {
        return;
    };
    // GIVEN an exhibition at Tate Modern
    let events = [exhibition("Electric Dreams", "Tate Modern", TATE_MODERN)];
    ingest_run(&pool, &events, 0).await;
    // AND Tate Modern participates in the National Art Pass with "50% off
    // exhibitions"
    record_offer(&pool, "Tate Modern", TATE_OFFER, TATE_SOURCE).await;
    // WHEN memberships are synced after an ingest run
    ingest_run(&pool, &events, 1).await;
    // THEN the exhibition matches art_pass with the offer "50% off exhibitions"
    let listed = api_events(&app(&pool), "").await;
    let m = art_pass(&listed[0]).unwrap_or_else(|| panic!("{}", listed[0]));
    assert_eq!(m["offer"], TATE_OFFER);
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
#[ignore = "acceptance test for #313; passes once #315 lands"]
async fn matched_by_the_events_own_text() {
    let Some((db, pool)) = setup("matched_by_the_events_own_text").await else {
        return;
    };
    // GIVEN an event at a venue not on any list
    // AND its price notes say "Free for National Art Pass holders"
    // WHEN memberships are synced
    ingest_run(&pool, &[text_match("Prints and drawings")], 0).await;
    // THEN the event matches art_pass
    // AND, as that text states no offer, carries none
    let listed = api_events(&app(&pool), "").await;
    let m = art_pass(&listed[0]).unwrap_or_else(|| panic!("{}", listed[0]));
    assert_eq!(m["offer"], Value::Null, "{m}");
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
#[ignore = "acceptance test for #313; passes once #315 lands"]
async fn no_match() {
    let Some((db, pool)) = setup("no_match").await else {
        return;
    };
    // GIVEN an event at a venue not on any list whose text never names a
    // membership
    let mut e = exhibition("Local makers", ELSEWHERE, ELSEWHERE_AT);
    e.description = Some("Ceramics and prints by local makers.".into());
    e.tags = vec!["craft".into()];
    // WHEN memberships are synced
    ingest_run(&pool, &[e], 0).await;
    // THEN it matches no membership
    let listed = api_events(&app(&pool), "").await;
    assert_eq!(listed[0]["memberships"], serde_json::json!([]));
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
#[ignore = "acceptance test for #313; passes once #315 and #316 land"]
async fn filtering_for_the_art_pass() {
    let Some((db, pool)) = setup("filtering_for_the_art_pass").await else {
        return;
    };
    // GIVEN two upcoming events matching art_pass and one that does not
    let events = [
        exhibition("Electric Dreams", "Tate Modern", TATE_MODERN),
        exhibition("Expressionists", "Tate Modern", TATE_MODERN),
        exhibition("Local makers", ELSEWHERE, ELSEWHERE_AT),
    ];
    ingest_run(&pool, &events, 0).await;
    record_offer(&pool, "Tate Modern", TATE_OFFER, TATE_SOURCE).await;
    ingest_run(&pool, &events, 1).await;
    let app = app(&pool);

    // The filter shows how many current events match.
    let (status, _, home) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK, "{home}");
    assert!(
        home.contains("<option value=\"art_pass\">National Art Pass (2)</option>"),
        "{home}"
    );

    // WHEN the events are listed with membership=art_pass
    // THEN only the two matching events are returned, each showing the
    // National Art Pass and its offer
    let listed = api_events(&app, "membership=art_pass").await;
    let titles: Vec<&str> = listed
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Electric Dreams", "Expressionists"]);
    for e in &listed {
        let m = art_pass(e).unwrap_or_else(|| panic!("{e}"));
        assert_eq!(m["name"], "National Art Pass");
        assert_eq!(m["offer"], TATE_OFFER);
    }
    let (status, _, page) = get(&app, "/?membership=art_pass").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(card_titles(&page), ["Electric Dreams", "Expressionists"]);
    assert_eq!(
        page.matches(&format!("National Art Pass: {TATE_OFFER}"))
            .count(),
        2,
        "{page}"
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
#[ignore = "acceptance test for #313; passes once #315 and #316 land"]
async fn combined_with_other_filters() {
    let Some((db, pool)) = setup("combined_with_other_filters").await else {
        return;
    };
    let now = Utc::now();
    let talk = |title: &str, starts_in: Duration| {
        let starts_at = now + starts_in;
        NewEvent {
            dedupe_key: dedupe_key(title, starts_at, Some(ELSEWHERE)),
            starts_at,
            ends_at: Some(starts_at + Duration::minutes(90)),
            all_day: false,
            category: Category::Talk,
            ..text_match(title)
        }
    };
    let mut on_now_unmatched = talk("Talk on now, no membership", Duration::minutes(-30));
    on_now_unmatched.description = None;
    ingest_run(
        &pool,
        &[
            talk("Talk on now", Duration::minutes(-30)),
            talk("Talk later", Duration::hours(3)),
            on_now_unmatched,
        ],
        0,
    )
    .await;
    let app = app(&pool);

    // WHEN the home page is opened with membership=art_pass and pick=open_now
    let (status, _, page) = get(&app, "/?membership=art_pass&pick=open_now").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    // THEN only events matching both are shown and both stay in the page's
    // links
    assert_eq!(card_titles(&page), ["Talk on now"]);
    let form = sort_form(&page);
    assert!(
        form.contains("<input type=\"hidden\" name=\"membership\" value=\"art_pass\">"),
        "{form}"
    );
    assert!(
        form.contains("<input type=\"hidden\" name=\"pick\" value=\"open_now\">"),
        "{form}"
    );
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
#[ignore = "acceptance test for #313; passes once #316 lands"]
async fn unknown_membership_in_the_api() {
    let Some((db, pool)) = setup("unknown_membership_in_the_api").await else {
        return;
    };
    ingest_run(&pool, &[text_match("Prints and drawings")], 0).await;
    let app = app(&pool);
    // A known membership is a valid parameter...
    let (status, _, body) = get(&app, "/v1/events?membership=art_pass").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // WHEN the API is asked for membership=unknown
    let (status, _, body) = get(&app, "/v1/events?membership=unknown").await;
    // THEN it answers with a client error naming the parameter
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let error = v["error"].as_str().unwrap();
    assert!(error.contains("membership"), "{error}");
    // ...and the website ignores it rather than filtering everything out.
    let (status, _, page) = get(&app, "/?membership=unknown").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(card_titles(&page), ["Prints and drawings"]);
    pool.close().await;
    db.drop_db().await;
}

/// Row counts of every table in schema `events`.
async fn row_counts(pool: &PgPool) -> Vec<(String, i64)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables
         WHERE table_schema = 'events' AND table_type = 'BASE TABLE'
         ORDER BY table_name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut counts = Vec::new();
    for t in tables {
        let n: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT count(*) FROM events.\"{t}\""
        )))
        .fetch_one(pool)
        .await
        .unwrap();
        counts.push((t, n));
    }
    counts
}

#[tokio::test]
#[ignore = "acceptance test for #313; passes once #315 and #316 land"]
async fn choosing_a_membership() {
    let Some((db, pool)) = setup("choosing_a_membership").await else {
        return;
    };
    ingest_run(&pool, &[text_match("Prints and drawings")], 0).await;
    let app = app(&pool);
    let before = row_counts(&pool).await;

    // WHEN a visitor filters by membership=art_pass
    for uri in ["/?membership=art_pass", "/v1/events?membership=art_pass"] {
        let (status, headers, body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert!(!headers.contains_key(header::SET_COOKIE), "{uri}");
    }
    // THEN nothing about that choice is stored beyond the request URL in
    // ordinary access logs
    assert_eq!(row_counts(&pool).await, before);
    pool.close().await;
    db.drop_db().await;
}
