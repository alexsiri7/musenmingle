//! The ICA scraper: a snapshot test over the saved listing and detail
//! pages, and an end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::ica::{
    EXHIBITIONS_PATH, Ica, LIVE_PATH, MAX_DETAILS, MAX_LIVE_DETAILS, Section, TALKS_PATH,
    needs_detail, parse_detail, parse_listing, raw_event,
};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/ica";
const SITE: &str = "https://www.ica.art";

const PAGES: [(&str, &str, Section); 3] = [
    (TALKS_PATH, "talks.html", Section::Talks),
    (EXHIBITIONS_PATH, "exhibitions.html", Section::Exhibitions),
    (LIVE_PATH, "live.html", Section::Live),
];

/// The date the fixtures were saved (the Makers Market card and its
/// detail page, an untimed single day (#321), were added on 2026-10-11;
/// `/live` and its detail pages on 2026-11-09, whose 2027 dates carry
/// their year).
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
        LIVE_PATH,
        "/talks/my-tragedy",
        "/live/goat-japan",
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
    assert_eq!(raws.len(), 48, "{ids:?}");
    let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
    assert_eq!(ids.iter().filter(|id| id.starts_with("/live/")).count(), 32);
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
    assert_eq!(details, 40);
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

/// A listing card for a single day (`section` is `talks` or `live`).
fn day_card(section: &str, slug: &str) -> String {
    format!(
        r#"<div class="item {section}"><a href="/{section}/{slug}"><div class="title">{slug}</div>
        <div class="date sans">Tue, 10 November</div></a></div>"#
    )
}

fn all_events(cards: &str) -> String {
    format!(r#"<div class="category-section" id="all-events">{cards}</div>"#)
}

const DAY_DETAIL: &str = r#"<div class="performance-list"><div class="performance">
    <div class="date">Tue, 10 Nov 2026</div><div class="time">07:30 pm</div></div></div>"#;

/// A server for the three listings, robots.txt a 404.
async fn serve_listings(talks: String, live: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    for (p, body) in [
        (TALKS_PATH, talks),
        (EXHIBITIONS_PATH, "<html></html>".to_string()),
        (LIVE_PATH, live),
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
    }
    server
}

/// A `/talks` listing with one all-day range card (no detail page).
fn one_range_talk() -> String {
    all_events(
        r#"<div class="item"><a href="/talks/r"><div class="title">Talk: R</div>
        <div class="date">1 – 2 October 2026</div></a></div>"#,
    )
}

#[tokio::test]
async fn live_cards_past_the_cap_are_left_out_without_an_error() {
    let cards: String = (0..=MAX_LIVE_DETAILS)
        .map(|i| day_card("live", &format!("c{i}")))
        .collect();
    let server = serve_listings(one_range_talk(), all_events(&cards)).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/live/c\d+$"))
        .respond_with(ResponseTemplate::new(200).set_body_string(DAY_DETAIL))
        .expect(MAX_LIVE_DETAILS as u64)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let got = Ica::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(got.len(), 1 + MAX_LIVE_DETAILS);
    let last = format!("/live/c{MAX_LIVE_DETAILS}");
    assert!(got.iter().all(|r| r.source_event_id != last));
}

#[tokio::test]
async fn a_failed_live_detail_does_not_use_up_the_cap() {
    let cards: String = std::iter::once(day_card("live", "broken"))
        .chain((0..=MAX_LIVE_DETAILS).map(|i| day_card("live", &format!("c{i}"))))
        .collect();
    let server = serve_listings(one_range_talk(), all_events(&cards)).await;
    Mock::given(method("GET"))
        .and(path("/live/broken"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/live/c\d+$"))
        .respond_with(ResponseTemplate::new(200).set_body_string(DAY_DETAIL))
        .expect(MAX_LIVE_DETAILS as u64)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let got = Ica::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("/live/broken"), "{errors:?}");
    assert_eq!(got.len(), 1 + MAX_LIVE_DETAILS);
    let last = format!("/live/c{}", MAX_LIVE_DETAILS - 1);
    assert!(got.iter().any(|r| r.source_event_id == last));
}

#[tokio::test]
async fn talks_past_the_cap_are_errors() {
    let cards: String = (0..=MAX_DETAILS)
        .map(|i| day_card("talks", &format!("t{i}")))
        .collect();
    let server = serve_listings(all_events(&cards), "<html></html>".to_string()).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/talks/t\d+$"))
        .respond_with(ResponseTemplate::new(200).set_body_string(DAY_DETAIL))
        .expect(MAX_DETAILS as u64)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let got = Ica::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    let errors = ctx.take_errors();
    let last = format!("/talks/t{MAX_DETAILS}");
    assert_eq!(
        errors,
        [format!("{last}: over the {MAX_DETAILS} detail-page cap")]
    );
    assert_eq!(got.len(), MAX_DETAILS);
    assert!(got.iter().all(|r| r.source_event_id != last));
}
