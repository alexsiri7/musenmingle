//! Artlogic platform: snapshot tests over saved listing pages of galleries on
//! different templates (normalised with each gallery's seed `config`), the
//! shared robots.txt, and end-to-end fetches against wiremock.

mod common;

use std::time::Duration;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RateLimiter, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::artlogic::{
    Artlogic, ArtlogicConfig, Card, parse_listing, parse_single_show,
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SEED: &str = "migrations/20260927952001_seed_artlogic_galleries.sql";

/// Every `(key, base_url, config)` Artlogic row in the seed migration.
fn seed_rows() -> Vec<(String, String, Value)> {
    let sql = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SEED))
        .unwrap();
    let mut rows = Vec::new();
    for (i, _) in sql.match_indices("('artlogic-") {
        let row = &sql[i + 2..];
        let key = &row[..row.find('\'').unwrap()];
        let base = row.split('\'').nth(4).unwrap();
        let config = &row[row.find("'{").unwrap() + 1..row.find("}'::jsonb").unwrap() + 1];
        let config = serde_json::from_str(&config.replace("''", "'"))
            .unwrap_or_else(|e| panic!("{key}: config is not JSON: {e}"));
        rows.push((key.to_string(), base.to_string(), config));
    }
    rows
}

fn seed_row(key: &str) -> (String, Value) {
    let (_, base, config) = seed_rows()
        .into_iter()
        .find(|(k, _, _)| k == key)
        .unwrap_or_else(|| panic!("no seed row {key}"));
    (base, config)
}

fn source(key: &str, base: &str) -> Artlogic {
    let (_, config) = seed_row(key);
    Artlogic::from_row(key, base.parse().unwrap(), Some(&config)).unwrap()
}

fn raw(host: &str, card: &Card) -> RawEvent {
    RawEvent {
        source_event_id: format!("{host}:{}", card.id),
        source_url: Some(card.url.clone()),
        payload: json!({ "card": card }),
    }
}

/// A gallery's saved listing, parsed as if fetched from `<base><path>`.
fn listing(key: &str, path: &str) -> Vec<Card> {
    let (base, _) = seed_row(key);
    let url = Url::parse(&base).unwrap().join(path).unwrap();
    parse_listing(&fixture(&format!("scrapers/{key}/listing.html")), &url)
}

fn normalised(key: &str, cards: &[Card]) -> Vec<Value> {
    let (base, _) = seed_row(key);
    let s = source(key, &base);
    let host = Url::parse(&base).unwrap().host_str().unwrap().to_string();
    cards
        .iter()
        .map(|c| {
            let raw = raw(&host, c);
            json!({"id": raw.source_event_id, "event": s.normalise(&raw).expect("normalise")})
        })
        .collect()
}

#[test]
fn every_seeded_config_is_valid() {
    let rows = seed_rows();
    assert_eq!(rows.len(), 38);
    for (key, base, config) in rows {
        let config =
            ArtlogicConfig::from_json(Some(&config)).unwrap_or_else(|e| panic!("{key}: {e}"));
        assert!(config.venue.address.is_some(), "{key}: no address");
        let base = Url::parse(&base).unwrap();
        assert_eq!(base.path(), "/", "{key}");
    }
}

#[test]
fn saved_robots_allow_every_listing_path() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture("scrapers/artlogic/robots.txt").as_bytes(),
    );
    for (key, base, config) in seed_rows() {
        let config = ArtlogicConfig::from_json(Some(&config)).unwrap();
        for path in &config.listing_paths {
            let url = Url::parse(&base).unwrap().join(path).unwrap();
            assert!(robots.allowed(&url), "{key}: {url}");
        }
        let detail = Url::parse(&base)
            .unwrap()
            .join("/exhibitions/249-colin-self-unseen/")
            .unwrap();
        assert!(robots.allowed(&detail), "{key}: {detail}");
    }
    assert!(!robots.allowed(&Url::parse("https://www.frithstreetgallery.com/api/x").unwrap()));
    assert_eq!(robots.crawl_delay(), None);
}

#[test]
fn frith_street_records_list_normalised() {
    let cards = listing("artlogic-frith-street", "/exhibitions/");
    insta::assert_json_snapshot!(
        "artlogic_frith_street_normalised",
        normalised("artlogic-frith-street", &cards)
    );
}

#[test]
fn grimm_us_dates_and_london_filter_normalised() {
    let cards = listing("artlogic-grimm", "/exhibitions/");
    insta::assert_json_snapshot!(
        "artlogic_grimm_normalised",
        normalised("artlogic-grimm", &cards)
    );
}

#[test]
fn flowers_classic_template_normalised() {
    let cards = listing("artlogic-flowers", "/exhibitions/");
    insta::assert_json_snapshot!(
        "artlogic_flowers_normalised",
        normalised("artlogic-flowers", &cards)
    );
}

#[test]
fn ropac_em_dash_dates_normalised() {
    let cards = listing("artlogic-ropac", "/exhibitions/");
    insta::assert_json_snapshot!(
        "artlogic_ropac_normalised",
        normalised("artlogic-ropac", &cards)
    );
}

#[test]
fn pilar_corrias_dotted_dates_normalised() {
    let cards = listing("artlogic-pilar-corrias", "/exhibitions/");
    insta::assert_json_snapshot!(
        "artlogic_pilar_corrias_normalised",
        normalised("artlogic-pilar-corrias", &cards)
    );
}

#[test]
fn victoria_miro_id_only_links_normalised() {
    let cards = listing("artlogic-victoria-miro", "/exhibitions/");
    insta::assert_json_snapshot!(
        "artlogic_victoria_miro_normalised",
        normalised("artlogic-victoria-miro", &cards)
    );
}

#[test]
fn a_listing_that_lands_on_one_show_is_read_from_its_header() {
    let (base, _) = seed_row("artlogic-jaggedart");
    let landed = Url::parse(&base)
        .unwrap()
        .join("/exhibitions/258-entwined/works/")
        .unwrap();
    let card = parse_single_show(
        &fixture("scrapers/artlogic-jaggedart/current.html"),
        &landed,
    )
    .expect("a card");
    insta::assert_json_snapshot!(
        "artlogic_jaggedart_single_show_normalised",
        normalised("artlogic-jaggedart", &[card])
    );
}

#[test]
fn past_and_online_sections_are_not_read() {
    for (key, want) in [
        ("artlogic-frith-street", 2),
        ("artlogic-grimm", 10),
        ("artlogic-flowers", 8),
        ("artlogic-victoria-miro", 6),
    ] {
        let cards = listing(key, "/exhibitions/");
        assert_eq!(cards.len(), want, "{key}: {cards:#?}");
        assert!(
            cards.iter().all(|c| {
                let s = c.section.to_lowercase();
                !s.contains("past") && !s.contains("online") && !s.contains("external")
            }),
            "{key}"
        );
    }
}

/// `static-assets.artlogic.net` (every gallery's image host) asks all bots
/// for 10 s between requests. The ingest run has one FetchContext whose
/// limiter keys on the host, so thumbnails of two different galleries are
/// spaced by the Crawl-delay too.
#[tokio::test(start_paused = true)]
async fn the_shared_image_host_crawl_delay_spans_galleries() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture("scrapers/artlogic/static-assets-robots.txt").as_bytes(),
    );
    let delay = robots.crawl_delay();
    assert_eq!(delay, Some(Duration::from_secs(10)));
    let frith = Url::parse("https://static-assets.artlogic.net/w_600/ws-frithstreetgallery/usr/images/exhibitions/a.jpg").unwrap();
    let grimm = Url::parse(
        "https://static-assets.artlogic.net/w_600/ws-grimm/usr/images/exhibitions/b.jpg",
    )
    .unwrap();
    assert!(robots.allowed(&frith) && robots.allowed(&grimm));
    let limiter = RateLimiter::new(RateLimitConfig::default());
    let start = tokio::time::Instant::now();
    limiter.acquire(frith.host_str().unwrap(), delay).await;
    limiter.acquire(grimm.host_str().unwrap(), delay).await;
    assert_eq!(start.elapsed(), Duration::from_secs(10));
}

async fn mount_open_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture("scrapers/artlogic/robots.txt")),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_page(server: &MockServer, at: &str, template: ResponseTemplate, expect: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(template)
        .expect(expect)
        .mount(server)
        .await;
}

fn ctx() -> FetchContext {
    FetchContext::new(RateLimitConfig::disabled()).unwrap()
}

fn gallery(server: &MockServer, config: Value) -> Artlogic {
    Artlogic::from_row(
        "artlogic-test",
        server.uri().parse().unwrap(),
        Some(&config),
    )
    .unwrap()
}

fn ids(raws: &[RawEvent]) -> Vec<String> {
    raws.iter().map(|r| r.source_event_id.clone()).collect()
}

#[tokio::test]
async fn fetches_the_listing_via_fetch_context() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    let html = fixture("scrapers/artlogic-grimm/listing.html");
    mount_page(
        &server,
        "/exhibitions/",
        ResponseTemplate::new(200).set_body_string(html),
        1,
    )
    .await;
    let ctx = ctx();
    let (_, config) = seed_row("artlogic-grimm");
    let s = gallery(&server, config);
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let host = Url::parse(&server.uri())
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    let want: Vec<String> = listing("artlogic-grimm", "/exhibitions/")
        .iter()
        .map(|c| format!("{host}:{}", c.id))
        .collect();
    assert_eq!(ids(&raws), want);
    let kept = raws
        .iter()
        .filter_map(|r| s.normalise(r).expect("normalise"))
        .count();
    assert_eq!(kept, 3, "only the London shows");
}

#[tokio::test]
async fn a_listing_redirecting_to_one_show_yields_that_show() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    let redirect =
        ResponseTemplate::new(302).insert_header("location", "/exhibitions/258-entwined/works/");
    mount_page(&server, "/exhibitions/current/", redirect, 1).await;
    let html = fixture("scrapers/artlogic-jaggedart/current.html");
    mount_page(
        &server,
        "/exhibitions/258-entwined/works/",
        ResponseTemplate::new(200).set_body_string(html),
        1,
    )
    .await;
    mount_page(
        &server,
        "/exhibitions/forthcoming/",
        ResponseTemplate::new(200).set_body_string("<html><body></body></html>"),
        1,
    )
    .await;
    let ctx = ctx();
    let (_, config) = seed_row("artlogic-jaggedart");
    let s = gallery(&server, config);
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 1);
    assert!(raws[0].source_event_id.ends_with(":258"), "{raws:?}");
    assert!(
        raws[0]
            .source_url
            .as_deref()
            .is_some_and(|u| u.ends_with("/exhibitions/258-entwined/")),
        "{raws:?}"
    );
    let event = s.normalise(&raws[0]).unwrap().expect("an event");
    assert_eq!(event.title, "Entwined");
}

#[tokio::test]
async fn one_failing_listing_is_reported_and_the_others_kept() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    let html = fixture("scrapers/artlogic-frith-street/listing.html");
    mount_page(
        &server,
        "/exhibitions/current/",
        ResponseTemplate::new(200).set_body_string(html),
        1,
    )
    .await;
    mount_page(
        &server,
        "/exhibitions/forthcoming/",
        ResponseTemplate::new(500),
        1,
    )
    .await;
    let ctx = ctx();
    let s = gallery(
        &server,
        json!({"listing_paths": ["/exhibitions/current/", "/exhibitions/forthcoming/"],
               "venue": {"name": "G", "address": "London"}}),
    );
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert_eq!(raws.len(), 2);
    let errors = ctx.take_errors();
    assert!(
        errors.len() == 1
            && errors[0].contains("/exhibitions/forthcoming/")
            && errors[0].contains("HTTP 500"),
        "{errors:?}"
    );
}

#[tokio::test]
async fn every_listing_failing_fails_the_run() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_page(&server, "/exhibitions/", ResponseTemplate::new(503), 1).await;
    let ctx = ctx();
    let err = gallery(&server, json!({"venue": {"name": "G"}}))
        .fetch(&ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("HTTP 503"), "{err}");
    assert!(
        ctx.take_errors().is_empty(),
        "counted once, as the run's failure"
    );
}

#[tokio::test]
async fn robots_disallow_blocks_the_gallery() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("User-agent: MuseNMingleBot\nDisallow: /\n"),
        )
        .mount(&server)
        .await;
    mount_page(&server, "/exhibitions/", ResponseTemplate::new(200), 0).await;
    let err = gallery(&server, json!({"venue": {"name": "G"}}))
        .fetch(&ctx())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("robots.txt disallows"), "{err}");
}
