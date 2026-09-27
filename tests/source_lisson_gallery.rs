//! The Lisson Gallery scraper: snapshot tests over saved HTML fixtures and
//! an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::lisson_gallery::{LissonGallery, parse_detail, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/lisson-gallery";
const SITE: &str = "https://lisson.com";

fn listing_paths() -> Vec<String> {
    parse_listing(&fixture(&format!("{DIR}/exhibitions.html")))
}

/// `/exhibitions/x` is saved as `detail/x.html`.
fn detail_fixture(path: &str) -> String {
    let slug = path.strip_prefix("/exhibitions/").unwrap();
    fixture(&format!("{DIR}/detail/{slug}.html"))
}

#[test]
fn listing_keeps_only_dated_london_cards() {
    // The saved listing has 23 cards: current and upcoming shows in five
    // cities, museum shows and past shows; two upcoming ones are in London.
    insta::assert_json_snapshot!("lisson_gallery_listing", listing_paths());
}

#[test]
fn normalised_output_snapshot() {
    let s = LissonGallery::new(SITE.parse().unwrap());
    let out: Vec<_> = listing_paths()
        .iter()
        .map(|p| {
            let url = Url::parse(SITE).unwrap().join(p).unwrap();
            let raw = parse_detail(&detail_fixture(p), &url).expect("JSON-LD");
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("lisson_gallery_normalised", out);
}

async fn serve_site(server: &MockServer, broken: &str) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/exhibitions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/exhibitions.html"))),
        )
        .mount(server)
        .await;
    for p in listing_paths() {
        let response = if p == broken {
            ResponseTemplate::new(500)
        } else {
            ResponseTemplate::new(200).set_body_string(detail_fixture(&p))
        };
        Mock::given(method("GET"))
            .and(path(p.as_str()))
            .respond_with(response)
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn fetches_listing_and_london_pages_via_fetch_context() {
    let server = MockServer::start().await;
    let paths = listing_paths();
    let broken = paths[1].clone();
    serve_site(&server, &broken).await;

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = LissonGallery::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains(&broken), "{errors:?}");
    assert_eq!(raws.len(), paths.len() - 1);
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(format!("{}{}", server.uri(), paths[0]).as_str())
    );
}

#[tokio::test]
async fn caps_exhibition_page_fetches() {
    let server = MockServer::start().await;
    serve_site(&server, "").await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = LissonGallery::new(server.uri().parse().unwrap())
        .with_max_detail_pages(1)
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 1);
    // robots.txt, listing, one page.
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn a_page_without_json_ld_is_reported() {
    let server = MockServer::start().await;
    serve_site(&server, "").await;
    let first = listing_paths()[0].clone();
    Mock::given(method("GET"))
        .and(path(first.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .with_priority(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    LissonGallery::new(server.uri().parse().unwrap())
        .with_max_detail_pages(1)
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("no ExhibitionEvent"), "{errors:?}");
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /exhibitions\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/exhibitions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = LissonGallery::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn a_listing_without_cards_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/exhibitions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body></body></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = LissonGallery::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no exhibition cards"), "{err}");
}
