//! October Gallery scraper: snapshot tests over the saved Exhibitions,
//! exhibition and Events pages, and end-to-end fetches against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::october_gallery::{
    OctoberGallery, parse_detail, parse_events, parse_listing,
};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/october-gallery";
const SITE: &str = "https://www.octobergallery.co.uk";
const DETAILS: [&str; 2] = [
    "zana-masombuka-spotlight-benji-reid",
    "romuald-hazoume-my-blue-period",
];

fn url(path: &str) -> Url {
    format!("{SITE}{path}").parse().unwrap()
}

fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
}

fn raws() -> Vec<RawEvent> {
    let mut raws: Vec<RawEvent> = DETAILS
        .iter()
        .map(|slug| {
            let html = fixture(&format!("{DIR}/detail/{slug}.html"));
            parse_detail(&html, &url(&format!("/exhibitions/{slug}"))).expect("ExhibitionEvent")
        })
        .collect();
    raws.extend(
        parse_events(
            &fixture(&format!("{DIR}/events.html")),
            &url("/events/"),
            listed_on(),
        )
        .expect("event cards"),
    );
    raws
}

#[test]
fn listing_has_only_current_and_forthcoming_exhibitions() {
    let paths = parse_listing(
        &fixture(&format!("{DIR}/exhibitions.html")),
        &url("/exhibitions/"),
    );
    let want: Vec<String> = DETAILS
        .iter()
        .map(|s| format!("/exhibitions/{s}"))
        .collect();
    assert_eq!(paths, want);
}

#[test]
fn events_page_yields_only_forthcoming_events() {
    let events = parse_events(
        &fixture(&format!("{DIR}/events.html")),
        &url("/events/"),
        listed_on(),
    )
    .expect("event cards");
    let titles: Vec<_> = events
        .iter()
        .map(|r| r.payload["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["Gallery Talk: Romuald Hazoumè in Conversation with Gerard Houghton"]
    );
}

#[test]
fn raw_snapshot() {
    insta::assert_json_snapshot!("october_gallery_raw", raws());
}

#[test]
fn normalised_output_snapshot() {
    let s = OctoberGallery::new(SITE.parse().unwrap());
    let out: Vec<_> = raws()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "id": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("october_gallery_normalised", out);
}

async fn mount(server: &MockServer, at: &str, body: String, times: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(times)
        .mount(server)
        .await;
}

async fn mount_site(server: &MockServer, detail_fetches: u64) {
    mount_site_with_events(
        server,
        detail_fetches,
        fixture(&format!("{DIR}/events.html")),
    )
    .await;
}

async fn mount_site_with_events(server: &MockServer, detail_fetches: u64, events: String) {
    mount(
        server,
        "/robots.txt",
        fixture(&format!("{DIR}/robots.txt")),
        1,
    )
    .await;
    mount(
        server,
        "/exhibitions/",
        fixture(&format!("{DIR}/exhibitions.html")),
        1,
    )
    .await;
    mount(server, "/events/", events, 1).await;
    for (i, slug) in DETAILS.iter().enumerate() {
        mount(
            server,
            &format!("/exhibitions/{slug}"),
            fixture(&format!("{DIR}/detail/{slug}.html")),
            u64::from((i as u64) < detail_fetches),
        )
        .await;
    }
}

#[tokio::test]
async fn fetches_exhibitions_and_events_via_fetch_context() {
    let server = MockServer::start().await;
    mount_site(&server, 2).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OctoberGallery::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    let want: Vec<String> = self::raws()
        .into_iter()
        .map(|r| r.source_event_id)
        .collect();
    assert_eq!(ids, want);
    let site = server.uri();
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(format!("{site}/exhibitions/{}", DETAILS[0]).as_str())
    );
    assert_eq!(
        raws[2].source_url.as_deref(),
        Some(format!("{site}/events/").as_str())
    );
    for raw in &raws {
        s.normalise(raw).expect("normalise");
    }
}

#[tokio::test]
async fn detail_fetches_are_capped() {
    let server = MockServer::start().await;
    mount_site(&server, 1).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OctoberGallery::new(server.uri().parse().unwrap()).with_max_detail_pages(1);
    let raws = s.fetch(&ctx).await.expect("fetch");
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids[0], DETAILS[0]);
    assert!(!ids.contains(&DETAILS[1]));
}

#[tokio::test]
async fn exhibition_page_without_jsonld_is_reported() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/robots.txt",
        fixture(&format!("{DIR}/robots.txt")),
        1,
    )
    .await;
    mount(
        &server,
        "/exhibitions/",
        r#"<div id="inner-content"><div class="exhib-title"><h2><a href="/exhibitions/show">Show</a></h2></div></div>"#.into(),
        1,
    )
    .await;
    mount(&server, "/exhibitions/show", "<html></html>".into(), 1).await;
    mount(&server, "/events/", "<html></html>".into(), 1).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OctoberGallery::new(server.uri().parse().unwrap());
    assert!(s.fetch(&ctx).await.expect("fetch").is_empty());
    assert_eq!(
        ctx.take_errors(),
        [
            "/exhibitions/show: no ExhibitionEvent JSON-LD",
            "/events/: no events found (forthcoming or past)",
        ]
    );
}

#[tokio::test]
async fn events_page_with_only_past_events_is_clean() {
    // No talk scheduled is normal; only a page without any event card
    // (forthcoming or past) is a template change worth an error.
    let past_only = fixture(&format!("{DIR}/events.html")).replacen(
        "Gallery Talk: Romuald Hazoumè in Conversation with Gerard Houghton",
        "",
        1,
    );
    let server = MockServer::start().await;
    mount_site_with_events(&server, 2, past_only).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OctoberGallery::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert!(raws.iter().all(|r| r.payload["kind"] == "exhibition"));
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/robots.txt",
        "User-agent: MuseNMingleBot\nDisallow: /\n".into(),
        1,
    )
    .await;
    mount(
        &server,
        "/exhibitions/",
        "should never be fetched".into(),
        0,
    )
    .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OctoberGallery::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A page without exhibition links (a template change) must reach the
    // health checker instead of producing clean, empty runs.
    let server = MockServer::start().await;
    mount(
        &server,
        "/robots.txt",
        fixture(&format!("{DIR}/robots.txt")),
        1,
    )
    .await;
    mount(
        &server,
        "/exhibitions/",
        r#"<div id="inner-content"><h1>CURRENT EXHIBITION</h1></div>"#.into(),
        1,
    )
    .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OctoberGallery::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(
        err.contains("no current or forthcoming exhibitions"),
        "{err}"
    );
}
