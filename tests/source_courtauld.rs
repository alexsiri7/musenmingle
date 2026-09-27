//! Courtauld scraper: snapshot tests over saved JSON/HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::courtauld::{
    Courtauld, ListingItem, parse_detail, parse_ids, parse_rest,
};
use serde_json::Value;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/courtauld";
const SITE: &str = "https://courtauld.ac.uk";

fn json_fixture(name: &str) -> Value {
    serde_json::from_str(&fixture(&format!("{DIR}/{name}"))).unwrap()
}

fn listing() -> Vec<ListingItem> {
    let ids = parse_ids(&json_fixture("fetch-post-data.json")).unwrap();
    parse_rest(&json_fixture("events.json"), &ids).unwrap()
}

fn slug(item: &ListingItem) -> &str {
    item.path.trim_matches('/').rsplit('/').next().unwrap()
}

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

/// The listing items that have a saved detail page.
fn fixture_items() -> Vec<ListingItem> {
    let slugs = detail_slugs();
    let items: Vec<ListingItem> = listing()
        .into_iter()
        .filter(|i| slugs.iter().any(|s| s == slug(i)))
        .collect();
    assert_eq!(items.len(), slugs.len(), "every detail fixture is listed");
    items
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!("courtauld_listing", listing());
}

#[test]
fn normalised_output_snapshot() {
    let s = Courtauld::new(SITE.parse().unwrap());
    let mut out = Vec::new();
    for item in fixture_items() {
        let slug = slug(&item).to_string();
        let html = fixture(&format!("{DIR}/detail/{slug}.html"));
        let url: Url = format!("{SITE}{}", item.path).parse().unwrap();
        let raw = parse_detail(&html, &url, &item).expect("detail parses");
        out.push(serde_json::json!({
            "slug": slug,
            "source_event_id": raw.source_event_id,
            "payload": raw.payload,
            "event": s.normalise(&raw).expect("normalise"),
        }));
    }
    insta::assert_json_snapshot!("courtauld_normalised", out);
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
async fn fetches_listing_and_details_via_fetch_context() {
    let server = MockServer::start().await;
    mount_robots(&server).await;
    let items = fixture_items();
    // Serve only the programme items that have detail fixtures, plus one
    // online course (filtered out before any detail request).
    let online = listing_raw_item("holbein-at-the-court-of-henry-viii-3");
    let mut ids: Vec<u64> = items.iter().map(|i| i.id).collect();
    ids.push(online["id"].as_u64().unwrap());
    let rest: Vec<Value> = json_fixture("events.json")
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| ids.contains(&e["id"].as_u64().unwrap()))
        .cloned()
        .collect();
    Mock::given(method("GET"))
        .and(path("/wp-json/unt/v1/fetch-post-data"))
        .and(query_param("listing_type", "whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&ids))
        .expect(1)
        .mount(&server)
        .await;
    let include: Vec<String> = ids.iter().map(u64::to_string).collect();
    Mock::given(method("GET"))
        .and(path("/wp-json/wp/v2/events"))
        .and(query_param("include", include.join(",").as_str()))
        .and(query_param("_fields", "id,link,class_list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&rest))
        .expect(1)
        .mount(&server)
        .await;
    // One detail page fails over HTTP and one returns an unparseable page:
    // both are reported as soft errors, the rest still parse.
    let [broken, malformed, ok @ ..] = items.as_slice() else {
        panic!("need at least three detail fixtures");
    };
    Mock::given(method("GET"))
        .and(path(broken.path.as_str()))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(malformed.path.as_str()))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html><body>not an event</body></html>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    for item in ok {
        Mock::given(method("GET"))
            .and(path(item.path.as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&format!("{DIR}/detail/{}.html", slug(item)))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Courtauld::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors.iter().any(|e| e.contains(&broken.path)),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains(&malformed.path) && e.contains("no page title")),
        "{errors:?}"
    );
    // Links from the REST API are rebased onto the configured base URL.
    let site = server.uri();
    let mut got: Vec<(String, String)> = raws
        .iter()
        .map(|r| (r.source_event_id.clone(), r.source_url.clone().unwrap()))
        .collect();
    got.sort();
    let mut want: Vec<(String, String)> = ok
        .iter()
        .map(|i| (i.id.to_string(), format!("{site}{}", i.path)))
        .collect();
    want.sort();
    assert_eq!(got, want);
}

fn listing_raw_item(slug: &str) -> Value {
    json_fixture("events.json")
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["link"].as_str().unwrap().contains(slug))
        .cloned()
        .unwrap()
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
        .and(path("/wp-json/unt/v1/fetch-post-data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[1]"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Courtauld::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_programme_is_an_error() {
    // An empty id list must reach the health checker instead of producing
    // clean, empty runs.
    let server = MockServer::start().await;
    mount_robots(&server).await;
    Mock::given(method("GET"))
        .and(path("/wp-json/unt/v1/fetch-post-data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/wp-json/wp/v2/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Courtauld::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("programme id list is empty"), "{err}");
}
