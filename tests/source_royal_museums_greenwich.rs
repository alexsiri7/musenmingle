//! Royal Museums Greenwich: a snapshot test over the saved `/whats-on-api`
//! feed and an end-to-end fetch against wiremock. The site's robots.txt is
//! a 404 (everything allowed), so the fetch test serves a 404 too.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::royal_museums_greenwich::{Item, RoyalMuseumsGreenwich, raw_events};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FEED: &str = "scrapers/royal-museums-greenwich/whats-on-api.json";
const SITE: &str = "https://www.rmg.co.uk";

fn raws(site: &str) -> Vec<RawEvent> {
    let items: Vec<Item> = serde_json::from_str(&fixture(FEED)).expect("feed");
    let (raws, problems) = raw_events(&items, &site.parse::<Url>().unwrap());
    assert!(problems.is_empty(), "{problems:?}");
    raws
}

#[test]
fn normalised_output_snapshot() {
    let s = RoyalMuseumsGreenwich::new(SITE.parse().unwrap());
    let out: Vec<_> = raws(SITE)
        .iter()
        .map(|raw| {
            serde_json::json!({
                "path": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("royal_museums_greenwich_normalised", out);
}

async fn serve(server: &MockServer, at: &str, response: ResponseTemplate, times: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .expect(times)
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_the_feed_via_fetch_context() {
    let server = MockServer::start().await;
    serve(&server, "/robots.txt", ResponseTemplate::new(404), 1).await;
    serve(
        &server,
        "/whats-on-api",
        ResponseTemplate::new(200).set_body_string(fixture(FEED)),
        1,
    )
    .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = RoyalMuseumsGreenwich::new(server.uri().parse().unwrap());
    let got = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(got, raws(&server.uri()));
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/robots.txt",
        ResponseTemplate::new(200).set_body_string("User-agent: MuseNMingleBot\nDisallow: /\n"),
        1,
    )
    .await;
    serve(&server, "/whats-on-api", ResponseTemplate::new(200), 0).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = RoyalMuseumsGreenwich::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
