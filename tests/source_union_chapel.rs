//! The Union Chapel scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::sources::Source;
use musenmingle::sources::union_chapel::{
    UnionChapel, parse_detail, parse_listing, raw_event, wanted,
};
use reqwest::StatusCode;
use serde_json::Value;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/union-chapel";
const SITE: &str = "https://unionchapel.org.uk";

/// The in-scope, upcoming cards on 2026-11-08 in listing order, whose
/// pages are all saved.
const SLUGS: &[&str] = &[
    "an-organ-recital-with-katy-silverman",
    "an-organ-recital-with-yvette-murphy",
    "johns-boys-welsh-male-voice-choir",
    "the-cinematic-orchestra",
    "a-christmas-organ-recital-with-wayne-marshall",
    "nu-civilisation-orchestra",
    "faures-requiem--in-a-service-setting-for-remembrance-sunday",
    "sunday-jam",
];

fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 8).unwrap()
}

fn page(slug: &str) -> String {
    fixture(&format!("{DIR}/{slug}.html"))
}

fn listing() -> String {
    fixture(&format!("{DIR}/whats-on.html"))
}

fn cards() -> Vec<Value> {
    let url = Url::parse(&format!("{SITE}/whats-on")).unwrap();
    parse_listing(&listing(), &url).expect("card list")
}

#[test]
fn saved_robots_allow_the_events_pages() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in ["/whats-on", "/whats-on/sunday-jam"] {
        assert!(
            robots.allowed(&Url::parse(SITE).unwrap().join(p).unwrap()),
            "{p}"
        );
    }
}

#[test]
fn the_listing_reads_every_card_and_keeps_the_arty_upcoming_ones() {
    let cards = cards();
    assert_eq!(cards.len(), 118);
    let slugs: Vec<_> = cards
        .iter()
        .filter(|c| wanted(c, listed_on()))
        .map(|c| c["slug"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(slugs, SLUGS);
    let first = &cards[0];
    assert_eq!(
        first["url"],
        "https://unionchapel.org.uk/whats-on/joan-as-police-woman-20-years-of-real-life-tour-6-nov"
    );
    assert_eq!(first["date"], "Fri 6 Nov 2026");
    assert!(
        first["image_url"]
            .as_str()
            .unwrap()
            .starts_with("https://s3.eu-west-2.amazonaws.com/cdn.unionchapel.org.uk/files/")
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = UnionChapel::new(SITE.parse().unwrap());
    let out: Vec<_> = cards()
        .iter()
        .filter(|c| wanted(c, listed_on()))
        .map(|card| {
            let slug = card["slug"].as_str().unwrap();
            let detail = parse_detail(&page(slug)).expect("event page");
            let raw = raw_event(card, listed_on(), detail);
            match s.normalise(&raw) {
                Ok(event) => serde_json::json!({
                    "source_event_id": raw.source_event_id,
                    "when": raw.payload["detail"]["when"],
                    "event": event,
                }),
                Err(e) => serde_json::json!({
                    "source_event_id": raw.source_event_id,
                    "error": e.to_string(),
                }),
            }
        })
        .collect();
    insta::assert_json_snapshot!("union_chapel_normalised", out);
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
        .and(path("/whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

/// The saved listing with its dates moved so that every card is upcoming
/// whenever the test runs: `wanted` compares them with today.
fn listing_in_the_future() -> String {
    listing()
        .replace(" 2026 ", " 2098 ")
        .replace(" 2027 ", " 2099 ")
}

#[tokio::test]
async fn fetches_the_listing_and_in_scope_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, listing_in_the_future()).await;
    for slug in SLUGS {
        Mock::given(method("GET"))
            .and(path(format!("/whats-on/{slug}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(slug)))
            .expect(1)
            .mount(&server)
            .await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = UnionChapel::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, SLUGS);
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(format!("{}/whats-on/{}", server.uri(), SLUGS[0]).as_str())
    );
    // robots.txt, the listing and the in-scope event pages only.
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2 + SLUGS.len()
    );
}

#[tokio::test]
async fn caps_event_page_fetches_and_reports_the_rest() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, listing_in_the_future()).await;
    Mock::given(method("GET"))
        .and(path(format!("/whats-on/{}", SLUGS[0])))
        .respond_with(ResponseTemplate::new(200).set_body_string(page(SLUGS[0])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/whats-on/{}", SLUGS[1])))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = UnionChapel::new(server.uri().parse().unwrap()).with_max_details(2);
    let raws = s.fetch(&ctx).await.expect("fetch");
    // Without its page an event has no time, so it is left out.
    assert_eq!(raws.len(), 1);
    assert_eq!(raws[0].source_event_id, SLUGS[0]);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), SLUGS.len() - 1, "{errors:?}");
    assert!(
        errors[0].contains("no event article on its page"),
        "{errors:?}"
    );
    assert!(
        errors[1].contains("over the 2 event-page cap"),
        "{errors:?}"
    );
    // robots.txt, the listing and two event pages.
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn an_empty_card_list_is_not_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(
        &server,
        r#"<html><body><div class="row" id="items"></div></body></html>"#.into(),
    )
    .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = UnionChapel::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(raws.is_empty());
    assert!(ctx.take_errors().is_empty());
}

#[tokio::test]
async fn a_page_without_the_card_list_is_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, "<html><body></body></html>".into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = UnionChapel::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no event cards"), "{err}");
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
    let err = UnionChapel::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
