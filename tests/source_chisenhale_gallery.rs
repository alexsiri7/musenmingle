//! Chisenhale Gallery scraper: snapshot tests over the saved What's On page
//! and an end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::chisenhale_gallery::{ChisenhaleGallery, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/chisenhale-gallery";
const SITE: &str = "https://chisenhale.org.uk";

/// The day the fixture was fetched: year inference is pinned to it.
fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()
}

fn page_url() -> Url {
    format!("{SITE}/whats-on/").parse().unwrap()
}

fn listing() -> Vec<musenmingle::model::RawEvent> {
    parse_listing(
        &fixture(&format!("{DIR}/whats-on.html")),
        &page_url(),
        listed_on(),
    )
}

#[test]
fn listing_snapshot() {
    let raws = listing();
    assert_eq!(raws.len(), 6);
    insta::assert_json_snapshot!("chisenhale_gallery_listing", raws);
}

#[test]
fn normalised_output_snapshot() {
    let s = ChisenhaleGallery::new(SITE.parse().unwrap());
    let out: Vec<_> = listing()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "id": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("chisenhale_gallery_normalised", out);
}

async fn mount_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_the_listing_only_via_fetch_context() {
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/whats-on.html"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    // Crawl-delay 20: no detail pages are ever requested.
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex("^/(project|whats-on)/.+"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = ChisenhaleGallery::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 6);
    let site = server.uri();
    for raw in &raws {
        let expected = format!("{site}/{}/", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(expected.as_str()));
        assert_eq!(raw.payload["url"], expected);
        assert!(raw.payload["listed_on"].is_string());
        assert!(s.normalise(raw).expect("normalise").is_some());
    }
    assert_eq!(raws[0].source_event_id, "project/wakaliga-uganda");
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /whats-on\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = ChisenhaleGallery::new(server.uri().parse().unwrap());
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
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><main><section class="block-calendar"><ul class="list"></ul></section></main></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = ChisenhaleGallery::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event cards"), "{err}");
}
