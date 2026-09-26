//! William Morris Gallery scraper: snapshot tests over saved HTML fixtures
//! and an end-to-end fetch against wiremock.
//!
//! The listing is saved in full with the detail page of every card on it,
//! plus one real response of the "Load more" endpoint (`filter.json`).

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::Category;
use musenmingle::sources::Source;
use musenmingle::sources::william_morris_gallery::{
    GRID_PAGE, MAX_DETAIL_PAGES, MAX_MORE_PAGES, WilliamMorrisGallery, detail_event, parse_detail,
    parse_listing, parse_more,
};
use rust_decimal::Decimal;
use url::Url;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/william-morris-gallery";
const SITE: &str = "https://www.wmgallery.org.uk";
const FILTER_PATH: &str = "/wp-json/williammorris/v1/filter";

fn slug(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap()
}

fn detail_html(slug: &str) -> String {
    fixture(&format!("{DIR}/detail/{slug}.html"))
}

fn listing_paths() -> Vec<String> {
    let listing = parse_listing(&fixture(&format!("{DIR}/whats-on.html")));
    assert!(listing.rejected.is_empty(), "{:?}", listing.rejected);
    listing.paths
}

async fn serve_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
}

async fn serve(server: &MockServer, at: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

#[test]
fn listing_snapshot() {
    let listing = parse_listing(&fixture(&format!("{DIR}/whats-on.html")));
    assert_eq!(listing.paths.len(), 4);
    assert_eq!(listing.grid_cards, 3);
    let (more, rejected) = parse_more(&fixture(&format!("{DIR}/filter.json"))).unwrap();
    assert!(rejected.is_empty());
    insta::assert_json_snapshot!(
        "william_morris_gallery_listing",
        serde_json::json!({ "listing": listing, "load_more": more })
    );
}

#[test]
fn no_more_response_has_no_cards() {
    let (more, rejected) =
        parse_more(r#""<article class=\"no_more\">No more items to see<\/article>""#).unwrap();
    assert!(more.is_empty() && rejected.is_empty());
    assert!(parse_more("<html>").is_err());
}

#[test]
fn normalised_output_snapshot() {
    let s = WilliamMorrisGallery::new(SITE.parse().unwrap());
    let site: Url = SITE.parse().unwrap();
    let out: Vec<serde_json::Value> = listing_paths()
        .iter()
        .map(|p| {
            let detail = parse_detail(&detail_html(slug(p))).expect("detail");
            let raw = detail_event(p, &site.join(p).unwrap(), &detail);
            serde_json::json!({
                "id": raw.source_event_id,
                "payload": raw.payload,
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("william_morris_gallery_normalised", out);
}

#[tokio::test]
async fn fetches_listing_and_details_via_fetch_context() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve(
        &server,
        "/whats-on/",
        fixture(&format!("{DIR}/whats-on.html")),
    )
    .await;
    // Three grid cards: nothing to load more.
    Mock::given(method("GET"))
        .and(path(FILTER_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let paths = listing_paths();
    for p in &paths {
        serve(&server, p, detail_html(slug(p))).await;
    }

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WilliamMorrisGallery::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    let errors = ctx.take_errors();
    assert!(errors.is_empty(), "{errors:?}");
    let got: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    let expected: Vec<&str> = paths.iter().map(|p| p.trim_matches('/')).collect();
    assert_eq!(got, expected);
    for raw in &raws {
        let url = format!("{}/{}/", server.uri(), raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(url.as_str()));
    }
    let family_day = raws
        .iter()
        .find(|r| r.source_event_id == "event/family-day-welcome-home")
        .unwrap();
    let event = s.normalise(family_day).unwrap().unwrap();
    assert_eq!(event.category, Category::Workshop);
    assert_eq!(event.venue_name.as_deref(), Some("William Morris Gallery"));
    assert_eq!(event.starts_at.to_rfc3339(), "2026-10-10T12:00:00+00:00");
    assert!(event.price.is_free);
    assert_eq!(event.price.max, Some(Decimal::ZERO));
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /whats-on/\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WilliamMorrisGallery::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A template change must reach the health checker instead of producing
    // clean, empty runs.
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve(
        &server,
        "/whats-on/",
        "<html><body></body></html>".to_string(),
    )
    .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WilliamMorrisGallery::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event cards"), "{err}");
}

fn card(slug: &str) -> String {
    format!(
        r#"<a href="https://www.wmgallery.org.uk/event/{slug}/" class="flex_item card card--grid"><h3>{slug}</h3></a>"#
    )
}

fn more_body(cards: &str) -> String {
    serde_json::to_string(cards).unwrap()
}

fn full_listing() -> String {
    let cards: String = (0..GRID_PAGE).map(|i| card(&format!("talk-{i}"))).collect();
    format!(
        r#"<html><body><form id="filter_form" data-id="364"></form>
        <div class="event_grid" id="response">{cards}</div></body></html>"#
    )
}

#[tokio::test]
async fn full_grid_loads_more_until_a_batch_is_empty() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve(&server, "/whats-on/", full_listing()).await;
    // The first batch is the endpoint's real response (three cards).
    Mock::given(method("GET"))
        .and(path(FILTER_PATH))
        .and(query_param("id", "364"))
        .and(query_param("offset", "10"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/filter.json"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(FILTER_PATH))
        .and(query_param("offset", "13"))
        .respond_with(ResponseTemplate::new(200).set_body_string(more_body(
            r#"<article class="no_more">No more items to see</article>"#,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/event/"))
        .respond_with(ResponseTemplate::new(404))
        .expect(13)
        .mount(&server)
        .await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WilliamMorrisGallery::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(raws.is_empty());
    // Every detail page was a 404: one soft error each.
    assert_eq!(ctx.take_errors().len(), 13);
}

#[tokio::test]
async fn load_more_and_detail_fetches_stop_at_their_caps() {
    const PER_BATCH: usize = 9;
    const { assert!(GRID_PAGE + MAX_MORE_PAGES * PER_BATCH > MAX_DETAIL_PAGES) };
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve(&server, "/whats-on/", full_listing()).await;
    for batch in 0..=MAX_MORE_PAGES {
        let offset = GRID_PAGE + batch * PER_BATCH;
        let cards: String = (0..PER_BATCH)
            .map(|i| card(&format!("more-{batch}-{i}")))
            .collect();
        Mock::given(method("GET"))
            .and(path(FILTER_PATH))
            .and(query_param("offset", offset.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_string(more_body(&cards)))
            .expect(u64::from(batch < MAX_MORE_PAGES))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path_regex("^/event/"))
        .respond_with(ResponseTemplate::new(404))
        .expect(MAX_DETAIL_PAGES as u64)
        .mount(&server)
        .await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WilliamMorrisGallery::new(server.uri().parse().unwrap());
    s.fetch(&ctx).await.expect("fetch");
    assert_eq!(ctx.take_errors().len(), MAX_DETAIL_PAGES);
}

#[tokio::test]
async fn full_grid_without_a_page_id_is_reported() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    let listing = full_listing().replace(r#" data-id="364""#, "");
    serve(&server, "/whats-on/", listing).await;
    Mock::given(method("GET"))
        .and(path(FILTER_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/event/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .expect(GRID_PAGE as u64)
        .mount(&server)
        .await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WilliamMorrisGallery::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(raws.is_empty());
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1 + GRID_PAGE, "{errors:?}");
    assert!(errors[0].contains("no page id"), "{errors:?}");
    assert!(errors[1].contains("without a title"), "{errors:?}");
}
