//! The Wilton's Music Hall scraper: snapshot tests over saved HTML fixtures
//! and an end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::sources::Source;
use musenmingle::sources::wiltons_music_hall::{
    MAX_PAGES, WiltonsMusicHall, parse_detail, parse_listing, raw_event,
};
use reqwest::StatusCode;
use serde_json::Value;
use url::Url;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/wiltons-music-hall";
const SITE: &str = "https://wiltons.org.uk";

/// The Music view's cards on 2026-11-08, then the Opera view's.
const MUSIC: &[&str] = &[
    "songs-from-the-sea-2",
    "city-of-london-sinfonia-these-memories-may-be-true",
    "the-swingle-singers-close-mic-classics",
    "jazz-noel",
    "the-dead-2",
    "brian-clemens-big-band-ft-francesca-confortini-the-20th-century-jukebox",
    "the-tiger-lillies-in-a-macbeth-song",
    "onetrackminds-mar27",
    "steve-pretty-and-friends-on-the-origin-of-the-pieces-3",
];
const OPERA: &[&str] = &[
    "the-marriage-of-figaro",
    "the-arcadians-jan-26",
    "the-mikado-2",
];

fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 8).unwrap()
}

fn page(slug: &str) -> String {
    fixture(&format!("{DIR}/{slug}.html"))
}

fn view(genre: &str) -> String {
    fixture(&format!("{DIR}/{genre}.html"))
}

fn cards() -> Vec<Value> {
    let mut out = Vec::new();
    for (genre, label) in [("music", "Music"), ("opera", "Opera")] {
        let url = Url::parse(&format!("{SITE}/whats-on/?category={genre}")).unwrap();
        let listing = parse_listing(&view(genre), &url, label, 1).expect("listing");
        assert!(!listing.has_next, "{genre}");
        out.extend(listing.cards);
    }
    out
}

fn slugs(cards: &[Value]) -> Vec<String> {
    cards
        .iter()
        .map(|c| c["slug"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn saved_robots_allow_the_events_pages() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in ["/whats-on/?category=music", "/whats-on/jazz-noel/"] {
        assert!(
            robots.allowed(&Url::parse(SITE).unwrap().join(p).unwrap()),
            "{p}"
        );
    }
}

#[test]
fn the_genre_views_give_their_cards() {
    let cards = cards();
    assert_eq!(slugs(&cards), [MUSIC, OPERA].concat());
    let first = &cards[0];
    assert_eq!(
        first["url"],
        "https://wiltons.org.uk/whats-on/songs-from-the-sea-2/"
    );
    assert_eq!(first["title"], "Songs from the Sea");
    assert_eq!(first["genre"], "Music");
    assert!(
        first["image_url"]
            .as_str()
            .unwrap()
            .starts_with("https://wiltons.org.uk/wp-content/uploads/")
    );
    assert_eq!(cards[MUSIC.len()]["genre"], "Opera");
}

#[test]
fn normalised_output_snapshot() {
    let s = WiltonsMusicHall::new(SITE.parse().unwrap());
    let out: Vec<_> = cards()
        .iter()
        .map(|card| {
            let slug = card["slug"].as_str().unwrap();
            let detail = parse_detail(&page(slug)).expect("event page");
            let raw = raw_event(card, listed_on(), detail);
            match s.normalise(&raw) {
                Ok(event) => serde_json::json!({
                    "source_event_id": raw.source_event_id,
                    "performances": raw.payload["detail"]["performances"],
                    "event": event,
                }),
                Err(e) => serde_json::json!({
                    "source_event_id": raw.source_event_id,
                    "error": e.to_string(),
                }),
            }
        })
        .collect();
    insta::assert_json_snapshot!("wiltons_music_hall_normalised", out);
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

async fn serve_view(server: &MockServer, genre: &str, body: String) {
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .and(query_param("category", genre))
        .and(query_param_is_missing("event-page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

async fn serve_page(server: &MockServer, slug: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(format!("/whats-on/{slug}/")))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_both_genre_views_and_their_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_view(&server, "music", view("music")).await;
    serve_view(&server, "opera", view("opera")).await;
    for slug in MUSIC.iter().chain(OPERA) {
        serve_page(&server, slug, page(slug)).await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = WiltonsMusicHall::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, [MUSIC, OPERA].concat());
    // The saved cards link the live site; their pages are read from the
    // listing's own origin.
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(format!("{}/whats-on/{}/", server.uri(), MUSIC[0]).as_str())
    );
    // robots.txt, two genre views and every card's page.
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        3 + MUSIC.len() + OPERA.len()
    );
}

/// A genre view page holding only `slugs`' cards (cut from the saved Music
/// and Opera views), with a pager linking page `next` when given.
fn view_with(slugs: &[&str], next: Option<u32>) -> String {
    let cards: String = ["music", "opera"]
        .map(|genre| {
            let saved = view(genre);
            let start = saved.find("<li class=\"WhatsonItem").unwrap();
            let end = saved.rfind("</li>").unwrap() + "</li>".len();
            saved[start..end].to_string()
        })
        .concat()
        .split("<li class=\"WhatsonItem")
        .filter(|c| slugs.iter().any(|s| c.contains(&format!("/whats-on/{s}/"))))
        .map(|c| format!("<li class=\"WhatsonItem{c}"))
        .collect();
    let pager = next.map_or(String::new(), |n| {
        format!(
            r#"<div class="whatson-event-listing-pager"><div class="whatson-event-listing-page-link"><a href="/whats-on/?event-page={n}&category=">{n}</a></div></div>"#
        )
    });
    format!(
        r#"<html><body><ul class="whatsoncategory"></ul><ul class="WhatsOnList">{cards}</ul>{pager}</body></html>"#
    )
}

const EMPTY_VIEW: &str = r#"<html><body><ul class="whatsoncategory"></ul></body></html>"#;

#[tokio::test]
async fn follows_the_pager_and_caps_event_pages() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_view(&server, "music", view_with(&MUSIC[..1], Some(2))).await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .and(query_param("category", "music"))
        .and(query_param("event-page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(view_with(&MUSIC[1..3], None)))
        .expect(1)
        .mount(&server)
        .await;
    serve_view(&server, "opera", EMPTY_VIEW.into()).await;
    serve_page(&server, MUSIC[0], page(MUSIC[0])).await;
    serve_page(&server, MUSIC[1], "<html></html>".into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = WiltonsMusicHall::new(server.uri().parse().unwrap()).with_max_details(2);
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert_eq!(raws.len(), 1);
    assert_eq!(raws[0].source_event_id, MUSIC[0]);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].contains("not an event page"), "{errors:?}");
    assert!(
        errors[1].contains("over the 2 event-page cap"),
        "{errors:?}"
    );
}

#[tokio::test]
async fn an_event_in_both_genre_views_is_fetched_once_under_the_first() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_view(&server, "music", view_with(&MUSIC[..1], None)).await;
    serve_view(&server, "opera", view_with(&[MUSIC[0], OPERA[0]], None)).await;
    serve_page(&server, MUSIC[0], page(MUSIC[0])).await;
    serve_page(&server, OPERA[0], page(OPERA[0])).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = WiltonsMusicHall::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, [MUSIC[0], OPERA[0]]);
    assert_eq!(raws[0].payload["card"]["genre"], "Music");
    assert_eq!(raws[1].payload["card"]["genre"], "Opera");
}

#[tokio::test]
async fn caps_listing_pages_a_genre() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_view(&server, "music", view_with(&[], Some(2))).await;
    for n in 2..=MAX_PAGES {
        Mock::given(method("GET"))
            .and(path("/whats-on/"))
            .and(query_param("category", "music"))
            .and(query_param("event-page", n.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_string(view_with(&[], Some(n + 1))))
            .expect(1)
            .mount(&server)
            .await;
    }
    serve_view(&server, "opera", EMPTY_VIEW.into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = WiltonsMusicHall::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(raws.is_empty());
    assert_eq!(
        ctx.take_errors(),
        [format!("music: over the {MAX_PAGES}-page cap")]
    );
}

#[tokio::test]
async fn a_later_listing_page_failing_keeps_the_cards_found_so_far() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_view(&server, "music", view_with(&MUSIC[..1], None)).await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .and(query_param("category", "opera"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    serve_page(&server, MUSIC[0], page(MUSIC[0])).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = WiltonsMusicHall::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 1);
    assert_eq!(raws[0].source_event_id, MUSIC[0]);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("opera listing page 1:"), "{errors:?}");
}

#[tokio::test]
async fn a_failing_second_page_of_the_first_genre_keeps_its_first_page() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_view(&server, "music", view_with(&MUSIC[..1], Some(2))).await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .and(query_param("category", "music"))
        .and(query_param("event-page", "2"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    serve_view(&server, "opera", view_with(&OPERA[..1], None)).await;
    serve_page(&server, MUSIC[0], page(MUSIC[0])).await;
    serve_page(&server, OPERA[0], page(OPERA[0])).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = WiltonsMusicHall::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, [MUSIC[0], OPERA[0]]);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("music listing page 2:"), "{errors:?}");
}

#[tokio::test]
async fn empty_genre_views_are_not_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_view(&server, "music", EMPTY_VIEW.into()).await;
    serve_view(&server, "opera", EMPTY_VIEW.into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = WiltonsMusicHall::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(raws.is_empty());
    assert!(ctx.take_errors().is_empty());
}

#[tokio::test]
async fn a_page_that_is_not_the_listing_is_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_view(&server, "music", "<html><body></body></html>".into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = WiltonsMusicHall::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no What's On listing"), "{err}");
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
    let err = WiltonsMusicHall::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
