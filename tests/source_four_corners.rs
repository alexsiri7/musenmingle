//! Four Corners scraper: snapshot tests over the saved What's on page and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::four_corners::{FourCorners, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/four-corners";
const SITE: &str = "https://www.fourcornersfilm.co.uk";

fn listing() -> Vec<RawEvent> {
    let url: Url = format!("{SITE}/whats-on/").parse().unwrap();
    parse_listing(&fixture(&format!("{DIR}/whats-on.html")), &url)
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!("four_corners_listing", listing());
}

#[test]
fn normalised_output_snapshot() {
    let s = FourCorners::new(SITE.parse().unwrap());
    let out: Vec<_> = listing()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "slug": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("four_corners_normalised", out);
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
async fn fetches_listing_via_fetch_context() {
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
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = FourCorners::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    let want: Vec<String> = listing().into_iter().map(|r| r.source_event_id).collect();
    assert_eq!(ids, want);
    let site = server.uri();
    for raw in &raws {
        let expected = format!("{site}/whats-on/{}", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(expected.as_str()));
        s.normalise(raw).expect("normalise");
    }
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("User-agent: MuseNMingleBot\nDisallow: /\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = FourCorners::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A page without cards (a template change) must reach the health checker
    // instead of producing clean, empty runs.
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><div class="events-index"><div class="event-item"><h4>No link</h4></div></div></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = FourCorners::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event cards found"), "{err}");
}
