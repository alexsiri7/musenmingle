//! The Estorick Collection scraper: a snapshot test over the saved listing
//! pages, and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::estorick_collection::{
    EVENTS_PATH, EXHIBITIONS_PATH, EstorickCollection, FUTURE_EXHIBITIONS_PATH, Section,
    parse_listing, raw_event,
};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/estorick-collection";
const SITE: &str = "https://www.estorickcollection.com";

const PAGES: [(&str, &str, Section); 3] = [
    (EVENTS_PATH, "events.html", Section::Events),
    (EXHIBITIONS_PATH, "exhibitions.html", Section::Exhibitions),
    (
        FUTURE_EXHIBITIONS_PATH,
        "exhibitions-future.html",
        Section::Exhibitions,
    ),
];

/// The cards as `fetch` would store them.
fn raws() -> Vec<RawEvent> {
    let site = Url::parse(SITE).unwrap();
    PAGES
        .iter()
        .flat_map(|(_, file, section)| {
            parse_listing(&fixture(&format!("{DIR}/{file}")), *section)
                .into_iter()
                .map(|c| raw_event(&c, *section, &site))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn saved_robots_allow_the_listings() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for (p, _, _) in PAGES {
        assert!(robots.allowed(&Url::parse(&format!("{SITE}{p}")).unwrap()));
    }
}

#[test]
fn every_card_has_a_date_line() {
    let raws = raws();
    assert_eq!(raws.len(), 17);
    for r in &raws {
        assert!(
            r.payload["card"]["date_text"].is_string(),
            "{}",
            r.source_event_id
        );
    }
}

#[test]
fn normalised_output_snapshot() {
    let s = EstorickCollection::new(SITE.parse().unwrap());
    let out: Vec<_> = raws()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "date_text": raw.payload["card"]["date_text"],
                "time_text": raw.payload["card"]["time_text"],
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("estorick_collection_normalised", out);
}

#[tokio::test]
async fn fetches_the_listings_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    for (p, file, _) in PAGES {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/{file}"))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = EstorickCollection::new(server.uri().parse().unwrap());
    let got = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let want = raws();
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g.source_event_id, w.source_event_id);
        assert_eq!(
            s.normalise(g)
                .unwrap()
                .map(|e| (e.title, e.starts_at, e.ends_at, e.category)),
            s.normalise(w)
                .unwrap()
                .map(|e| (e.title, e.starts_at, e.ends_at, e.category)),
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
        .and(path(EVENTS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    assert!(
        EstorickCollection::new(server.uri().parse().unwrap())
            .fetch(&ctx)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_empty_events_page_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(EVENTS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><main></main></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = EstorickCollection::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no event cards"), "{err}");
}
