//! The Estorick Collection scraper: snapshot tests over the saved listings
//! and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::estorick_collection::{EstorickCollection, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/estorick-collection";
const SITE: &str = "https://www.estorickcollection.com";
const PAGES: [(&str, &str); 3] = [
    ("/events", "events.html"),
    ("/exhibitions", "exhibitions.html"),
    ("/exhibitions/in-the/future", "exhibitions-future.html"),
];

fn listing(path: &str, file: &str) -> Vec<RawEvent> {
    let url = Url::parse(SITE).unwrap().join(path).unwrap();
    parse_listing(&fixture(&format!("{DIR}/{file}")), &url)
        .into_iter()
        .map(|r| r.expect("card"))
        .collect()
}

#[test]
fn normalised_output_snapshot() {
    let s = EstorickCollection::new(SITE.parse().unwrap());
    let out: Vec<_> = PAGES
        .iter()
        .flat_map(|(path, file)| listing(path, file))
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "date": raw.payload["date"],
                "time": raw.payload["time"],
                "tags": raw.payload["tags"],
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("estorick_collection_normalised", out);
}

#[test]
fn listings_have_the_expected_cards() {
    let counts: Vec<usize> = PAGES.iter().map(|(p, f)| listing(p, f).len()).collect();
    // 15 events; the current exhibition; one future exhibition.
    assert_eq!(counts, [15, 1, 1]);
}

async fn serve_site(server: &MockServer, broken: Option<&str>) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(server)
        .await;
    for (p, file) in PAGES {
        let response = if Some(p) == broken {
            ResponseTemplate::new(500)
        } else {
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/{file}")))
        };
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn fetches_the_three_listings_via_fetch_context() {
    let server = MockServer::start().await;
    serve_site(&server, None).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = EstorickCollection::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 17);
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(format!("{}/events/family-art-day-27-september", server.uri()).as_str())
    );
}

#[tokio::test]
async fn a_failing_exhibitions_page_is_reported_not_fatal() {
    let server = MockServer::start().await;
    serve_site(&server, Some("/exhibitions/in-the/future")).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = EstorickCollection::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("in-the/future"), "{errors:?}");
    assert_eq!(raws.len(), 16);
}

#[tokio::test]
async fn a_failing_events_page_fails_the_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    assert!(
        EstorickCollection::new(server.uri().parse().unwrap())
            .fetch(&ctx)
            .await
            .is_err()
    );
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
    let err = EstorickCollection::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn an_events_page_without_cards_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><main></main></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = EstorickCollection::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no event cards"), "{err}");
}
