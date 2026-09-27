//! Wellcome Collection (Content API): snapshot test over a saved API page
//! and an end-to-end fetch against wiremock.

mod common;

use chrono::{DateTime, Utc};
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::wellcome_collection::{WellcomeCollection, raw_events};
use serde_json::Value;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/wellcome-collection";
const API: &str = "https://api.wellcomecollection.org";

fn page() -> Value {
    serde_json::from_str(&fixture(&format!("{DIR}/events-page-1.json"))).unwrap()
}

/// When the fixture was saved.
fn saved_at() -> DateTime<Utc> {
    "2026-09-27T12:00:00Z".parse().unwrap()
}

#[test]
fn normalised_output_snapshot() {
    let s = WellcomeCollection::new(API.parse().unwrap());
    let (raws, errors) = raw_events(&page(), saved_at());
    assert!(errors.is_empty(), "{errors:?}");
    let out: Vec<_> = raws
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "source_url": raw.source_url,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("wellcome_collection_normalised", out);
}

#[test]
fn past_sessions_are_left_out() {
    let (all, _) = raw_events(&page(), "2026-01-01T00:00:00Z".parse().unwrap());
    let (now, _) = raw_events(&page(), saved_at());
    assert!(now.len() < all.len());
    let (none, _) = raw_events(&page(), "2091-01-01T00:00:00Z".parse().unwrap());
    assert!(none.is_empty());
}

async fn serve(server: &MockServer, robots: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(robots)
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_the_api_via_fetch_context() {
    let server = MockServer::start().await;
    // The real host has no robots.txt (404 = allow all).
    serve(&server, ResponseTemplate::new(404)).await;
    Mock::given(method("GET"))
        .and(path("/content/v0/events"))
        .and(query_param("timespan", "future"))
        .and(query_param("pageSize", "100"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixture(&format!("{DIR}/events-page-1.json"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WellcomeCollection::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let (expected, _) = raw_events(&page(), Utc::now());
    assert_eq!(raws.len(), expected.len());
    for raw in &raws {
        s.normalise(raw).expect("normalise");
    }
}

#[tokio::test]
async fn robots_disallow_blocks_the_source() {
    let server = MockServer::start().await;
    serve(
        &server,
        ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /content/\n"),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/content/v0/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = WellcomeCollection::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
