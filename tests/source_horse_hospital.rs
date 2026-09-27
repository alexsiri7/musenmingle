//! The Horse Hospital scraper: snapshot tests over saved HTML fixtures and
//! an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::horse_hospital::{HorseHospital, parse_detail, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/horse-hospital";
const SITE: &str = "https://www.thehorsehospital.com";

fn listing_paths() -> Vec<String> {
    let page = Url::parse(&format!("{SITE}/whats-on/")).unwrap();
    parse_listing(&fixture(&format!("{DIR}/whats-on.html")), &page)
}

/// Event pages are saved flat: `/events/un/stable-8` is `detail/un--stable-8.html`.
fn detail_fixture(path: &str) -> String {
    let id = path.strip_prefix("/events/").unwrap();
    fixture(&format!("{DIR}/detail/{}.html", id.replace('/', "--")))
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!("horse_hospital_listing", listing_paths());
}

#[test]
fn normalised_output_snapshot() {
    let s = HorseHospital::new(SITE.parse().unwrap());
    let out: Vec<_> = listing_paths()
        .iter()
        .map(|p| {
            let url = Url::parse(SITE).unwrap().join(p).unwrap();
            let raw = parse_detail(&detail_fixture(p), &url).expect("JSON-LD Event");
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "categories": raw.payload["categories"],
                "admission": raw.payload["admission"],
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("horse_hospital_normalised", out);
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
    let broken = paths[0].clone();
    serve_site(&server, &broken).await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = HorseHospital::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains(&broken), "{errors:?}");
    assert_eq!(raws.len(), paths.len() - 1);
    let slashed = raws
        .iter()
        .find(|r| r.source_event_id == "un/stable-8")
        .expect("slug with a slash");
    assert_eq!(
        slashed.source_url.as_deref(),
        Some(format!("{}/events/un/stable-8", server.uri()).as_str())
    );
}

#[tokio::test]
async fn caps_event_page_fetches() {
    let server = MockServer::start().await;
    serve_site(&server, "").await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = HorseHospital::new(server.uri().parse().unwrap())
        .with_max_detail_pages(3)
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 3);
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
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
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    HorseHospital::new(server.uri().parse().unwrap())
        .with_max_detail_pages(1)
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("no JSON-LD Event"), "{errors:?}");
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
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = HorseHospital::new(server.uri().parse().unwrap())
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
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = HorseHospital::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no event links"), "{err}");
}
