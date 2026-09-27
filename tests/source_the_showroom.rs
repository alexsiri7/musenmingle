//! The Showroom scraper: a snapshot test over the saved listing and detail
//! pages, and an end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::the_showroom::{
    EVENTS_PATH, EXHIBITIONS_PATH, Section, TheShowroom, parse_detail, parse_listing, raw_event,
};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/the-showroom";
const SITE: &str = "https://theshowroom.org";

const DETAILS: [(&str, &str); 2] = [
    (
        "/events/itle-made-by-hands-the-fabric-of-creative-action",
        "detail-itle-made-by-hands-the-fabric-of-creative-action.html",
    ),
    ("/events/deeper-than-rap", "detail-deeper-than-rap.html"),
];

/// The date the fixtures were saved.
fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
}

fn detail_for(path: &str) -> Option<serde_json::Value> {
    DETAILS
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, f)| parse_detail(&fixture(&format!("{DIR}/{f}"))))
}

/// The cards as `fetch` would store them.
fn raws() -> Vec<RawEvent> {
    let site = Url::parse(SITE).unwrap();
    let exhibitions = parse_listing(
        &fixture(&format!("{DIR}/exhibitions.html")),
        Section::Exhibitions,
    );
    let events = parse_listing(&fixture(&format!("{DIR}/events.html")), Section::Events);
    exhibitions
        .cards
        .iter()
        .map(|c| raw_event(c, Section::Exhibitions, &site, listed_on(), None))
        .chain(events.cards.iter().map(|c| {
            let detail = detail_for(c["path"].as_str().unwrap());
            raw_event(c, Section::Events, &site, listed_on(), detail)
        }))
        .collect()
}

#[test]
fn saved_robots_allow_the_pages() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in [EXHIBITIONS_PATH, EVENTS_PATH, DETAILS[0].0, DETAILS[1].0] {
        assert!(robots.allowed(&Url::parse(&format!("{SITE}{p}")).unwrap()));
    }
}

#[test]
fn only_current_cards_are_read() {
    let ex = parse_listing(
        &fixture(&format!("{DIR}/exhibitions.html")),
        Section::Exhibitions,
    );
    assert_eq!(ex.cards.len(), 1);
    assert!(ex.total > 50, "{}", ex.total);
    let ev = parse_listing(&fixture(&format!("{DIR}/events.html")), Section::Events);
    assert_eq!(ev.cards.len(), 2);
    assert!(ev.total > ev.cards.len());
    for r in raws() {
        assert!(
            r.payload["card"]["date_text"].is_string(),
            "{}",
            r.source_event_id
        );
    }
}

#[test]
fn normalised_output_snapshot() {
    let s = TheShowroom::new(SITE.parse().unwrap());
    let out: Vec<_> = raws()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "date_text": raw.payload["card"]["date_text"],
                "type": raw.payload["detail"]["type"],
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("the_showroom_normalised", out);
}

#[tokio::test]
async fn fetches_listings_and_details_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let pages = [
        (EXHIBITIONS_PATH, "exhibitions.html"),
        (EVENTS_PATH, "events.html"),
        DETAILS[0],
        DETAILS[1],
    ];
    for (p, file) in pages {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/{file}"))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = TheShowroom::new(server.uri().parse().unwrap());
    let got = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let want = raws();
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g.source_event_id, w.source_event_id);
        assert_eq!(g.payload["card"], w.payload["card"]);
        assert_eq!(g.payload["detail"], w.payload["detail"]);
    }
}

#[tokio::test]
async fn a_failed_detail_page_keeps_the_card() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    for (p, file) in [
        (EXHIBITIONS_PATH, "exhibitions.html"),
        (EVENTS_PATH, "events.html"),
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/{file}"))),
            )
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(DETAILS[1].0))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(DETAILS[0].0))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/{}", DETAILS[0].1))),
        )
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let got = TheShowroom::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(got.len(), 3);
    assert!(got[2].payload["detail"].is_null());
    assert_eq!(ctx.take_errors().len(), 1);
}

#[tokio::test]
async fn robots_disallow_stops_the_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /\n"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(EXHIBITIONS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    assert!(
        TheShowroom::new(server.uri().parse().unwrap())
            .fetch(&ctx)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_page_without_any_cards_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(EXHIBITIONS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><main></main></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = TheShowroom::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no exhibition cards"), "{err}");
}

#[tokio::test]
async fn a_quiet_season_is_not_an_error() {
    // Only archive cards: nothing current, but the layout is intact.
    let archive_only = r#"<html><main><section><div data-archive>
        <article class="group"><a href="https://theshowroom.org/exhibitions/old">
        <h3>Old</h3><p><time>1 June 2026</time></p></a></article>
        </div></section></main></html>"#;
    let events_archive_only = archive_only.replace("/exhibitions/", "/events/");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(EXHIBITIONS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(archive_only))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(EVENTS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(events_archive_only))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let got = TheShowroom::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(got.is_empty());
    assert!(ctx.take_errors().is_empty());
}
