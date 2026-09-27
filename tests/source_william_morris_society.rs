//! William Morris Society scraper: snapshot tests over the saved listing
//! and an end-to-end fetch against wiremock. The scraper never fetches
//! detail pages, so none are saved.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::william_morris_society::{
    WilliamMorrisSociety, card_event, parse_listing,
};
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/william-morris-society";
const SITE: &str = "https://williammorrissociety.org";

fn listing_html() -> String {
    fixture(&format!("{DIR}/whats-on.html"))
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!(
        "william_morris_society_listing",
        parse_listing(&listing_html()).cards
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = WilliamMorrisSociety::new(SITE.parse().unwrap());
    let page_url: Url = format!("{SITE}/whats-on/").parse().unwrap();
    let out: Vec<serde_json::Value> = parse_listing(&listing_html())
        .cards
        .iter()
        .map(|card| {
            let raw = card_event(card, &page_url).unwrap();
            serde_json::json!({
                "id": raw.source_event_id,
                "payload": raw.payload,
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("william_morris_society_normalised", out);
}

#[tokio::test]
async fn fetches_the_listing_once_via_fetch_context() {
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
        .respond_with(ResponseTemplate::new(200).set_body_string(listing_html()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/events/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = WilliamMorrisSociety::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    let errors = ctx.take_errors();
    assert!(errors.is_empty(), "{errors:?}");
    let got: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    let expected: Vec<String> = parse_listing(&listing_html())
        .cards
        .iter()
        .map(|c| c.path.trim_matches('/').to_string())
        .collect();
    assert_eq!(got.len(), 15);
    assert_eq!(got, expected);
    let site = server.uri();
    for raw in &raws {
        let url = format!("{site}/{}/", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(url.as_str()));
    }
    let halloween = raws
        .iter()
        .find(|r| r.source_event_id == "events/halloween-fun-at-kelmscott-house")
        .unwrap();
    let event = s.normalise(halloween).unwrap().unwrap();
    assert_eq!(event.venue_name.as_deref(), Some("William Morris Society"));
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
    let s = WilliamMorrisSociety::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A template change must reach the health checker instead of producing
    // clean, empty runs.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><body><ul class="fusion-grid"></ul></body></html>"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = WilliamMorrisSociety::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event cards"), "{err}");
}

#[tokio::test]
async fn cards_without_a_title_link_are_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    let listing = r#"<html><body><ul class="fusion-grid">
        <li class="post-card"><h3 class="fusion-title-heading"><a href="https://williammorrissociety.org/events/a-talk/">A Talk</a></h3></li>
        <li class="post-card"><h3 class="fusion-title-heading">Renamed Heading</h3></li>
        </ul></body></html>"#;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(listing))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = WilliamMorrisSociety::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, ["events/a-talk"]);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("Renamed Heading"), "{errors:?}");
}
