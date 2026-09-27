//! The Conway Hall scraper: a snapshot test over the saved listing and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::conway_hall::{ConwayHall, parse_listing};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/conway-hall";
const SITE: &str = "https://www.conwayhall.org.uk";

fn listing() -> Vec<RawEvent> {
    let page = Url::parse(&format!("{SITE}/whats-on/")).unwrap();
    parse_listing(&fixture(&format!("{DIR}/listing.html")), &page)
        .into_iter()
        .map(|r| r.expect("card"))
        .collect()
}

#[test]
fn saved_robots_allow_the_listing() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    assert!(robots.allowed(&Url::parse(&format!("{SITE}/whats-on/")).unwrap()));
}

#[test]
fn every_card_has_its_event() {
    let raws = listing();
    assert_eq!(raws.len(), 37);
    assert!(
        raws.iter()
            .all(|r| r.source_event_id.starts_with("/whats-on/event/"))
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = ConwayHall::new(SITE.parse().unwrap());
    let out: Vec<_> = listing()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "categories": raw.payload["categories"],
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("conway_hall_normalised", out);
}

#[tokio::test]
async fn fetches_the_listing_via_fetch_context() {
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
        .and(path("/whats-on/"))
        .respond_with(
            // The saved page links absolutely to the site; serve it as ours.
            ResponseTemplate::new(200).set_body_string(
                fixture(&format!("{DIR}/listing.html")).replace(SITE, &server.uri()),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = ConwayHall::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.clone()).collect();
    let want: Vec<_> = listing().into_iter().map(|r| r.source_event_id).collect();
    assert_eq!(ids, want);
}

#[tokio::test]
async fn an_empty_listing_is_an_error() {
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
    let err = ConwayHall::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no event cards"), "{err}");
}
