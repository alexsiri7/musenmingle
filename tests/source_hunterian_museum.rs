//! The Hunterian Museum scraper: snapshot tests over saved HTML fixtures and
//! an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::hunterian_museum::{HunterianMuseum, parse_detail, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/hunterian-museum";
const SITE: &str = "https://hunterianmuseum.org";

fn listing_paths() -> Vec<String> {
    let page = Url::parse(&format!("{SITE}/whats-on/")).unwrap();
    parse_listing(&fixture(&format!("{DIR}/whats-on.html")), &page)
}

/// Pages are saved flat: `/events/x` is `detail/events--x.html`.
fn detail_fixture(path: &str) -> String {
    let id = path.strip_prefix('/').unwrap();
    fixture(&format!("{DIR}/detail/{}.html", id.replace('/', "--")))
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!("hunterian_museum_listing", listing_paths());
}

#[test]
fn normalised_output_snapshot() {
    let s = HunterianMuseum::new(SITE.parse().unwrap());
    let out: Vec<_> = listing_paths()
        .iter()
        .map(|p| {
            let url = Url::parse(SITE).unwrap().join(p).unwrap();
            let raw = parse_detail(&detail_fixture(p), &url).expect("title");
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "facts": raw.payload["facts"],
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("hunterian_museum_normalised", out);
}

async fn serve_site(server: &MockServer, broken: &str) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/whats-on.html"))),
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
async fn fetches_listing_and_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    let paths = listing_paths();
    let broken = paths[1].clone();
    serve_site(&server, &broken).await;

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = HunterianMuseum::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains(&broken), "{errors:?}");
    assert_eq!(raws.len(), paths.len() - 1);
    let exhibition = raws
        .iter()
        .find(|r| r.source_event_id == "exhibitions/the-operating-theatre-250-years")
        .expect("exhibition page");
    assert_eq!(
        exhibition.source_url.as_deref(),
        Some(
            format!(
                "{}/exhibitions/the-operating-theatre-250-years",
                server.uri()
            )
            .as_str()
        )
    );
}

#[tokio::test]
async fn caps_event_page_fetches() {
    let server = MockServer::start().await;
    serve_site(&server, "").await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = HunterianMuseum::new(server.uri().parse().unwrap())
        .with_max_detail_pages(3)
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 3);
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn a_page_without_a_title_is_reported() {
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
    HunterianMuseum::new(server.uri().parse().unwrap())
        .with_max_detail_pages(1)
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("no title"), "{errors:?}");
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /whats-on/\n"),
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
    let err = HunterianMuseum::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn a_listing_without_event_links_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body></body></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = HunterianMuseum::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no event links"), "{err}");
}
