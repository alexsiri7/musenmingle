//! LUX scraper: snapshot tests over the saved What's on page and detail
//! pages, and an end-to-end fetch against wiremock.
//!
//! `detail/` holds the page of every upcoming card plus one archived talk
//! at LUX (`artist-talk-asako-ujita`), the only in-venue timed event.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::lux::{
    Card, Lux, MAX_DETAIL_PAGES, card_event, in_scope, parse_detail, parse_listing,
};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/lux";
const SITE: &str = "https://lux.org.uk";
const DETAILS: [&str; 4] = [
    "artist-talk-asako-ujita",
    "late-night-conversations-stuart-marshall-tamara-krikorian",
    "online-visual-archives-as-a-means-to-process-trauma-azza-el-hassan",
    "study-day-sharing-strategies-caring-for-artist-moving-image-in-the-margins",
];

fn detail_html(slug: &str) -> String {
    fixture(&format!("{DIR}/detail/{slug}.html"))
}

fn cards() -> Vec<Card> {
    let listing = parse_listing(&fixture(&format!("{DIR}/whats-on.html"))).expect("listing");
    assert!(listing.problems.is_empty(), "{:?}", listing.problems);
    listing.cards
}

/// The raw events a run produces from the fixtures: detail pages only for
/// in-scope cards.
fn raws() -> Vec<RawEvent> {
    cards()
        .iter()
        .map(|card| {
            let url: Url = format!("{SITE}{}", card.path).parse().unwrap();
            let raw = card_event(card, &url, None);
            match in_scope(&raw.payload) {
                Some(_) => card_event(card, &url, Some(&parse_detail(&detail_html(&card.slug)))),
                None => raw,
            }
        })
        .collect()
}

#[test]
fn listing_reads_only_the_upcoming_grid() {
    let slugs: Vec<String> = cards().into_iter().map(|c| c.slug).collect();
    assert_eq!(slugs, &DETAILS[1..]);
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!("lux_listing", cards());
}

#[test]
fn details_snapshot() {
    let details: Vec<_> = DETAILS
        .iter()
        .map(|slug| serde_json::json!({"slug": slug, "detail": parse_detail(&detail_html(slug))}))
        .collect();
    insta::assert_json_snapshot!("lux_details", details);
}

#[test]
fn normalised_output_snapshot() {
    let s = Lux::new(SITE.parse().unwrap());
    let out: Vec<_> = raws()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "slug": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("lux_normalised", out);
}

#[test]
fn missing_upcoming_grid_is_an_error() {
    // A template change must reach the health checker; an empty grid is fine.
    assert!(parse_listing("<html><body><h2>What's on</h2></body></html>").is_err());
    let empty = r#"<div class="elementor-widget-loop-grid"></div>"#;
    assert!(parse_listing(empty).unwrap().cards.is_empty());
}

/// A grid card for a one-day lecture at LUX; `href` is its title link.
fn lecture_card(href: &str) -> String {
    format!(
        r#"<div class="e-loop-item event_catogories-lecture">
        <h5><a href="{href}">A Lecture</a></h5>
        <div class="elementor-widget-text-editor">LUX</div>
        <div class="elementor-widget-text-editor">6 November, 2026</div>
        <div class="elementor-widget-text-editor">– 6 November, 2026</div></div>"#
    )
}

fn grid(cards: &str) -> String {
    format!(r#"<html><body><div class="elementor-widget-loop-grid">{cards}</div></body></html>"#)
}

#[test]
fn unreadable_cards_are_problems() {
    let html = grid(
        &[
            r#"<div class="e-loop-item"><h5>No link</h5></div>"#.to_string(),
            lecture_card("https://lux.org.uk/news/not-an-event/"),
            lecture_card("https://lux.org.uk/event/a-lecture/"),
        ]
        .concat(),
    );
    let listing = parse_listing(&html).unwrap();
    let slugs: Vec<_> = listing.cards.iter().map(|c| c.slug.as_str()).collect();
    assert_eq!(slugs, ["a-lecture"]);
    assert_eq!(listing.problems.len(), 2, "{:?}", listing.problems);
}

async fn serve(server: &MockServer, at: &str, body: String, times: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(times)
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_listing_and_in_scope_details_via_fetch_context() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/robots.txt",
        fixture(&format!("{DIR}/robots.txt")),
        1,
    )
    .await;
    serve(
        &server,
        "/whats-on/",
        fixture(&format!("{DIR}/whats-on.html")),
        1,
    )
    .await;
    for slug in DETAILS {
        // Only the study day is in scope: the exhibition is password
        // protected, the lecture online and the talk archived.
        let times = u64::from(slug.starts_with("study-day"));
        serve(
            &server,
            &format!("/event/{slug}/"),
            detail_html(slug),
            times,
        )
        .await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Lux::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());

    let site = server.uri();
    let want = raws_with_site(&site);
    assert_eq!(raws.len(), want.len());
    for (raw, want) in raws.iter().zip(&want) {
        assert_eq!(raw.source_event_id, want.source_event_id);
        assert_eq!(raw.source_url, want.source_url);
        assert_eq!(raw.payload, want.payload);
    }
    let events: Vec<_> = raws
        .iter()
        .filter_map(|raw| s.normalise(raw).expect("normalise"))
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].starts_at.to_rfc3339(),
        "2026-11-06T09:00:00+00:00"
    );
}

#[tokio::test]
async fn detail_fetches_stop_at_the_cap_and_failed_cards_are_dropped() {
    // Two more in-scope cards than the cap, plus an unreadable one. The
    // first detail page fails; cards past the cap are never requested. Both
    // are left out rather than stored without their time.
    let slug = |i: usize| format!("lecture-{i}");
    let cards: String = (0..MAX_DETAIL_PAGES + 2)
        .map(|i| lecture_card(&format!("https://lux.org.uk/event/{}/", slug(i))))
        .chain([lecture_card("https://lux.org.uk/news/")])
        .collect();
    let server = MockServer::start().await;
    serve(
        &server,
        "/robots.txt",
        fixture(&format!("{DIR}/robots.txt")),
        1,
    )
    .await;
    serve(&server, "/whats-on/", grid(&cards), 1).await;
    Mock::given(method("GET"))
        .and(path(format!("/event/{}/", slug(0))))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    for i in 1..MAX_DETAIL_PAGES + 2 {
        let times = u64::from(i < MAX_DETAIL_PAGES);
        serve(
            &server,
            &format!("/event/{}/", slug(i)),
            "<html></html>".into(),
            times,
        )
        .await;
    }

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Lux::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.clone()).collect();
    let want: Vec<_> = (1..MAX_DETAIL_PAGES).map(slug).collect();
    assert_eq!(ids, want);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().any(|e| e.contains(&slug(0))), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("/news/")), "{errors:?}");
}

fn raws_with_site(site: &str) -> Vec<RawEvent> {
    raws()
        .into_iter()
        .map(|mut raw| {
            let url = raw.source_url.unwrap().replace(SITE, site);
            raw.payload["url"] = serde_json::json!(url);
            raw.source_url = Some(url);
            raw
        })
        .collect()
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/robots.txt",
        "User-agent: MuseNMingleBot\nDisallow: /\n".into(),
        1,
    )
    .await;
    serve(&server, "/whats-on/", "should never be fetched".into(), 0).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Lux::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
