//! Gasworks scraper: snapshot tests over the saved listings and an
//! end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::gasworks::{Gasworks, MAX_DETAILS, Section, parse_detail, parse_listing};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/gasworks";
const SITE: &str = "https://www.gasworks.org.uk";

/// The day the fixtures were fetched.
fn listed_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
}

fn listing(file: &str, p: &str, section: Section) -> Vec<RawEvent> {
    let url: Url = format!("{SITE}{p}").parse().unwrap();
    parse_listing(
        &fixture(&format!("{DIR}/{file}")),
        &url,
        section,
        listed_on(),
    )
    .items
}

fn all() -> Vec<RawEvent> {
    let mut v = listing("exhibitions.html", "/exhibitions/", Section::Exhibitions);
    v.extend(listing("events.html", "/events/", Section::Events));
    v
}

#[test]
fn robots_allows_the_listings_and_asks_for_a_slow_rate() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in [
        "/exhibitions/",
        "/events/",
        "/events/elders-2046-for-living-and-dying-otherwise/",
    ] {
        assert!(robots.allowed(&Url::parse(&format!("{SITE}{p}")).unwrap()));
    }
    assert_eq!(
        robots.crawl_delay(),
        Some(std::time::Duration::from_secs(20))
    );
    // The 1/60 Request-rate is a built-in floor.
    let c = RateLimitConfig::default();
    assert_eq!(
        c.interval_for("www.gasworks.org.uk"),
        std::time::Duration::from_secs(60)
    );
    // Robots.txt, two listings and 6 detail pages, 60 s apart, plus a margin.
    let s = Gasworks::new(SITE.parse().unwrap());
    assert_eq!(s.fetch_timeout(), Some(std::time::Duration::from_secs(600)));
}

#[test]
fn only_current_and_forthcoming_cards_are_read() {
    let ex = listing("exhibitions.html", "/exhibitions/", Section::Exhibitions);
    let ids: Vec<_> = ex.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "exhibitions/paloma-contreras-lomas-exhibition",
            "exhibitions/thuy-tien-nguyen"
        ]
    );
    let ev = listing("events.html", "/events/", Section::Events);
    let ids: Vec<_> = ev.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "exhibitions/gasworks-x-cotch-presents-disco-inferno",
            "events/elders-2046-for-living-and-dying-otherwise",
            "events/curators-tour-disco-inferno",
            "events/disco-inferno-neighbourhood-breakfast-exhibition-tour",
        ]
    );
}

#[test]
fn normalised_output_snapshot() {
    let s = Gasworks::new(SITE.parse().unwrap());
    let out: Vec<_> = all()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "id": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("gasworks_normalised", out);
}

#[test]
fn detail_page_location() {
    assert_eq!(
        parse_detail(&fixture(&format!("{DIR}/detail-elders-2046.html"))).as_deref(),
        Some("Various times at Wellcome Collection, 183 Euston Road, London NW1 2BE")
    );
    assert_eq!(
        parse_detail(&fixture(&format!("{DIR}/detail-curators-tour.html"))).as_deref(),
        Some("12:30–1pm")
    );
}

#[test]
fn off_site_normalised_output_snapshot() {
    let s = Gasworks::new(SITE.parse().unwrap());
    let mut raw = listing("events.html", "/events/", Section::Events)
        .into_iter()
        .find(|r| r.source_event_id == ELDERS)
        .expect("Elders card");
    raw.payload["location"] = serde_json::json!(parse_detail(&fixture(&format!(
        "{DIR}/detail-elders-2046.html"
    ))));
    insta::assert_json_snapshot!(
        "gasworks_off_site_normalised",
        s.normalise(&raw).expect("normalise")
    );
}

const ELDERS: &str = "events/elders-2046-for-living-and-dying-otherwise";
const EVENT_DETAILS: [&str; 4] = [
    "/exhibitions/gasworks-x-cotch-presents-disco-inferno/",
    "/events/elders-2046-for-living-and-dying-otherwise/",
    "/events/curators-tour-disco-inferno/",
    "/events/disco-inferno-neighbourhood-breakfast-exhibition-tour/",
];

async fn mount_open_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .mount(server)
        .await;
}

async fn mount_page(server: &MockServer, p: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

/// A test context with no configured interval (robots.txt's Crawl-delay
/// still applies, so fetch tests use an open robots.txt without one).
fn ctx() -> FetchContext {
    FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap()
}

async fn mount_listings(server: &MockServer) {
    mount_open_robots(server).await;
    mount_page(
        server,
        "/exhibitions/",
        fixture(&format!("{DIR}/exhibitions.html")),
    )
    .await;
    mount_page(server, "/events/", fixture(&format!("{DIR}/events.html"))).await;
}

#[tokio::test]
async fn fetches_listings_and_event_details_via_fetch_context() {
    let server = MockServer::start().await;
    mount_listings(&server).await;
    for p in EVENT_DETAILS {
        let file = if p.contains("elders") {
            "detail-elders-2046.html"
        } else {
            "detail-curators-tour.html"
        };
        mount_page(&server, p, fixture(&format!("{DIR}/{file}"))).await;
    }
    // Exhibition detail pages are never requested.
    for p in [
        "/exhibitions/paloma-contreras-lomas-exhibition/",
        "/exhibitions/thuy-tien-nguyen/",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
    }

    let ctx = ctx();
    let s = Gasworks::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 6);
    let site = server.uri();
    for raw in &raws {
        let expected = format!("{site}/{}/", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(expected.as_str()));
        let e = s.normalise(raw).expect("normalise").expect("in scope");
        let venue = if raw.source_event_id == ELDERS {
            "Wellcome Collection"
        } else {
            "Gasworks"
        };
        assert_eq!(
            e.venue_name.as_deref(),
            Some(venue),
            "{}",
            raw.source_event_id
        );
    }
}

#[tokio::test]
async fn failed_detail_page_is_reported_and_keeps_the_card() {
    let server = MockServer::start().await;
    mount_listings(&server).await;
    Mock::given(method("GET"))
        .and(path_regex("^/(exhibitions|events)/.+"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let ctx = ctx();
    let s = Gasworks::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert_eq!(ctx.take_errors().len(), EVENT_DETAILS.len());
    assert_eq!(raws.len(), 6);
    for raw in &raws {
        let e = s.normalise(raw).expect("normalise").expect("in scope");
        assert_eq!(e.venue_name.as_deref(), Some("Gasworks"));
    }
}

#[tokio::test]
async fn detail_page_without_a_location_line_is_reported() {
    let server = MockServer::start().await;
    mount_listings(&server).await;
    for p in EVENT_DETAILS {
        mount_page(&server, p, "<html><body></body></html>".to_string()).await;
    }

    let ctx = ctx();
    let s = Gasworks::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), EVENT_DETAILS.len());
    assert!(
        errors.iter().all(|e| e.contains("no location line")),
        "{errors:?}"
    );
    assert_eq!(raws.len(), 6);
    for raw in &raws {
        let e = s.normalise(raw).expect("normalise").expect("in scope");
        assert_eq!(e.venue_name.as_deref(), Some("Gasworks"));
    }
}

#[tokio::test]
async fn detail_pages_past_the_cap_are_reported_and_not_fetched() {
    let cards: String = (1..=MAX_DETAILS + 1)
        .map(|i| {
            format!(
                r#"<article class="list-item"><header><h3>Event</h3><h2 class="date">1 Oct 30</h2><h1><a href="/events/talk-{i}/">Talk {i}</a></h1></header></article>"#
            )
        })
        .collect();
    let events = format!(
        r#"<html><body><main><section id="current">{cards}</section><section id="archive"></section></main></body></html>"#
    );
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_page(
        &server,
        "/exhibitions/",
        fixture(&format!("{DIR}/exhibitions.html")),
    )
    .await;
    mount_page(&server, "/events/", events).await;
    for i in 1..=MAX_DETAILS {
        mount_page(
            &server,
            &format!("/events/talk-{i}/"),
            fixture(&format!("{DIR}/detail-elders-2046.html")),
        )
        .await;
    }
    let over = format!("/events/talk-{}/", MAX_DETAILS + 1);
    Mock::given(method("GET"))
        .and(path(over.as_str()))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = ctx();
    let s = Gasworks::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("detail-page cap"), "{errors:?}");
    let venues: Vec<_> = raws
        .iter()
        .filter(|r| r.source_event_id.starts_with("events/talk-"))
        .map(|r| {
            s.normalise(r)
                .expect("normalise")
                .expect("in scope")
                .venue_name
        })
        .collect();
    let mut expected = vec![Some("Wellcome Collection".to_string()); MAX_DETAILS];
    expected.push(Some("Gasworks".to_string()));
    assert_eq!(venues, expected);
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /exhibitions\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/exhibitions/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("never fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let s = Gasworks::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx()).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn no_cards_at_all_is_an_error_but_a_quiet_season_is_not() {
    // A robots.txt without the Crawl-delay keeps this test fast.
    let empty = r#"<html><body><main><section id="archive"></section></main></body></html>"#;
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_page(&server, "/exhibitions/", empty.to_string()).await;
    let s = Gasworks::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx()).await.unwrap_err().to_string();
    assert!(err.contains("no exhibition cards"), "{err}");

    // Archive cards only: nothing current, no error.
    let archive_only = r#"<html><body><main><section id="archive"><article class="list-item"><header><h3>Exhibition</h3><h2 class="date">2 Oct – 14 Dec 25</h2><h1><a href="/exhibitions/old/">Old</a></h1></header></article></section></main></body></html>"#;
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_page(&server, "/exhibitions/", archive_only.to_string()).await;
    mount_page(
        &server,
        "/events/",
        archive_only.replace("/exhibitions/old/", "/events/old/"),
    )
    .await;
    let ctx = ctx();
    let raws = s_fetch(&server, &ctx).await;
    assert!(raws.is_empty());
    assert!(ctx.take_errors().is_empty());
}

async fn s_fetch(server: &MockServer, ctx: &FetchContext) -> Vec<RawEvent> {
    Gasworks::new(server.uri().parse().unwrap())
        .fetch(ctx)
        .await
        .expect("fetch")
}
