//! The Stone Nest scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::sources::Source;
use musenmingle::sources::stone_nest::{StoneNest, parse_detail, parse_listing, raw_event};
use reqwest::StatusCode;
use serde_json::Value;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/stone-nest";
const SITE: &str = "https://www.stonenest.org";

/// The upcoming events on 2026-11-08, whose pages are all saved.
const SLUGS: &[&str] = &[
    "muireann-bradley",
    "faris-ishaq",
    "manu-delago-x-max-zt",
    "same-moon-in-the-same-world-2026",
    "orpheus-and-eurydice",
    "soho-hidden-jazz-club-05-12-26",
    "from-one-glasse-to-another-day-1",
    "from-one-glasse-to-another-day-2",
];

fn page(slug: &str) -> String {
    fixture(&format!("{DIR}/{slug}.html"))
}

fn listing() -> String {
    fixture(&format!("{DIR}/events.html"))
}

fn cards() -> Vec<Value> {
    let url = Url::parse(&format!("{SITE}/events")).unwrap();
    parse_listing(&listing(), &url).expect("upcoming list")
}

#[test]
fn saved_robots_allow_the_events_pages() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in ["/events", "/events/muireann-bradley"] {
        assert!(
            robots.allowed(&Url::parse(SITE).unwrap().join(p).unwrap()),
            "{p}"
        );
    }
}

#[test]
fn the_listing_reads_only_the_upcoming_events() {
    let slugs: Vec<_> = cards()
        .iter()
        .map(|c| c["slug"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(slugs, SLUGS);
}

#[test]
fn listing_cards_snapshot() {
    insta::assert_json_snapshot!("stone_nest_cards", cards());
}

#[test]
fn normalised_output_snapshot() {
    let s = StoneNest::new(SITE.parse().unwrap());
    let listed_on = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
    let out: Vec<_> = cards()
        .iter()
        .map(|card| {
            let slug = card["slug"].as_str().unwrap();
            let detail = parse_detail(&page(slug)).expect("event page");
            let raw = raw_event(card, listed_on, Some(detail));
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
    insta::assert_json_snapshot!("stone_nest_normalised", out);
}

#[test]
fn a_free_event_page_says_so() {
    let html = page(SLUGS[0])
        .replace("tickets-free w-condition-invisible", "tickets-free")
        .replace(
            r#"<div class="tickets-buy">"#,
            r#"<div class="tickets-buy w-condition-invisible">"#,
        );
    assert_eq!(parse_detail(&html).unwrap()["free"], true);
    assert_eq!(parse_detail(&page(SLUGS[0])).unwrap()["free"], false);
}

async fn serve_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
}

async fn serve_listing(server: &MockServer, body: String) {
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_listing_and_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, listing()).await;
    for slug in SLUGS {
        Mock::given(method("GET"))
            .and(path(format!("/events/{slug}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(slug)))
            .expect(1)
            .mount(&server)
            .await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = StoneNest::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), SLUGS.len());
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(format!("{}/events/{}", server.uri(), SLUGS[0]).as_str())
    );
    assert!(raws.iter().all(|r| !r.payload["detail"].is_null()));
    assert!(raws.iter().all(|r| s.normalise(r).unwrap().is_some()));
}

#[tokio::test]
async fn caps_event_page_fetches_and_reports_the_rest() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, listing()).await;
    Mock::given(method("GET"))
        .and(path(format!("/events/{}", SLUGS[0])))
        .respond_with(ResponseTemplate::new(200).set_body_string(page(SLUGS[0])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/events/{}", SLUGS[1])))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = StoneNest::new(server.uri().parse().unwrap()).with_max_details(2);
    let raws = s.fetch(&ctx).await.expect("fetch");
    // Every card is kept; the ones without their page use the card alone.
    assert_eq!(raws.len(), SLUGS.len());
    assert!(!raws[0].payload["detail"].is_null());
    assert!(raws[1..].iter().all(|r| r.payload["detail"].is_null()));
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), SLUGS.len() - 1, "{errors:?}");
    assert!(
        errors[0].contains("no event heading on its page"),
        "{errors:?}"
    );
    assert!(
        errors[1].contains("over the 2 event-page cap"),
        "{errors:?}"
    );
    // robots.txt, the listing and two event pages.
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
    let without_page = s.normalise(&raws[2]).unwrap().unwrap();
    assert!(without_page.description.is_some());
}

#[tokio::test]
async fn an_empty_upcoming_list_is_not_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(
        &server,
        r#"<html><body><div class="events-current w-dyn-list"><div class="w-dyn-empty">No items found.</div></div></body></html>"#.into(),
    )
    .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = StoneNest::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(raws.is_empty());
    assert!(ctx.take_errors().is_empty());
}

#[tokio::test]
async fn a_page_without_the_upcoming_list_is_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, "<html><body></body></html>".into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = StoneNest::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no upcoming events list"), "{err}");
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /events\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = StoneNest::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
