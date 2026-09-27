//! Gasworks scraper: snapshot tests over the saved listings and an
//! end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::gasworks::{Gasworks, Section, parse_listing};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/gasworks";
const SITE: &str = "https://www.gasworks.org.uk";

/// The day the fixtures were fetched.
fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
}

fn listing(file: &str, p: &str, section: Section) -> Vec<RawEvent> {
    let url: Url = format!("{SITE}{p}").parse().unwrap();
    parse_listing(
        &fixture(&format!("{DIR}/{file}")),
        &url,
        section,
        listed_on(),
    )
    .items
}

fn all() -> Vec<RawEvent> {
    let mut v = listing("exhibitions.html", "/exhibitions/", Section::Exhibitions);
    v.extend(listing("events.html", "/events/", Section::Events));
    v
}

#[test]
fn robots_allows_the_listings_and_asks_for_a_slow_rate() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in ["/exhibitions/", "/events/"] {
        assert!(robots.allowed(&Url::parse(&format!("{SITE}{p}")).unwrap()));
    }
    assert_eq!(
        robots.crawl_delay(),
        Some(std::time::Duration::from_secs(20))
    );
    // The 1/60 Request-rate is a built-in floor.
    let c = RateLimitConfig::default();
    assert_eq!(
        c.interval_for("www.gasworks.org.uk"),
        std::time::Duration::from_secs(60)
    );
}

#[test]
fn only_current_and_forthcoming_cards_are_read() {
    let ex = listing("exhibitions.html", "/exhibitions/", Section::Exhibitions);
    let ids: Vec<_> = ex.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "exhibitions/paloma-contreras-lomas-exhibition",
            "exhibitions/thuy-tien-nguyen"
        ]
    );
    let ev = listing("events.html", "/events/", Section::Events);
    let ids: Vec<_> = ev.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "exhibitions/gasworks-x-cotch-presents-disco-inferno",
            "events/elders-2046-for-living-and-dying-otherwise",
            "events/curators-tour-disco-inferno",
            "events/disco-inferno-neighbourhood-breakfast-exhibition-tour",
        ]
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = Gasworks::new(SITE.parse().unwrap());
    let out: Vec<_> = all()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "id": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("gasworks_normalised", out);
}

async fn mount_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_open_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .mount(server)
        .await;
}

async fn mount_page(server: &MockServer, p: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

/// A test context with no configured interval (robots.txt's Crawl-delay
/// still applies: the saved robots.txt makes the full fetch test take ~40 s).
fn ctx() -> FetchContext {
    FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap()
}

#[tokio::test]
async fn fetches_the_two_listings_only_via_fetch_context() {
    let server = MockServer::start().await;
    mount_robots(&server).await;
    mount_page(
        &server,
        "/exhibitions/",
        fixture(&format!("{DIR}/exhibitions.html")),
    )
    .await;
    mount_page(&server, "/events/", fixture(&format!("{DIR}/events.html"))).await;
    // No detail pages are ever requested.
    Mock::given(method("GET"))
        .and(path_regex("^/(exhibitions|events)/.+"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = ctx();
    let s = Gasworks::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 6);
    let site = server.uri();
    for raw in &raws {
        let expected = format!("{site}/{}/", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(expected.as_str()));
        assert!(s.normalise(raw).expect("normalise").is_some());
    }
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
        .and(path("/exhibitions/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("never fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let s = Gasworks::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx()).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn no_cards_at_all_is_an_error_but_a_quiet_season_is_not() {
    // A robots.txt without the Crawl-delay keeps this test fast.
    let empty = r#"<html><body><main><section id="archive"></section></main></body></html>"#;
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_page(&server, "/exhibitions/", empty.to_string()).await;
    let s = Gasworks::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx()).await.unwrap_err().to_string();
    assert!(err.contains("no exhibition cards"), "{err}");

    // Archive cards only: nothing current, no error.
    let archive_only = r#"<html><body><main><section id="archive"><article class="list-item"><header><h3>Exhibition</h3><h2 class="date">2 Oct – 14 Dec 25</h2><h1><a href="/exhibitions/old/">Old</a></h1></header></article></section></main></body></html>"#;
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_page(&server, "/exhibitions/", archive_only.to_string()).await;
    mount_page(
        &server,
        "/events/",
        archive_only.replace("/exhibitions/old/", "/events/old/"),
    )
    .await;
    let ctx = ctx();
    let raws = s_fetch(&server, &ctx).await;
    assert!(raws.is_empty());
    assert!(ctx.take_errors().is_empty());
}

async fn s_fetch(server: &MockServer, ctx: &FetchContext) -> Vec<RawEvent> {
    Gasworks::new(server.uri().parse().unwrap())
        .fetch(ctx)
        .await
        .expect("fetch")
}
