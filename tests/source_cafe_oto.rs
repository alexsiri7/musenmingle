//! The Cafe OTO scraper: a snapshot test over the saved listing pages and
//! residency pages, and an end-to-end fetch against wiremock.

mod common;

use std::time::Duration;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::sources::Source;
use musenmingle::sources::cafe_oto::{
    CafeOto, MAX_LISTING_PAGES, is_residency, parse_detail, parse_listing, raw_event,
};
use reqwest::StatusCode;
use serde_json::Value;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/cafe-oto";
const SITE: &str = "https://www.cafeoto.co.uk";
const PAGES: usize = 5;
/// The residencies whose pages were saved; the others are served as 404s.
const SAVED_RESIDENCIES: [&str; 3] = [
    "upset-the-rhythm-grouper",
    "ahmed-four-day-residency-26",
    "baba-yagas-hut-dj-scotch-egg-2026",
];

fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 8).unwrap()
}

fn page_fixture(n: usize) -> String {
    fixture(&format!("{DIR}/events-page-{n}.html"))
}

fn page_url(n: usize) -> Url {
    let base = Url::parse(SITE).unwrap().join("/events/").unwrap();
    match n {
        1 => base,
        n => base.join(&format!("?page={n}")).unwrap(),
    }
}

fn all_cards() -> Vec<Value> {
    (1..=PAGES)
        .flat_map(|n| {
            let page = parse_listing(&page_fixture(n), &page_url(n));
            let next = page.next.as_ref().map(Url::as_str);
            let expected = (n < PAGES).then(|| page_url(n + 1));
            assert_eq!(next, expected.as_ref().map(Url::as_str), "page {n}");
            page.cards
        })
        .collect()
}

fn residency_slugs() -> Vec<String> {
    all_cards()
        .iter()
        .filter(|c| is_residency(c["date_text"].as_str().unwrap(), listed_on()))
        .map(|c| c["slug"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn robots_allows_the_events_pages_and_asks_for_a_slow_rate() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in [
        "/events/",
        "/events/?page=2",
        "/events/upset-the-rhythm-grouper/",
    ] {
        assert!(robots.allowed(&Url::parse(&format!("{SITE}{p}")).unwrap()));
    }
    assert_eq!(robots.crawl_delay(), Some(Duration::from_secs(10)));
    // Robots.txt, 10 listing pages and 15 residency pages, 10 s apart, plus
    // a margin.
    let s = CafeOto::new(SITE.parse().unwrap());
    assert_eq!(s.fetch_timeout(), Some(Duration::from_secs(320)));
}

#[test]
fn normalised_output_snapshot() {
    let s = CafeOto::new(SITE.parse().unwrap());
    let cards = all_cards();
    // 52 cards, less the "CLOSED FOR PRIVATE FUNCTION" one without a link.
    assert_eq!(cards.len(), 51);
    let out: Vec<_> = cards
        .iter()
        .map(|card| {
            let slug = card["slug"].as_str().unwrap();
            let detail = SAVED_RESIDENCIES
                .contains(&slug)
                .then(|| parse_detail(&fixture(&format!("{DIR}/{slug}.html"))));
            let raw = raw_event(card, listed_on(), detail);
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "date_text": card["date_text"],
                "price_text": card["price_text"],
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("cafe_oto_normalised", out);
}

fn residency_card(date_text: &str) -> Value {
    serde_json::json!({
        "slug": "a-residency",
        "url": format!("{SITE}/events/a-residency/"),
        "date_text": date_text,
        "title": "A Residency",
        "price_text": "£15",
    })
}

#[test]
fn a_residency_page_without_nights_is_an_error() {
    let s = CafeOto::new(SITE.parse().unwrap());
    let detail = parse_detail("<html><body><p>Moved</p></body></html>");
    let raw = raw_event(
        &residency_card("18–19 November 2026"),
        listed_on(),
        Some(detail),
    );
    let err = s.normalise(&raw).unwrap_err().to_string();
    assert!(err.contains("residency page had no nights"), "{err}");
}

#[test]
fn a_run_longer_than_a_year_is_skipped() {
    let s = CafeOto::new(SITE.parse().unwrap());
    let card = residency_card("18 November 2026 – 19 November 2028");
    let raw = raw_event(&card, listed_on(), None);
    assert!(s.normalise(&raw).unwrap().is_none());
    let grouper = parse_detail(&fixture(&format!("{DIR}/upset-the-rhythm-grouper.html")));
    let raw = raw_event(&card, listed_on(), Some(grouper));
    assert!(s.normalise(&raw).unwrap().is_none());
}

async fn serve_site(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        // The saved robots.txt's Crawl-delay would make this test slow:
        // `robots_allows_the_events_pages_and_asks_for_a_slow_rate` reads it.
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
    for n in 2..=PAGES {
        Mock::given(method("GET"))
            .and(path("/events/"))
            .and(query_param("page", n.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_string(page_fixture(n)))
            .with_priority(1)
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/events/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page_fixture(1)))
        .mount(server)
        .await;
    for slug in SAVED_RESIDENCIES {
        Mock::given(method("GET"))
            .and(path(format!("/events/{slug}/")))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/{slug}.html"))),
            )
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn fetches_every_listing_page_and_the_residency_pages_via_fetch_context() {
    let server = MockServer::start().await;
    serve_site(&server).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = CafeOto::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 51);

    let residencies = residency_slugs();
    let missing: Vec<&String> = residencies
        .iter()
        .filter(|s| !SAVED_RESIDENCIES.contains(&s.as_str()))
        .collect();
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), missing.len(), "{errors:?}");
    for slug in &missing {
        assert!(
            errors.iter().any(|e| e.contains(slug.as_str())),
            "{errors:?}"
        );
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1 + PAGES + residencies.len());

    let grouper = raws
        .iter()
        .find(|r| r.source_event_id == "upset-the-rhythm-grouper")
        .unwrap();
    assert_eq!(
        grouper.payload["detail"]["nights"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let ev = CafeOto::new(server.uri().parse().unwrap())
        .normalise(grouper)
        .unwrap()
        .unwrap();
    assert_eq!(ev.sessions.len(), 2);
}

#[tokio::test]
async fn caps_residency_page_fetches() {
    let server = MockServer::start().await;
    serve_site(&server).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = CafeOto::new(server.uri().parse().unwrap())
        .with_max_details(1)
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 51);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1 + PAGES + 1
    );
    let over_cap = ctx
        .take_errors()
        .iter()
        .filter(|e| e.contains("residency-page cap"))
        .count();
    assert_eq!(over_cap, residency_slugs().len() - 1);
}

#[tokio::test]
async fn caps_listing_page_fetches() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    // Page 1 links to `?page=2` and is served for every page, so the chain
    // never ends.
    Mock::given(method("GET"))
        .and(path("/events/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page_fixture(1)))
        .expect(MAX_LISTING_PAGES as u64)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex("^/events/.+/$"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = CafeOto::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(!raws.is_empty());
    let errors = ctx.take_errors();
    assert!(
        errors
            .iter()
            .any(|e| e.contains(&format!("more than {MAX_LISTING_PAGES} listing pages"))),
        "{errors:?}"
    );
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /events/\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = CafeOto::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn a_listing_without_cards_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body></body></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = CafeOto::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no event cards"), "{err}");
}
