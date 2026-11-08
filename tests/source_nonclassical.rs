//! The Nonclassical scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::sources::Source;
use musenmingle::sources::nonclassical::{Nonclassical, parse_detail, parse_listing};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/nonclassical";
const SITE: &str = "https://www.nonclassical.co.uk";
const LISTED: &str = "/events/2026/9/18/jocelyn-campbell-lo-end-pav";

/// Saved event pages: the one listed on 2026-11-08, and past ones reached
/// through its "Previous" links (London by postcode or by name, BST, Bristol
/// (skipped) and an address of only "United Kingdom" (an error)).
const PAGES: &[(&str, &str)] = &[
    (LISTED, "jocelyn-campbell-lo-end-pav"),
    (
        "/events/2026/10/31/wack-x-lovemusic-presented-by-arnolfini-lovemusic-nonclassical",
        "wack-x-lovemusic-arnolfini",
    ),
    (
        "/events/wack-collective-lovemusic",
        "wack-collective-lovemusic",
    ),
    ("/events/outside-the-lines-vol-8", "outside-the-lines-vol-8"),
    ("/events/kogg-album-launch", "kogg-album-launch"),
];

fn page(file: &str) -> String {
    fixture(&format!("{DIR}/{file}.html"))
}

fn listing_paths() -> Vec<String> {
    let url = Url::parse(&format!("{SITE}/events")).unwrap();
    parse_listing(&fixture(&format!("{DIR}/events.html")), &url).expect("event list")
}

#[test]
fn saved_robots_allow_the_events_pages() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in ["/events", LISTED] {
        assert!(
            robots.allowed(&Url::parse(SITE).unwrap().join(p).unwrap()),
            "{p}"
        );
    }
}

#[test]
fn the_listing_links_its_one_upcoming_event() {
    assert_eq!(listing_paths(), [LISTED]);
}

#[test]
fn the_listing_keeps_each_event_page_once_on_its_own_host() {
    let url = Url::parse(&format!("{SITE}/events")).unwrap();
    let html = listing_of(&[
        "/events/a",
        "https://example.com/events/elsewhere",
        "/events/b?format=ical",
        "/events/",
        "/about",
        &format!("{SITE}/events/a"),
        "/events/c",
    ]);
    assert_eq!(
        parse_listing(&html, &url).expect("event list"),
        ["/events/a", "/events/c"]
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = Nonclassical::new(SITE.parse().unwrap());
    let out: Vec<_> = PAGES
        .iter()
        .map(|(p, file)| {
            let url = Url::parse(SITE).unwrap().join(p).unwrap();
            let raw = parse_detail(&page(file), &url).expect("JSON-LD Event");
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
    insta::assert_json_snapshot!("nonclassical_normalised", out);
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
    serve_listing(&server, fixture(&format!("{DIR}/events.html"))).await;
    Mock::given(method("GET"))
        .and(path(LISTED))
        .respond_with(ResponseTemplate::new(200).set_body_string(page(PAGES[0].1)))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = Nonclassical::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 1);
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(format!("{}{LISTED}", server.uri()).as_str())
    );
}

fn listing_of(paths: &[&str]) -> String {
    let articles: String = paths
        .iter()
        .map(|p| {
            format!(
                r#"<article class="eventlist-event"><h1 class="entry-title"><a href="{p}">x</a></h1></article>"#
            )
        })
        .collect();
    format!(r#"<html><body><div class="eventlist">{articles}</div></body></html>"#)
}

#[tokio::test]
async fn caps_event_page_fetches_and_reports_the_rest() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(
        &server,
        listing_of(&["/events/a", "/events/b", "/events/c"]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/events/a"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page(PAGES[0].1)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events/b"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = Nonclassical::new(server.uri().parse().unwrap())
        .with_max_detail_pages(2)
        .fetch(&ctx)
        .await
        .expect("fetch");
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, ["a"]);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].contains("only the first 2"), "{errors:?}");
    assert!(
        errors[1].contains("/events/b: no JSON-LD Event"),
        "{errors:?}"
    );
    // robots.txt, the listing and two event pages.
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn an_empty_event_list_is_not_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, listing_of(&[])).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = Nonclassical::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(raws.is_empty());
    assert!(ctx.take_errors().is_empty());
}

#[tokio::test]
async fn a_page_without_an_event_list_is_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_listing(&server, "<html><body></body></html>".into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = Nonclassical::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no event list"), "{err}");
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
    let err = Nonclassical::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
