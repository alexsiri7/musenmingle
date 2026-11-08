//! The Vortex Jazz Club scraper: a snapshot test over the saved iCal feed
//! and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::vortex_jazz_club::{VortexJazzClub, parse_feed};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/vortex-jazz-club";
const SITE: &str = "https://www.vortexjazz.co.uk";

fn feed_url(base: &str) -> Url {
    Url::parse(&format!("{base}/events/?ical=1")).unwrap()
}

fn feed() -> Vec<RawEvent> {
    parse_feed(&fixture(&format!("{DIR}/events.ics")), &feed_url(SITE))
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
fn every_vevent_is_an_event() {
    let raws = feed();
    assert_eq!(raws.len(), 49);
    assert!(
        raws.iter()
            .all(|r| r.source_url.as_deref().unwrap().starts_with(SITE))
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = VortexJazzClub::new(SITE.parse().unwrap());
    let out: Vec<_> = feed()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("vortex_jazz_club_normalised", out);
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
        .and(path("/events/"))
        .and(query_param("ical", "1"))
        .respond_with(
            // The saved feed links absolutely to the site; serve it as ours.
            ResponseTemplate::new(200)
                .set_body_string(fixture(&format!("{DIR}/events.ics")).replace(SITE, &server.uri()))
                .insert_header("content-type", "text/calendar; charset=UTF-8"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = VortexJazzClub::new(server.uri().parse().unwrap());
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
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /events/\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = VortexJazzClub::new(server.uri().parse().unwrap())
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
        .and(path("/events/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"),
        )
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = VortexJazzClub::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no events"), "{err}");
}
