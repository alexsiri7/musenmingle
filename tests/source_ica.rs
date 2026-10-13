//! The ICA scraper: a snapshot test over the saved listing and detail
//! pages, and an end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::{Price, RawEvent};
use musenmingle::sources::Source;
use musenmingle::sources::ica::{
    EXHIBITIONS_PATH, Ica, Section, TALKS_PATH, needs_detail, parse_detail, parse_listing,
    raw_event,
};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/ica";
const SITE: &str = "https://www.ica.art";

const PAGES: [(&str, &str, Section); 2] = [
    (TALKS_PATH, "talks.html", Section::Talks),
    (EXHIBITIONS_PATH, "exhibitions.html", Section::Exhibitions),
];

/// The date the fixtures were saved.
fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
}

/// The saved detail page for a card path (`/talks/my-tragedy` →
/// `detail-my-tragedy.html`).
fn detail_file(card_path: &str) -> String {
    let slug = card_path.rsplit('/').next().unwrap();
    format!("{DIR}/detail-{slug}.html")
}

/// The cards as `fetch` would store them.
fn raws() -> Vec<RawEvent> {
    let site = Url::parse(SITE).unwrap();
    PAGES
        .iter()
        .flat_map(|(_, file, section)| {
            parse_listing(&fixture(&format!("{DIR}/{file}")), *section)
                .into_iter()
                .map(|c| {
                    let detail = needs_detail(&c, *section, listed_on())
                        .then(|| parse_detail(&fixture(&detail_file(c["path"].as_str().unwrap()))));
                    raw_event(&c, *section, &site, listed_on(), detail)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn saved_robots_allow_the_listings_and_details() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in [
        TALKS_PATH,
        EXHIBITIONS_PATH,
        "/talks/my-tragedy",
        "/exhibitions/autumn-knight-party-done",
    ] {
        assert!(
            robots.allowed(&Url::parse(&format!("{SITE}{p}")).unwrap()),
            "{p}"
        );
    }
}

#[test]
fn programme_tabs_and_films_are_left_out() {
    let raws = raws();
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(raws.len(), 15, "{ids:?}");
    assert!(ids.iter().all(|id| !id.starts_with("/films/")));
    assert!(
        ids.iter()
            .all(|id| !id.contains("ica-creatives") && !id.contains("residency"))
    );
    for r in &raws {
        assert!(
            r.payload["card"]["date_text"].is_string(),
            "{}",
            r.source_event_id
        );
    }
    let details = raws
        .iter()
        .filter(|r| !r.payload["detail"].is_null())
        .count();
    assert_eq!(details, 8);
}

#[test]
fn normalised_output_snapshot() {
    let s = Ica::new(SITE.parse().unwrap());
    let out: Vec<_> = raws()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "date_text": raw.payload["card"]["date_text"],
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("ica_normalised", out);
}

#[tokio::test]
async fn fetches_the_listings_and_details_via_fetch_context() {
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
    let want = raws();
    for r in want.iter().filter(|r| !r.payload["detail"].is_null()) {
        Mock::given(method("GET"))
            .and(path(r.source_event_id.as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&detail_file(&r.source_event_id))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Ica::new(server.uri().parse().unwrap());
    let got = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g.source_event_id, w.source_event_id);
        assert_eq!(g.payload["card"], w.payload["card"]);
        assert_eq!(g.payload["detail"], w.payload["detail"]);
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
        .and(path(TALKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    assert!(
        Ica::new(server.uri().parse().unwrap())
            .fetch(&ctx)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_empty_talks_page_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(TALKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><main></main></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = Ica::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no cards"), "{err}");
}

#[test]
fn a_detail_page_without_times_gives_an_all_day_event() {
    let card = serde_json::json!({
        "path": "/talks/makers-market", "tab": "all-events", "title": "Makers Market",
        "date_text": "Sun, 6 December", "description": null, "image": null,
    });
    let detail = parse_detail(&fixture(&detail_file("/talks/makers-market")));
    assert_eq!(detail["performances"], serde_json::json!([]));
    assert!(detail["price_text"].is_null());
    let listed_on = NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
    let raw = raw_event(
        &card,
        Section::Talks,
        &Url::parse(SITE).unwrap(),
        listed_on,
        Some(detail),
    );
    let ev = Ica::new(SITE.parse().unwrap())
        .normalise(&raw)
        .expect("normalise")
        .expect("in scope");
    assert!(ev.all_day);
    assert_eq!(ev.starts_at.to_rfc3339(), "2026-12-06T00:00:00+00:00");
    assert_eq!(ev.ends_at, None);
    assert_eq!(ev.price, Price::default());
}

#[tokio::test]
async fn untimed_detail_pages_everywhere_report_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let cards: String = [("a", "Sun, 6 December"), ("b", "Mon, 7 December"), ("c", "Tue, 8 December")]
        .iter()
        .map(|(slug, date)| {
            format!(
                r#"<div class="item"><a href="/talks/{slug}"><div class="title">Talk {slug}</div><div class="date">{date}</div></a></div>"#
            )
        })
        .collect();
    Mock::given(method("GET"))
        .and(path(TALKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"<html><div class="category-section" id="all-events">{cards}</div></html>"#
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(EXHIBITIONS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .mount(&server)
        .await;
    for slug in ["a", "b", "c"] {
        Mock::given(method("GET"))
            .and(path(format!("/talks/{slug}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&detail_file("/talks/makers-market"))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let got = Ica::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(got.len(), 3);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("performance time"), "{errors:?}");
}
