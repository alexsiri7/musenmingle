//! Headstone Manor & Museum scraper: snapshot tests over saved HTML fixtures
//! and an end-to-end fetch against wiremock.
//!
//! The three listing pages are saved in full, plus the detail page of every
//! in-scope card (the only detail pages the scraper fetches).

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::Category;
use musenmingle::sources::Source;
use musenmingle::sources::headstone_manor::{
    Card, HeadstoneManor, MAX_DETAIL_PAGES, MAX_LISTING_PAGES, card_event, category, parse_detail,
    parse_listing,
};
use rust_decimal::Decimal;
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/headstone-manor";
const SITE: &str = "https://headstonemanor.org";

/// Listing fixtures in crawl order: (path, file stem).
const LISTINGS: &[(&str, &str)] = &[
    ("/whats-on/", "whats-on"),
    ("/whats-on/page-2/", "whats-on-page-2"),
    ("/whats-on/page-3/", "whats-on-page-3"),
];

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
    let mut unique = slugs.clone();
    unique.dedup();
    assert_eq!(unique, slugs, "detail fixture stems must be unique");
    slugs
}

fn slug(card: &Card) -> &str {
    card.path.rsplit('/').next().unwrap()
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
            serde_json::json!({ "fixture": stem, "next_path": listing.next_path })
        })
        .collect();
    insta::assert_json_snapshot!(
        "headstone_manor_listing",
        serde_json::json!({ "pages": pages, "cards": cards() })
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = HeadstoneManor::new(SITE.parse().unwrap());
    let site: Url = SITE.parse().unwrap();
    let details = detail_slugs();
    let mut in_scope: Vec<String> = Vec::new();
    let out: Vec<serde_json::Value> = cards()
        .iter()
        .map(|card| {
            if category(card).unwrap().is_some() {
                in_scope.push(slug(card).to_string());
            }
            let detail = details
                .iter()
                .any(|d| d == slug(card))
                .then(|| parse_detail(&detail_html(slug(card))));
            let raw = card_event(card, &site, detail.as_ref());
            serde_json::json!({
                "id": raw.source_event_id,
                "payload": raw.payload,
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    in_scope.sort();
    assert_eq!(in_scope, details, "detail fixtures are the in-scope cards");
    insta::assert_json_snapshot!("headstone_manor_normalised", out);
}

#[tokio::test]
async fn fetches_listing_pages_and_in_scope_details_via_fetch_context() {
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
    let cards = cards();
    for card in &cards {
        if detail_slugs().iter().any(|d| d == slug(card)) {
            Mock::given(method("GET"))
                .and(path(format!("{}/", card.path)))
                .respond_with(ResponseTemplate::new(200).set_body_string(detail_html(slug(card))))
                .expect(1)
                .mount(&server)
                .await;
        }
    }
    Mock::given(method("GET"))
        .and(path_regex("^/(events|exhibitions)/"))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = HeadstoneManor::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    let errors = ctx.take_errors();
    assert!(errors.is_empty(), "{errors:?}");
    let site = server.uri();
    let got: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    let expected: Vec<&str> = cards.iter().map(|c| c.path.trim_matches('/')).collect();
    assert_eq!(got, expected);
    for raw in &raws {
        let url = format!("{site}/{}", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(url.as_str()));
    }
    let talk = raws
        .iter()
        .find(|r| r.source_event_id == "events/hmm-tuesday-talk-the-story-of-the-music-hall")
        .unwrap();
    let event = s.normalise(talk).unwrap().unwrap();
    assert_eq!(event.category, Category::Talk);
    assert_eq!(
        event.venue_name.as_deref(),
        Some("Headstone Manor & Museum")
    );
    assert_eq!(event.price.min, Some(Decimal::new(450, 2)));
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
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = HeadstoneManor::new(server.uri().parse().unwrap());
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
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = HeadstoneManor::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event cards"), "{err}");
}

fn talk_card(slug: &str, title: &str) -> String {
    format!(
        r#"<a href="https://headstonemanor.org/events/{slug}" class="c-media c-media--link c-media--event">
        <div class="c-media__text"><h3 class="c-media__title">{title}</h3>
        <span class="c-media__posttitle "><time datetime="2026-10-06T14:00:00+01:00" itemprop="startDate">Tue 6 Oct</time></span>
        <p class="c-media__category">Events for Adults</p></div></a>"#
    )
}

#[tokio::test]
async fn listing_and_detail_fetches_stop_at_their_caps() {
    // Every listing page links to a further one and holds eight single-day
    // talks; the page past the cap and details past theirs are never
    // requested.
    const TALKS_PER_PAGE: usize = 8;
    const { assert!(MAX_LISTING_PAGES * TALKS_PER_PAGE > MAX_DETAIL_PAGES) };
    let server = MockServer::start().await;
    serve_robots(&server).await;
    for page in 1..=MAX_LISTING_PAGES + 1 {
        let cards: String = (0..TALKS_PER_PAGE)
            .map(|i| talk_card(&format!("talk-{page}-{i}"), "A Talk"))
            .collect();
        let body = format!(
            r#"<html><body>{cards}<a href="https://headstonemanor.org/whats-on/page-{}" class="c-pagination__next">Next</a></body></html>"#,
            page + 1
        );
        let listing_path = if page == 1 {
            "/whats-on/".to_string()
        } else {
            format!("/whats-on/page-{page}/")
        };
        Mock::given(method("GET"))
            .and(path(listing_path))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(u64::from(page <= MAX_LISTING_PAGES))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path_regex("^/events/talk-"))
        .respond_with(ResponseTemplate::new(404))
        .expect(MAX_DETAIL_PAGES as u64)
        .mount(&server)
        .await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = HeadstoneManor::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    // Every fetched detail page was a 404: one soft error each.
    assert!(raws.is_empty());
    assert_eq!(ctx.take_errors().len(), MAX_DETAIL_PAGES);
}

#[tokio::test]
async fn bad_cards_are_reported() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    let listing = format!(
        r#"<html><body>{}
        <a href="https://elsewhere.example/events/off-site" class="c-media--event"><h3 class="c-media__title">Off Site</h3></a>
        </body></html>"#,
        talk_card("a-talk", "A Talk")
    );
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(listing))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events/a-talk/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = HeadstoneManor::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, ["events/a-talk"]);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("Off Site"), "{errors:?}");
}
