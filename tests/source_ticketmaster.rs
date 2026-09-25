//! Ticketmaster source against wiremock + saved fixtures (no real network).

mod common;

use common::fixture;
use thaleia::config::RateLimitConfig;
use thaleia::fetch::{FetchContext, user_agent};
use thaleia::sources::Source;
use thaleia::sources::ticketmaster::Ticketmaster;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn json_fixture(name: &str) -> serde_json::Value {
    serde_json::from_str(&fixture(&format!("ticketmaster/{name}"))).unwrap()
}

async fn mock_pages(server: &MockServer) {
    for page in ["0", "1"] {
        Mock::given(method("GET"))
            .and(path("/discovery/v2/events.json"))
            .and(query_param("apikey", "test-key"))
            .and(query_param("city", "London"))
            .and(query_param("countryCode", "GB"))
            .and(query_param(
                "segmentId",
                "KZFzniwnSyZfZ7v7na,KZFzniwnSyZfZ7v7n1",
            ))
            .and(query_param("page", page))
            .and(header("user-agent", user_agent().as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json_fixture(&format!("events-page-{page}.json"))),
            )
            .expect(1)
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn fetches_all_pages_and_normalises() {
    let server = MockServer::start().await;
    mock_pages(&server).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let tm = Ticketmaster::new(server.uri().parse().unwrap(), "test-key".into());

    let raws = tm.fetch(&ctx).await.expect("fetch");
    assert_eq!(raws.len(), 5);
    assert!(ctx.take_errors().is_empty());

    let normalised: Vec<_> = raws
        .iter()
        .map(|r| {
            (
                r.source_event_id.clone(),
                tm.normalise(r).expect("normalise"),
            )
        })
        .collect();
    // The musical is out of scope and skipped (None), not an error.
    assert!(
        normalised
            .iter()
            .any(|(id, n)| id == "Z698xZG2Z17aLion" && n.is_none())
    );
    insta::assert_json_snapshot!("ticketmaster_normalised", normalised);
}

#[tokio::test]
async fn respects_page_cap() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/discovery/v2/events.json"))
        .and(query_param("page", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("events-page-0.json")))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let tm = Ticketmaster::new(server.uri().parse().unwrap(), "test-key".into()).with_paging(3, 1);
    assert_eq!(tm.fetch(&ctx).await.unwrap().len(), 3);
}

#[tokio::test]
async fn api_error_fails_fetch_without_leaking_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/discovery/v2/events.json"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let tm = Ticketmaster::new(server.uri().parse().unwrap(), "super-secret".into());
    let err = tm.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("429"), "{err}");
    assert!(
        !err.contains("super-secret"),
        "API key leaked into error: {err}"
    );
}
