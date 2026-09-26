//! Somerset House scraper: snapshot tests over saved listing fixtures and
//! end-to-end fetches against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::somerset_house::{MAX_LISTING_PAGES, SomersetHouse, parse_listing};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/somerset-house";

#[test]
fn listing_snapshot() {
    let page = parse_listing(&fixture(&format!("{DIR}/whats-on-page-2.html"))).expect("parse");
    let ids: Vec<&str> = page
        .events
        .iter()
        .map(|r| r.source_event_id.as_str())
        .collect();
    insta::assert_json_snapshot!(
        "somerset_house_listing",
        serde_json::json!({ "total_pages": page.total_pages, "ids": ids })
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = SomersetHouse::new("https://www.somersethouse.org.uk".parse().unwrap());
    // Pagination is cumulative, so the last page holds every item.
    let page = parse_listing(&fixture(&format!("{DIR}/whats-on-page-2.html"))).expect("parse");
    let out: Vec<_> = page
        .events
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "source_url": raw.source_url,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("somerset_house_normalised", out);
}

#[test]
fn props_json_with_invalid_escape_parses() {
    let html = r#"<html><body><script id="props" type="application/json">{"x":"<\!-- c -->","data":{"page":{"items":{"edges":[{"node":{"url":"/whats-on/a","title":"A","dateStart":"2026-10-01T00:00"}}],"pageInfo":{"totalPages":1},"total":1}}}}</script></body></html>"#;
    let page = parse_listing(html).expect("parse");
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].source_event_id, "a");
}

/// A listing page whose props hold one item per id and claim `total_pages`.
fn listing_html(total_pages: u32, urls: &[&str]) -> String {
    let edges: Vec<_> = urls
        .iter()
        .map(|url| serde_json::json!({ "node": { "url": url, "title": url, "dateStart": "2026-10-01T00:00" } }))
        .collect();
    let props = serde_json::json!({
        "data": { "page": { "items": { "edges": edges, "pageInfo": { "totalPages": total_pages } } } }
    });
    format!(
        r#"<html><body><script id="props" type="application/json">{props}</script></body></html>"#
    )
}

#[test]
fn unusable_listing_items_are_rejected() {
    let page =
        parse_listing(&listing_html(1, &["/whats-on/a", "/about", "/whats-on/"])).expect("parse");
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.rejected.len(), 2, "{:?}", page.rejected);
}

#[test]
fn missing_props_is_a_parse_error() {
    assert!(parse_listing("<html></html>").is_err());
}

async fn mount_robots(server: &MockServer, body: &str) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
}

async fn mount_page(server: &MockServer, page: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .and(query_param("page", page))
        .respond_with(response)
        .mount(server)
        .await;
}

fn page_fixture(n: u32) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/whats-on-page-{n}.html")))
}

#[tokio::test]
async fn fetches_all_pages_via_fetch_context() {
    let server = MockServer::start().await;
    mount_robots(&server, &fixture(&format!("{DIR}/robots.txt"))).await;
    for n in [1, 2] {
        Mock::given(method("GET"))
            .and(path("/whats-on"))
            .and(query_param("page", n.to_string()))
            .respond_with(page_fixture(n))
            .expect(1)
            .mount(&server)
            .await;
    }

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = SomersetHouse::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(raws.len(), 24);
    let mut ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 24);
}

#[tokio::test]
async fn failed_later_page_is_a_soft_error() {
    let server = MockServer::start().await;
    mount_robots(&server, &fixture(&format!("{DIR}/robots.txt"))).await;
    mount_page(&server, "1", page_fixture(1)).await;
    mount_page(&server, "2", ResponseTemplate::new(500)).await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = SomersetHouse::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("page 2"), "{errors:?}");
    assert_eq!(raws.len(), 12);
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    mount_robots(&server, "User-agent: MuseNMingleBot\nDisallow: /whats-on\n").await;
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = SomersetHouse::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn failed_first_page_fails_the_fetch() {
    let server = MockServer::start().await;
    mount_robots(&server, &fixture(&format!("{DIR}/robots.txt"))).await;
    mount_page(&server, "1", ResponseTemplate::new(500)).await;
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .and(query_param("page", "2"))
        .respond_with(page_fixture(2))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let result = SomersetHouse::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await;
    assert!(result.is_err(), "{result:?}");
}

#[tokio::test]
async fn pagination_stops_at_the_page_cap() {
    let server = MockServer::start().await;
    mount_robots(&server, &fixture(&format!("{DIR}/robots.txt"))).await;
    for n in 1..=MAX_LISTING_PAGES + 1 {
        let url = format!("/whats-on/item-{n}");
        Mock::given(method("GET"))
            .and(path("/whats-on"))
            .and(query_param("page", n.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_string(listing_html(20, &[&url])))
            .expect(u64::from(n <= MAX_LISTING_PAGES))
            .mount(&server)
            .await;
    }

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = SomersetHouse::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), MAX_LISTING_PAGES as usize);
}

#[tokio::test]
async fn unusable_listing_items_are_reported_once() {
    let server = MockServer::start().await;
    mount_robots(&server, &fixture(&format!("{DIR}/robots.txt"))).await;
    for n in ["1", "2"] {
        let body = listing_html(2, &["/whats-on/a", "/about"]);
        mount_page(&server, n, ResponseTemplate::new(200).set_body_string(body)).await;
    }

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = SomersetHouse::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("/about"), "{errors:?}");
    assert_eq!(raws.len(), 1);
}
