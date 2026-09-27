//! Luma calendars: snapshot tests over each seeded calendar's saved iCal
//! feed (normalised with the calendar's seed `config`), a synthetic feed of
//! iCalendar edge cases (recurrence, TZID, all-day, cancelled, hidden and
//! online locations), and an end-to-end fetch against wiremock.

mod common;

use chrono::{DateTime, Utc};
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::luma::{Luma, parse_feed};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SEED: &str = "migrations/20260927951201_seed_luma_calendars.sql";
const DIR: &str = "scrapers/luma";

/// When the fixtures were saved.
fn now() -> DateTime<Utc> {
    "2026-09-27T08:00:00Z".parse().unwrap()
}

/// Every `(key, config)` Luma row in the seed migration.
fn seed_rows() -> Vec<(String, Value)> {
    let sql = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SEED))
        .unwrap();
    sql.match_indices("('luma-")
        .map(|(i, _)| {
            let row = &sql[i + 2..];
            let key = &row[..row.find('\'').unwrap()];
            let config = &row[row.find("'{").unwrap() + 1..row.find("}'::jsonb").unwrap() + 1];
            let config = serde_json::from_str(&config.replace("''", "'"))
                .unwrap_or_else(|e| panic!("{key}: config is not JSON: {e}"));
            (key.to_string(), config)
        })
        .collect()
}

fn source(key: &str, base: &str) -> Luma {
    let (_, config) = seed_rows()
        .into_iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("no seed row {key}"));
    Luma::from_row(key, base.parse().unwrap(), Some(&config))
        .unwrap()
        .with_now(now())
}

fn feed(name: &str) -> Vec<RawEvent> {
    parse_feed(fixture(&format!("{DIR}/{name}.ics")).as_bytes(), now()).unwrap()
}

fn normalised(s: &Luma, raws: &[RawEvent]) -> Vec<Value> {
    raws.iter()
        .map(|raw| {
            json!({
                "id": raw.source_event_id,
                "start": raw.payload["start"],
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect()
}

#[test]
fn seed_rows_are_the_saved_calendars() {
    let keys: Vec<String> = seed_rows().into_iter().map(|(k, _)| k).collect();
    assert_eq!(
        keys,
        [
            "luma-new-media-london",
            "luma-creative-ai-meetup",
            "luma-mason-and-fifth",
            "luma-for-writers"
        ]
    );
}

#[test]
fn calendar_snapshots() {
    for (key, fixture_name) in [
        ("luma-new-media-london", "new-media-london"),
        ("luma-creative-ai-meetup", "creative-ai-meetup"),
        ("luma-mason-and-fifth", "mason-and-fifth"),
        ("luma-for-writers", "for-writers"),
    ] {
        let s = source(key, "https://api2.luma.com");
        insta::assert_json_snapshot!(key, normalised(&s, &feed(fixture_name)));
    }
}

#[test]
fn edge_cases_snapshot() {
    let s = source("luma-for-writers", "https://api2.luma.com");
    insta::assert_json_snapshot!("luma_edge_cases", normalised(&s, &feed("edge-cases")));
}

#[test]
fn the_feed_url_is_the_official_ical_subscription() {
    let s = source("luma-new-media-london", "https://api2.luma.com");
    assert_eq!(
        s.feed_url().unwrap().as_str(),
        "https://api2.luma.com/ics/get?entity=calendar&id=cal-fMoq9nuFiKXYCzi"
    );
}

#[tokio::test]
async fn fetches_the_feed_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ics/get"))
        .and(query_param("entity", "calendar"))
        .and(query_param("id", "cal-mJXPpBb7tgosK3a"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/calendar; charset=utf-8")
                .set_body_string(fixture(&format!("{DIR}/mason-and-fifth.ics"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = source("luma-mason-and-fifth", &server.uri())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws, feed("mason-and-fifth"));
    assert!(
        raws.iter()
            .any(|r| r.source_event_id == "evt-heINBWFqBgkAJUI")
    );
}

#[tokio::test]
async fn robots_disallow_blocks_the_feed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /ics/\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ics/get"))
        .respond_with(ResponseTemplate::new(200).set_body_string("never fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = source("luma-for-writers", &server.uri())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn a_page_instead_of_a_feed_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ics/get"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>sign in</html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = source("luma-for-writers", &server.uri())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not an iCalendar feed"), "{err}");
}
