//! The Sinfonia Smith Square scraper: a snapshot test over the saved
//! What's On feed and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::sinfonia_smith_square::{SinfoniaSmithSquare, parse_feed};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/sinfonia-smith-square";
const SITE: &str = "https://www.sinfoniasmithsq.org.uk";
const FEED_PATH: &str = "/wp-content/uploads/whats-on.json";

fn feed_url(base: &str) -> Url {
    Url::parse(&format!("{base}{FEED_PATH}")).unwrap()
}

fn feed() -> Vec<RawEvent> {
    let json = serde_json::from_str(&fixture(&format!("{DIR}/whats-on.json"))).unwrap();
    parse_feed(&json, &feed_url(SITE))
        .expect("feed")
        .into_iter()
        .map(|r| r.expect("event"))
        .collect()
}

#[test]
fn saved_robots_allow_the_feed() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    assert!(robots.allowed(&feed_url(SITE)));
}

#[test]
fn every_item_is_an_event_with_its_own_id() {
    let raws = feed();
    assert_eq!(raws.len(), 39);
    let mut ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), raws.len());
}

#[test]
fn normalised_output_snapshot() {
    let s = SinfoniaSmithSquare::new(SITE.parse().unwrap());
    let out: Vec<_> = feed()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("sinfonia_smith_square_normalised", out);
}

#[tokio::test]
async fn fetches_the_feed_via_fetch_context() {
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
        .and(path(FEED_PATH))
        .respond_with(
            // The saved feed links absolutely to the site; serve it as ours.
            ResponseTemplate::new(200)
                .set_body_string(
                    fixture(&format!("{DIR}/whats-on.json"))
                        .replace(&SITE.replace('/', "\\/"), &server.uri().replace('/', "\\/")),
                )
                .insert_header("content-type", "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = SinfoniaSmithSquare::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.clone()).collect();
    let want: Vec<_> = feed().into_iter().map(|r| r.source_event_id).collect();
    assert_eq!(ids, want);
}

#[tokio::test]
async fn a_feed_disallowed_by_robots_is_not_fetched() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /wp-content/\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(FEED_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = SinfoniaSmithSquare::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn an_empty_feed_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(FEED_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"filters":{},"items":[]}"#))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = SinfoniaSmithSquare::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no events"), "{err}");
}

#[tokio::test]
async fn a_bad_item_is_reported_and_the_rest_are_kept() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let feed = serde_json::json!({"items": [
        {"title": "Festival", "slug": "fest", "link": format!("{}/festival/fest/", server.uri())},
        {"title": "Concert", "slug": "concert", "link": format!("{}/event/concert/", server.uri())},
    ]});
    Mock::given(method("GET"))
        .and(path(FEED_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(feed))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = SinfoniaSmithSquare::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, ["concert"]);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("Festival"), "{errors:?}");
}
