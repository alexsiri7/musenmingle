//! ArtRabbit listing scraper: snapshot tests over saved (trimmed) listing
//! pages, an end-to-end fetch against wiremock, and the cross-source merge
//! with a venue scraper's copy of the same show.
//!
//! The fixtures are trimmed to the show cards (no images, no social buttons)
//! and the pagination: ArtRabbit's terms forbid reproducing the site, so we
//! keep only what these tests need.

mod common;

use common::{TestDb, fixture};
use thaleia::config::RateLimitConfig;
use thaleia::fetch::FetchContext;
use thaleia::matching::{self, MatchInput};
use thaleia::model::{NewEvent, RawEvent};
use thaleia::repo;
use thaleia::sources::Source;
use thaleia::sources::artrabbit::{self, ArtRabbit, parse_listing};
use thaleia::sources::somerset_house;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/artrabbit";
const LISTING: &str = "/all-shows/united-kingdom/london";
const PAGES: [&str; 2] = ["page-1.html", "page-2.html"];

fn page(name: &str) -> String {
    fixture(&format!("{DIR}/{name}"))
}

fn fixture_cards() -> Vec<RawEvent> {
    PAGES
        .iter()
        .flat_map(|p| parse_listing(&page(p)).events)
        .collect()
}

fn artrabbit_card(id: &str) -> RawEvent {
    fixture_cards()
        .into_iter()
        .find(|r| r.source_event_id == id)
        .unwrap_or_else(|| panic!("card {id} in fixtures"))
}

#[test]
fn listing_snapshot() {
    let pages: Vec<_> = PAGES
        .iter()
        .map(|p| {
            let parsed = parse_listing(&page(p));
            serde_json::json!({
                "page": p,
                "has_next": parsed.has_next,
                "cards": parsed.events,
            })
        })
        .collect();
    insta::assert_json_snapshot!("artrabbit_listing", pages);
}

#[test]
fn normalised_output_snapshot() {
    let s = ArtRabbit::new("https://www.artrabbit.com".parse().unwrap());
    let out: Vec<_> = fixture_cards()
        .iter()
        .map(|raw| {
            serde_json::json!({
                "source_event_id": raw.source_event_id,
                "source_url": raw.source_url,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    assert_eq!(out.len(), 40);
    insta::assert_json_snapshot!("artrabbit_normalised", out);
}

#[test]
fn cards_carry_facts_only() {
    for raw in fixture_cards() {
        let keys: Vec<&String> = raw.payload.as_object().unwrap().keys().collect();
        for k in &keys {
            assert!(
                [
                    "url",
                    "title",
                    "category",
                    "date_text",
                    "venue",
                    "place",
                    "lat",
                    "lng"
                ]
                .contains(&k.as_str()),
                "unexpected payload field {k}"
            );
        }
        let text = raw.payload.to_string();
        assert!(!text.contains("img.artrabbit.com"), "{text}");
        let event = artrabbit::normalise_payload(&raw.payload).unwrap().unwrap();
        assert_eq!(event.description, None);
        assert_eq!(event.image_url, None);
    }
}

#[test]
fn page_past_the_end_is_empty() {
    let parsed = parse_listing(&page("page-past-end.html"));
    assert!(parsed.events.is_empty());
    assert!(!parsed.has_next);
}

async fn mock_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page("robots.txt")))
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
}

async fn mock_page(server: &MockServer, n: Option<&str>, body: ResponseTemplate, times: u64) {
    let m = Mock::given(method("GET")).and(path(LISTING));
    let m = match n {
        Some(n) => m.and(query_param("page", n)),
        None => m.and(query_param_is_missing("page")),
    };
    m.respond_with(body).expect(times).mount(server).await;
}

fn html(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(page(name))
}

#[tokio::test]
async fn pages_through_the_listing_until_there_is_no_next_page() {
    let server = MockServer::start().await;
    mock_robots(&server).await;
    mock_page(&server, None, html("page-1.html"), 1).await;
    mock_page(&server, Some("2"), html("page-2.html"), 1).await;
    mock_page(&server, Some("3"), html("page-past-end.html"), 1).await;
    mock_page(&server, Some("4"), html("page-past-end.html"), 0).await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = ArtRabbit::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), 40);
    // Links point at ArtRabbit's canonical event pages, never the mock.
    assert!(raws.iter().all(|r| {
        r.source_url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://www.artrabbit.com/events/"))
    }));
}

#[tokio::test]
async fn stops_at_the_page_cap() {
    let server = MockServer::start().await;
    mock_robots(&server).await;
    mock_page(&server, None, html("page-1.html"), 1).await;
    mock_page(&server, Some("2"), html("page-2.html"), 0).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = ArtRabbit::with_max_pages(server.uri().parse().unwrap(), 1)
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 20);
}

#[tokio::test]
async fn a_failing_later_page_is_a_soft_error_and_ends_paging() {
    let server = MockServer::start().await;
    mock_robots(&server).await;
    mock_page(&server, None, html("page-1.html"), 1).await;
    mock_page(&server, Some("2"), ResponseTemplate::new(403), 1).await;
    mock_page(&server, Some("3"), html("page-past-end.html"), 0).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let raws = ArtRabbit::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 20);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("listing page 2"), "{errors:?}");
}

#[tokio::test]
async fn empty_first_page_is_an_error() {
    let server = MockServer::start().await;
    mock_robots(&server).await;
    mock_page(&server, None, html("page-past-end.html"), 1).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = ArtRabbit::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no show cards"), "{err}");
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: ThaleiaBot\nDisallow: /all-shows\n"),
        )
        .mount(&server)
        .await;
    mock_page(&server, None, html("page-1.html"), 0).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let err = ArtRabbit::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

// ---- cross-source merge -------------------------------------------------

/// ArtRabbit's card for "Jenkin Van Zyl: Enclosure" (Somerset House Studios).
const JENKIN_ARTRABBIT_ID: &str = "3037765";
const JENKIN_SOMERSET_SLUG: &str = "jenkin-van-zyl-enclosure";

fn somerset_jenkin() -> RawEvent {
    somerset_house::parse_listing(&fixture("scrapers/somerset-house/whats-on-page-2.html"))
        .expect("somerset listing parses")
        .events
        .into_iter()
        .find(|r| r.source_event_id == JENKIN_SOMERSET_SLUG)
        .expect("Jenkin van Zyl in the Somerset House fixture")
}

fn both_jenkins() -> (RawEvent, NewEvent, RawEvent, NewEvent) {
    let ar_raw = artrabbit_card(JENKIN_ARTRABBIT_ID);
    let ar = artrabbit::normalise_payload(&ar_raw.payload)
        .unwrap()
        .unwrap();
    let sh_raw = somerset_jenkin();
    let sh = somerset_house::normalise_payload(&sh_raw.payload)
        .unwrap()
        .unwrap();
    (ar_raw, ar, sh_raw, sh)
}

#[test]
fn artrabbit_copy_of_a_venue_show_matches_the_venue_scraper() {
    let (_, ar, _, sh) = both_jenkins();
    // Different titles' case, venue names and dedupe keys ...
    assert_eq!(ar.title, "Jenkin Van Zyl: Enclosure");
    assert_eq!(sh.title, "Jenkin van Zyl: Enclosure");
    assert_eq!(ar.venue_name.as_deref(), Some("Somerset House Studios"));
    assert_ne!(ar.dedupe_key, sh.dedupe_key);
    // ... but all three fuzzy gates pass.
    let (a, b) = (MatchInput::from(&ar), MatchInput::from(&sh));
    assert!(matching::dates_compatible(&a, &b));
    assert!(matching::venues_compatible(&a, &b));
    assert!(matching::title_score(&a, &b).is_match);
    assert!(matching::match_score(&a, &b).is_some());
}

async fn merge_case(name: &str, artrabbit_first: bool) {
    let Some(db) = TestDb::create(name).await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let ar_src = repo::source_by_key(&pool, artrabbit::KEY)
        .await
        .unwrap()
        .expect("artrabbit seeded");
    let sh_src = repo::source_by_key(&pool, somerset_house::KEY)
        .await
        .unwrap()
        .unwrap();
    let (ar_raw, ar, sh_raw, sh) = both_jenkins();

    let (first, second) = if artrabbit_first {
        (
            repo::upsert_event(&pool, ar_src.id, &ar, &ar_raw).await,
            repo::upsert_event(&pool, sh_src.id, &sh, &sh_raw).await,
        )
    } else {
        let s = repo::upsert_event(&pool, sh_src.id, &sh, &sh_raw).await;
        let a = repo::upsert_event(&pool, ar_src.id, &ar, &ar_raw).await;
        (s, a)
    };
    let (first, second) = (first.unwrap(), second.unwrap());
    assert!(first.created);
    assert!(!second.created);
    assert_eq!(first.event_id, second.event_id);

    let links: Vec<(i64, serde_json::Value, Option<String>)> = sqlx::query_as(
        "SELECT source_id, raw, source_url FROM events.event_sources
         WHERE event_id = $1 ORDER BY source_id",
    )
    .bind(first.event_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(links.len(), 2);
    let ar_link = links.iter().find(|l| l.0 == ar_src.id).unwrap();
    // Facts + link only: ArtRabbit's raw payload is not kept.
    assert_eq!(ar_link.1, repo::redacted_raw());
    assert_eq!(
        ar_link.2.as_deref(),
        Some("https://www.artrabbit.com/events/jenkin-van-zyl-enclosure")
    );

    // Whichever came first, the venue's own site provides the facts it has
    // (an aggregator never takes precedence).
    let (venue, description, url): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT venue_name, description, url FROM events.events WHERE id = $1")
            .bind(first.event_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(description.is_some());
    assert_eq!(url, sh.url);
    if !artrabbit_first {
        assert_eq!(venue.as_deref(), Some("Somerset House"));
    }
}

#[tokio::test]
async fn venue_first_then_artrabbit_merges() {
    merge_case("artrabbit_venue_first_merge", false).await;
}

#[tokio::test]
async fn artrabbit_first_then_venue_merges() {
    merge_case("artrabbit_first_merge", true).await;
}
