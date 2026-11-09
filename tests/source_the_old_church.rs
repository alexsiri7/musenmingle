//! The Old Church scraper: a snapshot test over the saved What's On page and
//! an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::sources::Source;
use musenmingle::sources::the_old_church::{TheOldChurch, parse_listing, raw_event};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/the-old-church";
const SITE: &str = "https://www.theoldchurch.org.uk";

fn whats_on() -> String {
    fixture(&format!("{DIR}/whats-on.html"))
}

#[test]
fn saved_robots_allow_the_listing_and_event_pages() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in ["/whats-on", "/event-details/hidden-gems"] {
        assert!(
            robots.allowed(&Url::parse(SITE).unwrap().join(p).unwrap()),
            "{p}"
        );
    }
}

#[test]
fn the_listing_holds_the_first_page_of_events() {
    let events = parse_listing(&whats_on()).expect("Wix Events data");
    assert_eq!(events.len(), 18);
}

#[test]
fn a_page_without_wix_events_data_has_no_listing() {
    assert!(parse_listing("<html><body></body></html>").is_none());
    let other_app = r#"<script type="application/json" id="wix-warmup-data">{"appsWarmupData":{"other":{}}}</script>"#;
    assert!(parse_listing(other_app).is_none());
}

#[test]
fn normalised_output_snapshot() {
    let s = TheOldChurch::new(SITE.parse().unwrap());
    let base = Url::parse(SITE).unwrap();
    let out: Vec<_> = parse_listing(&whats_on())
        .expect("Wix Events data")
        .into_iter()
        .map(|ev| {
            let raw = raw_event(ev, &base).expect("id and slug");
            match s.normalise(&raw) {
                Ok(event) => serde_json::json!({
                    "source_event_id": raw.source_event_id,
                    "event": event,
                }),
                Err(e) => serde_json::json!({
                    "source_event_id": raw.source_event_id,
                    "error": e.to_string(),
                }),
            }
        })
        .collect();
    insta::assert_json_snapshot!("the_old_church_normalised", out);
}

async fn serve_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn serve_listing(server: &MockServer, body: String) {
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_only_the_listing_via_fetch_context() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, whats_on()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = TheOldChurch::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 18);
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(
            format!(
                "{}/event-details/quiet-folk-and-detta-danford-album-launch",
                server.uri()
            )
            .as_str()
        )
    );
    // robots.txt and the listing.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_page_without_wix_events_data_is_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, "<html><body></body></html>".into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = TheOldChurch::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no Wix Events data"), "{err}");
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
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = TheOldChurch::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
