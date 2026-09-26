//! D&AD events scraper: snapshot tests over saved pages and an end-to-end
//! fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::dandad::{Dandad, parse_detail, parse_listing};
use serde_json::Value;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/dandad";
const SITE: &str = "https://www.dandad.org";
const SLUG: &str = "annual-trend-report-insights-2026";

fn detail_url(slug: &str) -> Url {
    format!("{SITE}/events/{slug}").parse().unwrap()
}

#[test]
fn listing_snapshot() {
    let items = parse_listing(&fixture(&format!("{DIR}/events.html"))).expect("listing parses");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].path, format!("/events/{SLUG}"));
    insta::assert_json_snapshot!("dandad_listing", items);
}

#[test]
fn normalised_output_snapshot() {
    let s = Dandad::new(SITE.parse().unwrap());
    let html = fixture(&format!("{DIR}/detail/{SLUG}.html"));
    let raws = parse_detail(&html, &detail_url(SLUG), &Value::Null).expect("detail parses");
    let out: Vec<_> = raws
        .iter()
        .map(|raw| {
            serde_json::json!({
                "id": raw.source_event_id,
                "payload": raw.payload,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("dandad_normalised", out);
}

/// A listing page whose embedded data links the real fixture event plus a
/// page that fails over HTTP and one that isn't an event page.
fn synthetic_listing() -> String {
    let node = |url: &str| serde_json::json!({"node": {"__typename": "EventDetailPage", "title": "x", "url": url}});
    let props = serde_json::json!({"data": {"page": {"items": {"edges": [
        node(&format!("/events/{SLUG}")),
        node("/events/broken"),
        node("/events/malformed"),
        node("/not-an-event"),
    ]}}}});
    format!(
        r#"<html><body><script id="props" type="application/json">{props}</script></body></html>"#
    )
}

async fn mount_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_listing_and_details_via_fetch_context() {
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string(synthetic_listing()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/events/{SLUG}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixture(&format!("{DIR}/detail/{SLUG}.html"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events/broken"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events/malformed"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html><body>not an event</body></html>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/not-an-event"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Dandad::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors.iter().any(|e| e.contains("/events/broken")),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("/events/malformed") && e.contains("script#props")),
        "{errors:?}"
    );
    assert_eq!(raws.len(), 1);
    let expected = format!("{}/events/{SLUG}", server.uri());
    assert_eq!(raws[0].source_event_id, SLUG);
    assert_eq!(raws[0].source_url.as_deref(), Some(expected.as_str()));
    assert!(s.normalise(&raws[0]).expect("normalise").is_some());
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /events\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Dandad::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A template change must reach the health checker instead of producing
    // clean, empty runs.
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><script id="props" type="application/json">{"data":{"page":{"items":{"edges":[]}}}}</script></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Dandad::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event links"), "{err}");
}

#[tokio::test]
async fn a_listing_that_says_zero_events_is_an_empty_run() {
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><script id="props" type="application/json">{"data":{"page":{"items":{"edges":[],"total":0}}}}</script></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Dandad::new(server.uri().parse().unwrap());
    assert!(s.fetch(&ctx).await.expect("fetch").is_empty());
    assert!(ctx.take_errors().is_empty());
}
