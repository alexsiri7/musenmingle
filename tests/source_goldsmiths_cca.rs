//! Goldsmiths CCA scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::goldsmiths_cca::{
    GoldsmithsCca, ListingItem, parse_detail, parse_listing,
};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/goldsmiths-cca";
const SITE: &str = "https://goldsmithscca.art";

fn listing() -> Vec<ListingItem> {
    parse_listing(&fixture(&format!("{DIR}/home.html")))
}

fn scraper() -> GoldsmithsCca {
    GoldsmithsCca::new(SITE.parse().unwrap())
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!("goldsmiths_cca_listing", listing());
}

#[test]
fn normalised_output_snapshot() {
    let s = scraper();
    let mut out = Vec::new();
    // Every homepage exhibition has a saved page.
    for item in listing() {
        let slug = item.slug().to_string();
        let html = fixture(&format!("{DIR}/detail/{slug}.html"));
        let url: Url = format!("{SITE}{}", item.path).parse().unwrap();
        let raw = parse_detail(&html, &url, &item).expect("detail parses");
        out.push(serde_json::json!({
            "slug": slug,
            "payload": raw.payload,
            "event": s.normalise(&raw).expect("normalise"),
        }));
    }
    insta::assert_json_snapshot!("goldsmiths_cca_normalised", out);
}

async fn mount_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_homepage_and_details_via_fetch_context() {
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/home.html"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let items = listing();
    for item in &items {
        Mock::given(method("GET"))
            .and(path(item.path.as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&format!("{DIR}/detail/{}.html", item.slug()))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = GoldsmithsCca::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let site = server.uri();
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    let want: Vec<&str> = items.iter().map(ListingItem::slug).collect();
    assert_eq!(ids, want);
    for raw in &raws {
        let expected = format!("{site}/exhibition/{}/", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(expected.as_str()));
        assert!(s.normalise(raw).unwrap().is_some());
    }
}

#[tokio::test]
async fn failing_detail_pages_are_soft_errors() {
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/home.html"))),
        )
        .mount(&server)
        .await;
    let items = listing();
    let [broken, malformed, ..] = items.as_slice() else {
        panic!("need two homepage exhibitions");
    };
    Mock::given(method("GET"))
        .and(path(broken.path.as_str()))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(malformed.path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body>nope</body></html>"))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = GoldsmithsCca::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert_eq!(raws.len(), items.len() - 2);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors.iter().any(|e| e.contains(&broken.path)),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains(&malformed.path) && e.contains("not an exhibition page")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("User-agent: MuseNMingleBot\nDisallow: /\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = GoldsmithsCca::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A homepage without exhibition blocks (a template change) must reach the
    // health checker instead of producing clean, empty runs.
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><div class="inner events"><section class="event-item"><h2 class="title">No link</h2></section></div></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = GoldsmithsCca::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no exhibitions found"), "{err}");
}
