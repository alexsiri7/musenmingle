//! The Photographers' Gallery scraper: snapshot tests over saved HTML
//! fixtures and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::photographers_gallery::{PhotographersGallery, next_page, parse_listing};
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/photographers-gallery";
const SITE: &str = "https://thephotographersgallery.org.uk";

fn page(n: usize) -> String {
    match n {
        0 => fixture(&format!("{DIR}/whats-on.html")),
        n => fixture(&format!("{DIR}/whats-on-page-{n}.html")),
    }
}

#[test]
fn normalised_output_snapshot() {
    let s = PhotographersGallery::new(SITE.parse().unwrap());
    let out: Vec<_> = (0..2)
        .flat_map(|n| {
            let url = Url::parse(&format!("{SITE}/whats-on?page={n}")).unwrap();
            parse_listing(&page(n), &url)
        })
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "post_type": raw.payload["post_type"],
                "date_text": raw.payload["date_text"],
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("photographers_gallery_normalised", out);
}

#[test]
fn pager_links_to_the_second_page_only() {
    let first = Url::parse(&format!("{SITE}/whats-on")).unwrap();
    let second = next_page(&page(0), &first).expect("a next page");
    assert_eq!(second.as_str(), format!("{SITE}/whats-on?page=1"));
    assert_eq!(next_page(&page(1), &second), None);
}

async fn serve_site(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page(1)))
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page(0)))
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_both_listing_pages_via_fetch_context() {
    let server = MockServer::start().await;
    serve_site(&server).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = PhotographersGallery::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let expected: usize = (0..2)
        .map(|n| {
            let url = Url::parse(&format!("{SITE}/whats-on?page={n}")).unwrap();
            parse_listing(&page(n), &url).len()
        })
        .sum();
    assert_eq!(raws.len(), expected);
    assert!(raws.iter().any(|r| r.source_event_id == "roots"));
    assert!(
        raws.iter()
            .any(|r| r.source_event_id == "exhibition-tour-sanle-sory-colour-years-0")
    );
    // robots.txt + two listing pages.
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn caps_listing_pages() {
    let server = MockServer::start().await;
    serve_site(&server).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = PhotographersGallery::new(server.uri().parse().unwrap())
        .with_max_pages(1)
        .fetch(&ctx)
        .await
        .expect("fetch");
    let url = Url::parse(&format!("{SITE}/whats-on")).unwrap();
    assert_eq!(raws.len(), parse_listing(&page(0), &url).len());
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_failing_second_page_is_a_soft_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(1)
        .mount(&server)
        .await;
    serve_site(&server).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = PhotographersGallery::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("listing page 2"), "{errors:?}");
    assert!(!raws.is_empty());
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
        .and(path("/whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = PhotographersGallery::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn a_listing_without_teasers_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body></body></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = PhotographersGallery::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no event teasers"), "{err}");
}
