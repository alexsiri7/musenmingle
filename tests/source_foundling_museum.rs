//! The Foundling Museum scraper: snapshot tests over saved HTML fixtures
//! and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::foundling_museum::{
    Card, FoundlingMuseum, in_scope, needs_detail, next_page, parse_detail, parse_listing,
    raw_event,
};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/foundling-museum";
const SITE: &str = "https://foundlingmuseum.org.uk";
const PAGES: [(&str, &str); 3] = [
    ("/whats-on/", "whats-on.html"),
    ("/whats-on/page/2/", "whats-on-page-2.html"),
    ("/whats-on/page/3/", "whats-on-page-3.html"),
];

fn cards() -> Vec<Card> {
    let mut out: Vec<Card> = Vec::new();
    for (p, file) in PAGES {
        let url = Url::parse(SITE).unwrap().join(p).unwrap();
        for card in parse_listing(&fixture(&format!("{DIR}/{file}")), &url) {
            if !out.iter().any(|c| c.slug == card.slug) {
                out.push(card);
            }
        }
    }
    out
}

fn detail(slug: &str) -> String {
    fixture(&format!("{DIR}/event/{slug}.html"))
}

#[test]
fn listing_snapshot() {
    let summary: Vec<_> = cards()
        .iter()
        .map(|c| {
            serde_json::json!({
                "slug": c.slug,
                "category": c.category,
                "date_text": c.date_text,
                "in_scope": in_scope(c),
                "needs_detail": in_scope(c) && needs_detail(c),
            })
        })
        .collect();
    insta::assert_json_snapshot!("foundling_museum_listing", summary);
}

#[test]
fn normalised_output_snapshot() {
    let s = FoundlingMuseum::new(SITE.parse().unwrap());
    let out: Vec<_> = cards()
        .iter()
        .filter(|c| in_scope(c))
        .map(|c| {
            let d = needs_detail(c).then(|| parse_detail(&detail(&c.slug)).expect("a date"));
            let raw = raw_event(c, d);
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("foundling_museum_normalised", out);
}

#[test]
fn pages_chain_to_the_last() {
    let mut url = Url::parse(&format!("{SITE}/whats-on/")).unwrap();
    for (p, file) in &PAGES[..2] {
        assert_eq!(url.path(), *p);
        url = next_page(&fixture(&format!("{DIR}/{file}")), &url).expect("a next page");
    }
    assert_eq!(url.path(), "/whats-on/page/3/");
    assert_eq!(
        next_page(&fixture(&format!("{DIR}/whats-on-page-3.html")), &url),
        None
    );
}

async fn serve_site(server: &MockServer, broken: &str) {
    let local = |body: String| body.replace(SITE, &server.uri());
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(server)
        .await;
    for (p, file) in PAGES {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(local(fixture(&format!("{DIR}/{file}")))),
            )
            .mount(server)
            .await;
    }
    for c in cards().iter().filter(|c| in_scope(c) && needs_detail(c)) {
        let response = if c.slug == broken {
            ResponseTemplate::new(500)
        } else {
            ResponseTemplate::new(200).set_body_string(local(detail(&c.slug)))
        };
        Mock::given(method("GET"))
            .and(path(format!("/event/{}/", c.slug)))
            .respond_with(response)
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn fetches_pages_and_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    let broken = "sunday-drop-in-tours-18-oct";
    serve_site(&server, broken).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = FoundlingMuseum::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains(broken), "{errors:?}");
    let in_scope_count = cards().iter().filter(|c| in_scope(c)).count();
    assert_eq!(raws.len(), in_scope_count - 1);
    let s = FoundlingMuseum::new(server.uri().parse().unwrap());
    for raw in &raws {
        assert!(s.normalise(raw).expect("normalise").is_some(), "{raw:?}");
    }
}

#[tokio::test]
async fn caps_event_page_fetches() {
    let server = MockServer::start().await;
    serve_site(&server, "").await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = FoundlingMuseum::new(server.uri().parse().unwrap())
        .with_max_detail_pages(2)
        .fetch(&ctx)
        .await
        .expect("fetch");
    let exhibitions = cards()
        .iter()
        .filter(|c| in_scope(c) && !needs_detail(c))
        .count();
    assert_eq!(raws.len(), exhibitions + 2);
    // robots.txt + 3 listing pages + 2 event pages.
    assert_eq!(server.received_requests().await.unwrap().len(), 6);
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
    let err = FoundlingMuseum::new(server.uri().parse().unwrap())
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
    let err = FoundlingMuseum::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no event cards"), "{err}");
}
