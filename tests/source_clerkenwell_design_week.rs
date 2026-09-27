//! Clerkenwell Design Week: snapshot of the normalised festival from the
//! saved (trimmed) homepage, and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::clerkenwell_design_week::{ClerkenwellDesignWeek, parse_home};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/clerkenwell-design-week";

fn scraper(base: &str) -> ClerkenwellDesignWeek {
    ClerkenwellDesignWeek::new(format!("{base}/").parse().unwrap())
}

#[test]
fn normalised_output_snapshot() {
    let s = scraper("https://www.clerkenwelldesignweek.com");
    let out: Vec<_> = parse_home(&fixture(&format!("{DIR}/home.html")))
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "source_url": raw.source_url,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    assert_eq!(out.len(), 1);
    insta::assert_json_snapshot!("clerkenwell_design_week_normalised", out);
}

#[tokio::test]
async fn fetches_the_homepage_via_fetch_context() {
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
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/home.html"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = scraper(&server.uri()).fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 1);
    assert_eq!(raws[0].source_event_id, "2027-05-25");
}

#[tokio::test]
async fn homepage_without_an_event_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body>soon</body></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = scraper(&server.uri())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no JSON-LD Event"), "{err}");
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
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("never fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = scraper(&server.uri())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
