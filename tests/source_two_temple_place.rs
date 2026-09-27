//! The Two Temple Place scraper: snapshot tests over the saved What's on
//! page and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::two_temple_place::{TwoTemplePlace, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/two-temple-place";
const SITE: &str = "https://twotempleplace.org";

fn listing() -> String {
    fixture(&format!("{DIR}/whats-on.html"))
}

#[test]
fn normalised_output_snapshot() {
    let s = TwoTemplePlace::new(SITE.parse().unwrap());
    let page = Url::parse(&format!("{SITE}/whats-on/")).unwrap();
    let out: Vec<_> = parse_listing(&listing(), &page)
        .into_iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "date_lines": raw.payload["date_lines"],
                "price": raw.payload["price"],
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("two_temple_place_normalised", out);
}

#[test]
fn only_upcoming_cards_are_read() {
    let page = Url::parse(&format!("{SITE}/whats-on/")).unwrap();
    let items = parse_listing(&listing(), &page);
    // The page links ~540 past events below the upcoming ones.
    assert_eq!(items.len(), 11);
}

#[tokio::test]
async fn fetches_the_listing_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        // The page links events absolutely, on its own host.
        .respond_with(
            ResponseTemplate::new(200).set_body_string(listing().replace(SITE, &server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = TwoTemplePlace::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 11);
    let rest = raws
        .iter()
        .find(|r| r.source_event_id == "gathering-rest")
        .expect("the exhibition");
    assert_eq!(
        rest.source_url.as_deref(),
        Some(format!("{}/events/gathering-rest/", server.uri()).as_str())
    );
}

#[tokio::test]
async fn an_empty_upcoming_section_is_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<section class="related-events-block filters upcoming"></section>"#,
        ))
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = TwoTemplePlace::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(raws.is_empty());
}

#[tokio::test]
async fn a_page_without_the_upcoming_section_is_an_error() {
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
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = TwoTemplePlace::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no upcoming events section"), "{err}");
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
    let err = TwoTemplePlace::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
