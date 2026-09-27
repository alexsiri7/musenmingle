//! Old Royal Naval College scraper: snapshot tests over saved HTML fixtures
//! and an end-to-end fetch against wiremock.
//!
//! Both listing pages are saved in full, plus the detail page of every dated
//! card (the only detail pages the scraper fetches).

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::Category;
use musenmingle::sources::Source;
use musenmingle::sources::old_royal_naval_college::{
    Card, MAX_DETAIL_PAGES, MAX_LISTING_PAGES, OldRoyalNavalCollege, card_event, is_dated,
    parse_detail, parse_listing,
};
use rust_decimal::Decimal;
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/old-royal-naval-college";
const SITE: &str = "https://ornc.org";

/// Listing fixtures in crawl order: (path, file stem).
const LISTINGS: &[(&str, &str)] = &[
    ("/whats-on/", "whats-on"),
    ("/whats-on/page/2/", "whats-on-page-2"),
];

/// The day the fixtures were fetched: year inference is pinned to it.
fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()
}

/// Slugs of every saved detail page, sorted.
fn detail_slugs() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(DIR)
        .join("detail");
    let mut slugs: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            e.unwrap()
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    slugs.sort();
    slugs
}

fn slug(card: &Card) -> &str {
    card.path.trim_end_matches('/').rsplit('/').next().unwrap()
}

fn detail_html(slug: &str) -> String {
    fixture(&format!("{DIR}/detail/{slug}.html"))
}

fn listing_html(stem: &str) -> String {
    fixture(&format!("{DIR}/{stem}.html"))
}

/// The cards of every listing page, de-duplicated by path in crawl order.
fn cards() -> Vec<Card> {
    let mut cards: Vec<Card> = Vec::new();
    for (_, stem) in LISTINGS {
        let listing = parse_listing(&listing_html(stem));
        assert!(listing.problems.is_empty(), "{:?}", listing.problems);
        for card in listing.cards {
            if !cards.iter().any(|c| c.path == card.path) {
                cards.push(card);
            }
        }
    }
    cards
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

#[test]
fn listing_snapshot() {
    let pages: Vec<_> = LISTINGS
        .iter()
        .map(|(_, stem)| {
            let listing = parse_listing(&listing_html(stem));
            serde_json::json!({ "fixture": stem, "has_more": listing.has_more })
        })
        .collect();
    let cards = cards();
    assert_eq!(cards.len(), 26);
    insta::assert_json_snapshot!(
        "old_royal_naval_college_listing",
        serde_json::json!({ "pages": pages, "cards": cards })
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = OldRoyalNavalCollege::new(SITE.parse().unwrap());
    let site: Url = SITE.parse().unwrap();
    let details = detail_slugs();
    let mut dated: Vec<String> = Vec::new();
    let out: Vec<serde_json::Value> = cards()
        .iter()
        .map(|card| {
            if is_dated(card, listed_on()).unwrap() {
                dated.push(slug(card).to_string());
            }
            let detail = details
                .iter()
                .any(|d| d == slug(card))
                .then(|| parse_detail(&detail_html(slug(card))));
            let raw = card_event(card, &site, listed_on(), detail.as_ref());
            serde_json::json!({
                "id": raw.source_event_id,
                "payload": raw.payload,
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    dated.sort();
    assert_eq!(dated, details, "detail fixtures are the dated cards");
    insta::assert_json_snapshot!("old_royal_naval_college_normalised", out);
}

#[tokio::test]
async fn fetches_listing_pages_and_dated_details_via_fetch_context() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    for (listing_path, stem) in LISTINGS {
        Mock::given(method("GET"))
            .and(path(*listing_path))
            .respond_with(ResponseTemplate::new(200).set_body_string(listing_html(stem)))
            .expect(1)
            .mount(&server)
            .await;
    }
    // Page 2 has no "Load more": page 3 is never requested.
    Mock::given(method("GET"))
        .and(path("/whats-on/page/3/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let cards = cards();
    for card in &cards {
        if detail_slugs().iter().any(|d| d == slug(card)) {
            Mock::given(method("GET"))
                .and(path(card.path.as_str()))
                .respond_with(ResponseTemplate::new(200).set_body_string(detail_html(slug(card))))
                .expect(1)
                .mount(&server)
                .await;
        }
    }
    Mock::given(method("GET"))
        .and(path_regex("^/whats-on/[a-z0-9-]+/$"))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OldRoyalNavalCollege::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    let errors = ctx.take_errors();
    assert!(errors.is_empty(), "{errors:?}");
    let site = server.uri();
    let got: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    let expected: Vec<&str> = cards.iter().map(|c| c.path.trim_matches('/')).collect();
    assert_eq!(got, expected);
    for raw in &raws {
        let url = format!("{site}/{}/", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(url.as_str()));
        assert!(raw.payload["listed_on"].is_string());
    }
    let talk = raws
        .iter()
        .find(|r| r.source_event_id == "whats-on/art-of-directing-adjani-salmon")
        .unwrap();
    let event = s.normalise(talk).unwrap().unwrap();
    assert_eq!(event.category, Category::Talk);
    assert_eq!(event.venue_name.as_deref(), Some("Old Royal Naval College"));
    assert_eq!(event.price.min, Some(Decimal::new(20, 0)));
    assert_eq!(event.price.max, Some(Decimal::new(25, 0)));
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
    let s = OldRoyalNavalCollege::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A template change must reach the health checker instead of producing
    // clean, empty runs.
    let server = MockServer::start().await;
    serve_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body></body></html>"))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OldRoyalNavalCollege::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event cards"), "{err}");
}

fn talk_card(slug: &str, title: &str, when: &str) -> String {
    format!(
        r#"<div class="news__item blue"><div class="news__details-wrap"><div class="news__details text-block blue">
        <span class="heading--sm">Film and TV</span><h3 class="text-listing">{title}</h3>
        <strong>{when}</strong><p></p>
        <a href="https://ornc.org/whats-on/{slug}/">Find out more</a></div></div></div>"#
    )
}

fn listing_page(cards: &str, has_more: bool) -> String {
    let more = if has_more {
        r#"<button class="news__btn heading--sm" id="loadmore_events">LOAD MORE</button>"#
    } else {
        ""
    };
    format!(r#"<html><body><section class="news container">{cards}{more}</section></body></html>"#)
}

#[tokio::test]
async fn listing_and_detail_fetches_stop_at_their_caps() {
    // Every listing page offers "Load more" and holds six dated talks; the
    // page past the cap and details past theirs are never requested.
    const TALKS_PER_PAGE: usize = 6;
    const { assert!(MAX_LISTING_PAGES * TALKS_PER_PAGE > MAX_DETAIL_PAGES) };
    let server = MockServer::start().await;
    serve_robots(&server).await;
    for page in 1..=MAX_LISTING_PAGES + 1 {
        let cards: String = (0..TALKS_PER_PAGE)
            .map(|i| talk_card(&format!("talk-{page}-{i}"), "A Talk", "Tue 6 Oct | 7pm"))
            .collect();
        let listing_path = if page == 1 {
            "/whats-on/".to_string()
        } else {
            format!("/whats-on/page/{page}/")
        };
        Mock::given(method("GET"))
            .and(path(listing_path))
            .respond_with(ResponseTemplate::new(200).set_body_string(listing_page(&cards, true)))
            .expect(u64::from(page <= MAX_LISTING_PAGES))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path_regex("^/whats-on/talk-"))
        .respond_with(ResponseTemplate::new(404))
        .expect(MAX_DETAIL_PAGES as u64)
        .mount(&server)
        .await;

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OldRoyalNavalCollege::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    // Every fetched detail page was a 404: one soft error each.
    assert!(raws.is_empty());
    assert_eq!(ctx.take_errors().len(), MAX_DETAIL_PAGES);
}

#[tokio::test]
async fn bad_cards_are_reported_and_bad_dates_kept_for_normalise() {
    // An off-site card is reported; a card with an unreadable time is kept
    // without its detail page, so that normalise reports it as a run error
    // rather than it vanishing. Undated programmes need no detail page.
    let server = MockServer::start().await;
    serve_robots(&server).await;
    let cards = [
        talk_card("a-talk", "A Talk", "Tue 6 Oct | 7pm"),
        talk_card("bad-time", "A Talk", "Tue 6 Oct | at dusk"),
        talk_card("daily", "A Talk", "Daily | 10am–5pm"),
        talk_card("x", "Off Site", "Tue 6 Oct | 7pm").replace("ornc.org", "elsewhere.example"),
    ]
    .concat();
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(listing_page(&cards, false)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/a-talk/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/whats-on/(bad-time|daily)/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = OldRoyalNavalCollege::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(
        ids,
        ["whats-on/a-talk", "whats-on/bad-time", "whats-on/daily"]
    );
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("Off Site"), "{errors:?}");
    assert!(s.normalise(&raws[0]).unwrap().is_some());
    assert!(s.normalise(&raws[1]).is_err());
    assert!(s.normalise(&raws[2]).unwrap().is_none());
}
