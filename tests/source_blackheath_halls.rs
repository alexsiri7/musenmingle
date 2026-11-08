//! The Blackheath Halls scraper: a snapshot test over the saved listing
//! pages and event pages, and an end-to-end fetch against wiremock.

mod common;

use std::time::Duration;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::sources::Source;
use musenmingle::sources::blackheath_halls::{
    BlackheathHalls, MAX_DETAILS, MAX_LISTING_PAGES, needs_detail, parse_detail, parse_listing,
    raw_event,
};
use reqwest::StatusCode;
use serde_json::Value;
use url::Url;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/blackheath-halls";
const SITE: &str = "https://www.blackheathhalls.com";
const PAGES: usize = 4;
/// The event pages that were saved; the others are served as 404s.
const SAVED_DETAILS: [&str; 13] = [
    "an-evening-with-sarah-raven-adam-nicolson",
    "blackheath-christmas-fair-2",
    "burns-night-2027",
    "emma-smith",
    "green-matthews-midwinter-revels",
    "leveret",
    "lifem26-exhibition-only",
    "lifem26-festival-evensong",
    "lifem26-phantasm-elizabeth-kenny",
    "lunchtime-recital-blaze-ensemble-26",
    "music-appreciation-the-roaring-twenties-5",
    "opera-gala-night-2027",
    "turning-of-the-year-2026",
];

fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 8).unwrap()
}

fn page_fixture(n: usize) -> String {
    fixture(&format!("{DIR}/page-{n}.html"))
}

fn page_url(n: usize) -> Url {
    let base = Url::parse(SITE).unwrap().join("/whats-on/").unwrap();
    match n {
        1 => base,
        n => base.join(&format!("?pagetoshow={n}")).unwrap(),
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

fn detail_fixture(slug: &str) -> String {
    fixture(&format!("{DIR}/{slug}.html"))
}

#[test]
fn robots_allows_the_events_pages_and_asks_for_a_slow_rate() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in [
        "/whats-on/",
        "/whats-on/?pagetoshow=2",
        "/whats-on/leveret/",
    ] {
        assert!(robots.allowed(&Url::parse(&format!("{SITE}{p}")).unwrap()));
    }
    assert_eq!(robots.crawl_delay(), Some(Duration::from_secs(10)));
    // Robots.txt, 8 listing pages and 60 event pages, 10 s apart, plus a
    // margin.
    let s = BlackheathHalls::new(SITE.parse().unwrap());
    assert_eq!(s.fetch_timeout(), Some(Duration::from_secs(750)));
}

#[test]
fn normalised_output_snapshot() {
    let s = BlackheathHalls::new(SITE.parse().unwrap());
    let cards = all_cards();
    assert_eq!(cards.len(), 80);
    let out: Vec<_> = cards
        .iter()
        .map(|card| {
            let slug = card["slug"].as_str().unwrap();
            let detail = SAVED_DETAILS
                .contains(&slug)
                .then(|| parse_detail(&detail_fixture(slug)));
            let raw = raw_event(card, listed_on(), detail);
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "types": card["types"],
                "date_text": card["date_text"],
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("blackheath_halls_normalised", out);
}

#[test]
fn the_event_page_gives_the_title_when_the_card_has_the_date() {
    let s = BlackheathHalls::new(SITE.parse().unwrap());
    let card = all_cards()
        .into_iter()
        .find(|c| c["slug"] == "turning-of-the-year-2026")
        .unwrap();
    assert_eq!(card["title"], "Mon 23 Nov, 6.30pm SOLD OUT");
    let detail = parse_detail(&detail_fixture("turning-of-the-year-2026"));
    let ev = s
        .normalise(&raw_event(&card, listed_on(), Some(detail)))
        .unwrap()
        .unwrap();
    assert_eq!(ev.title, "Turning of The Year 2026");
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
            .and(path("/whats-on/"))
            .and(query_param("pagetoshow", n.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                // The saved pages link to the real site; point them here.
                page_fixture(n).replace(SITE, &server.uri()),
            ))
            .with_priority(1)
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(page_fixture(1).replace(SITE, &server.uri())),
        )
        .mount(server)
        .await;
    for slug in SAVED_DETAILS {
        Mock::given(method("GET"))
            .and(path(format!("/whats-on/{slug}/")))
            .respond_with(ResponseTemplate::new(200).set_body_string(detail_fixture(slug)))
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn fetches_every_listing_page_and_the_in_scope_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    serve_site(&server).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = BlackheathHalls::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 80);

    let in_scope: Vec<String> = all_cards()
        .iter()
        .filter(|c| needs_detail(c))
        .map(|c| c["slug"].as_str().unwrap().to_string())
        .collect();
    let missing: Vec<&String> = in_scope
        .iter()
        .filter(|s| !SAVED_DETAILS.contains(&s.as_str()))
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
    assert_eq!(requests.len(), 1 + PAGES + in_scope.len());

    let phantasm = raws
        .iter()
        .find(|r| r.source_event_id == "lifem26-phantasm-elizabeth-kenny")
        .unwrap();
    let ev = BlackheathHalls::new(server.uri().parse().unwrap())
        .normalise(phantasm)
        .unwrap()
        .unwrap();
    assert_eq!(
        ev.venue_name.as_deref(),
        Some("St Michael & All Angels Church")
    );
    assert!(ev.description.is_some());
}

#[tokio::test]
async fn caps_event_page_fetches() {
    let cards: String = (1..=MAX_DETAILS + 1)
        .map(|i| {
            format!(
                r#"<div class="event-card"><span class="event-card-types"><span>Music</span></span><h3>Concert {i}</h3><span class="event-card-date">Mon 7 Dec, 7pm</span><a href="/whats-on/concert-{i}/" class="button-more-details">More Details</a></div>"#
            )
        })
        .collect();
    let listing = format!(r#"<html><body><div class="events-grid">{cards}</div></body></html>"#);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(listing))
        .mount(&server)
        .await;
    let over = format!("/whats-on/concert-{}/", MAX_DETAILS + 1);
    Mock::given(method("GET"))
        .and(path(over.as_str()))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/whats-on/.+/$"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .expect(MAX_DETAILS as u64)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = BlackheathHalls::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), MAX_DETAILS + 1);
    assert_eq!(
        ctx.take_errors(),
        vec![format!(
            "concert-{}: over the {MAX_DETAILS} event-page cap",
            MAX_DETAILS + 1
        )]
    );
}

#[tokio::test]
async fn caps_listing_page_fetches() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    // Page 1 links to `?pagetoshow=2` and is served for every page, so the
    // chain never ends.
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(page_fixture(1).replace(SITE, &server.uri())),
        )
        .expect(MAX_LISTING_PAGES as u64)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/whats-on/.+/$"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = BlackheathHalls::new(server.uri().parse().unwrap())
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
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /whats-on/\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = BlackheathHalls::new(server.uri().parse().unwrap())
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
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body></body></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = BlackheathHalls::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no event cards"), "{err}");
}
