//! Garden Museum scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::garden_museum::{GardenMuseum, ListingItem, parse_detail, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/garden-museum";
const SITE: &str = "https://www.gardenmuseum.org.uk";

fn detail_slugs() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(DIR)
        .join("detail");
    let mut slugs: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            e.unwrap()
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    slugs.sort();
    slugs
}

fn listing() -> Vec<ListingItem> {
    parse_listing(&fixture(&format!("{DIR}/whats-on.html"))).expect("listing parses")
}

fn scraper() -> GardenMuseum {
    GardenMuseum::new(SITE.parse().unwrap())
}

/// The real listing page with its `json-data` cut down to the items that
/// have a saved event page (the full listing has 57 items).
fn trimmed_listing_html() -> String {
    let html = fixture(&format!("{DIR}/whats-on.html"));
    let open = r#"<script id="json-data" type="application/json">"#;
    let start = html.find(open).expect("json-data script") + open.len();
    let end = start + html[start..].find("</script>").unwrap();
    let data: Vec<serde_json::Value> = serde_json::from_str(html[start..end].trim()).unwrap();
    let slugs = detail_slugs();
    let kept: Vec<&serde_json::Value> = data
        .iter()
        .filter(|item| {
            let link = item["link"].as_str().unwrap();
            slugs
                .iter()
                .any(|s| link.ends_with(&format!("/whats-on/{s}/")))
        })
        .collect();
    assert_eq!(kept.len(), slugs.len());
    format!(
        "{}{}{}",
        &html[..start],
        serde_json::to_string(&kept).unwrap(),
        &html[end..]
    )
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!("garden_museum_listing", listing());
}

#[test]
fn normalised_output_snapshot() {
    let s = scraper();
    let items = listing();
    let mut out = Vec::new();
    for slug in detail_slugs() {
        let item = items
            .iter()
            .find(|i| i.slug() == slug)
            .unwrap_or_else(|| panic!("{slug} not in the listing"));
        let html = fixture(&format!("{DIR}/detail/{slug}.html"));
        let url: Url = format!("{SITE}{}", item.path).parse().unwrap();
        let raw = parse_detail(&html, &url, item).expect("detail parses");
        out.push(serde_json::json!({
            "slug": slug,
            "payload": raw.payload,
            "event": s.normalise(&raw).expect("normalise"),
        }));
    }
    insta::assert_json_snapshot!("garden_museum_normalised", out);
}

#[tokio::test]
async fn fetches_listing_and_details_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(trimmed_listing_html()))
        .expect(1)
        .mount(&server)
        .await;
    let slugs = detail_slugs();
    // One page fails over HTTP and one is unparseable: both are reported as
    // soft errors, the rest still parse.
    let [broken, malformed, ok @ ..] = slugs.as_slice() else {
        panic!("need at least three detail fixtures");
    };
    let broken_path = format!("/whats-on/{broken}/");
    let malformed_path = format!("/whats-on/{malformed}/");
    Mock::given(method("GET"))
        .and(path(broken_path.as_str()))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(malformed_path.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>not an event page</body></html>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    for slug in ok {
        Mock::given(method("GET"))
            .and(path(format!("/whats-on/{slug}/")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&format!("{DIR}/detail/{slug}.html"))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = GardenMuseum::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors.iter().any(|e| e.contains(&broken_path)),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains(&malformed_path) && e.contains("no page title")),
        "{errors:?}"
    );
    let site = server.uri();
    for raw in &raws {
        let expected = format!("{site}/whats-on/{}/", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(expected.as_str()));
        assert_eq!(raw.payload["url"], expected);
    }
    let mut ids: Vec<String> = raws.into_iter().map(|r| r.source_event_id).collect();
    ids.sort();
    assert_eq!(&ids, ok);
}

#[tokio::test]
async fn detail_fetches_are_capped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/whats-on.html"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    // Only the first two in-scope listing items are requested.
    let first: Vec<String> = listing().into_iter().take(2).map(|i| i.path).collect();
    for p in &first {
        Mock::given(method("GET"))
            .and(path(p.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<html><body><h1 class="page--header__title">X</h1></body></html>"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
    }
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = GardenMuseum::new(server.uri().parse().unwrap()).with_max_detail_pages(2);
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert_eq!(raws.len(), 2);
    assert!(ctx.take_errors().is_empty());
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /whats-on\n"),
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
    let s = GardenMuseum::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A page without the json-data script (a template change) must reach the
    // health checker instead of producing clean, empty runs.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><section class="whats-on--feed"></section></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = GardenMuseum::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no events found"), "{err}");
}
