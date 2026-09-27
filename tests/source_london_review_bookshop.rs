//! London Review Bookshop scraper: snapshot tests over the saved Events page
//! and an end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::london_review_bookshop::{LondonReviewBookshop, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/london-review-bookshop";
const SITE: &str = "https://www.londonreviewbookshop.co.uk";

/// The London date the fixture was saved on.
fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
}

fn listing() -> Vec<RawEvent> {
    let url: Url = format!("{SITE}/events").parse().unwrap();
    parse_listing(&fixture(&format!("{DIR}/events.html")), &url, listed_on())
}

#[test]
fn listing_reads_every_event_and_no_podcast() {
    let raws = listing();
    assert_eq!(raws.len(), 11);
    assert!(
        raws.iter()
            .all(|r| r.source_event_id.chars().all(|c| c.is_ascii_digit()))
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = LondonReviewBookshop::new(SITE.parse().unwrap());
    let out: Vec<_> = listing()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "id": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("london_review_bookshop_normalised", out);
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
        .and(path("/events"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/events.html"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = LondonReviewBookshop::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    let want: Vec<String> = listing().into_iter().map(|r| r.source_event_id).collect();
    assert_eq!(ids, want);
    let expected = format!("{}/events", server.uri());
    for raw in &raws {
        assert_eq!(raw.source_url.as_deref(), Some(expected.as_str()));
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
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = LondonReviewBookshop::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A page without event previews (a template change) must reach the
    // health checker instead of producing clean, empty runs.
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><section class="pv-preview" itemtype="https://schema.org/PodcastEpisode"><h2>A podcast</h2></section></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = LondonReviewBookshop::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event previews found"), "{err}");
}
