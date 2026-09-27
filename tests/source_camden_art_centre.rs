//! The Camden Art Centre scraper: a snapshot test over the saved feed and
//! event pages, and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::camden_art_centre::{
    CamdenArtCentre, category, item_path, parse_detail, parse_feed, raw_event,
};
use reqwest::StatusCode;
use serde_json::Value;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/camden-art-centre";
const SITE: &str = "https://camdenartcentre.org";

fn feed_items() -> Vec<Value> {
    ["api-page-1.json", "api-page-2.json"]
        .iter()
        .flat_map(|f| {
            let v: Value = serde_json::from_str(&fixture(&format!("{DIR}/{f}"))).unwrap();
            parse_feed(&v).unwrap().0
        })
        .collect()
}

/// The feed items as `fetch` would store them, event pages read from the
/// fixtures (skipped programme types have none).
fn raws() -> Vec<RawEvent> {
    let site = Url::parse(SITE).unwrap();
    feed_items()
        .iter()
        .map(|item| {
            let path = item_path(item).expect("event link");
            let in_scope = category(item["type"]["slug"].as_str().unwrap()).is_some();
            let detail = in_scope.then(|| {
                let slug = path.trim_start_matches("/whats-on/");
                parse_detail(&fixture(&format!("{DIR}/{slug}.html")))
            });
            raw_event(item, &path, &site, detail)
        })
        .collect()
}

#[test]
fn saved_robots_allow_the_feed_and_event_pages() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in [
        "/api/programmes?format=in-the-building&page=1",
        "/whats-on/a-hard-line-to-bend",
    ] {
        assert!(robots.allowed(&Url::parse(&format!("{SITE}{p}")).unwrap()));
    }
}

#[test]
fn every_feed_item_links_to_its_page() {
    let items = feed_items();
    assert_eq!(items.len(), 11);
    assert!(items.iter().all(|i| item_path(i).is_some()));
}

#[test]
fn normalised_output_snapshot() {
    let s = CamdenArtCentre::new(SITE.parse().unwrap());
    let out: Vec<_> = raws()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "type": raw.payload["item"]["type"]["slug"],
                "page_date": raw.payload["detail"]["date_text"],
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("camden_art_centre_normalised", out);
}

#[tokio::test]
async fn fetches_the_feed_and_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    for page in ["1", "2"] {
        Mock::given(method("GET"))
            .and(path("/api/programmes"))
            .and(query_param("format", "in-the-building"))
            .and(query_param("page", page))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(fixture(&format!("{DIR}/api-page-{page}.json"))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let mut pages = 0;
    for raw in raws().iter().filter(|r| !r.payload["detail"].is_null()) {
        let slug = raw.source_event_id.trim_start_matches("/whats-on/");
        Mock::given(method("GET"))
            .and(path(raw.source_event_id.as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/{slug}.html"))),
            )
            .expect(1)
            .mount(&server)
            .await;
        pages += 1;
    }
    assert_eq!(pages, 8);
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = CamdenArtCentre::new(server.uri().parse().unwrap());
    let got = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let want = raws();
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g.source_event_id, w.source_event_id);
        assert_eq!(
            s.normalise(g)
                .unwrap()
                .map(|e| (e.title, e.starts_at, e.ends_at, e.price)),
            s.normalise(w)
                .unwrap()
                .map(|e| (e.title, e.starts_at, e.ends_at, e.price)),
        );
    }
}

#[tokio::test]
async fn robots_disallow_stops_the_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /\n"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/programmes"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    assert!(
        CamdenArtCentre::new(server.uri().parse().unwrap())
            .fetch(&ctx)
            .await
            .is_err()
    );
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
        .and(path("/api/programmes"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"data":[],"meta":{"last_page":1}}"#),
        )
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = CamdenArtCentre::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no items"), "{err}");
}
