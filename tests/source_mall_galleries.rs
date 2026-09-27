//! Mall Galleries scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::mall_galleries::{
    ListingItem, MallGalleries, parse_detail, parse_listing,
};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/mall-galleries";
const SITE: &str = "https://www.mallgalleries.org.uk";

fn listing() -> Vec<ListingItem> {
    parse_listing(&fixture(&format!("{DIR}/home.html")))
}

fn slug(item: &ListingItem) -> &str {
    item.path.rsplit('/').next().unwrap()
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
    insta::assert_json_snapshot!("mall_galleries_listing", listing());
}

#[test]
fn normalised_output_snapshot() {
    let s = MallGalleries::new(SITE.parse().unwrap());
    let mut out = Vec::new();
    for item in fixture_items() {
        let html = fixture(&format!("{DIR}/detail/{}.html", slug(&item)));
        let url: Url = format!("{SITE}{}", item.path).parse().unwrap();
        let raw = parse_detail(&html, &url, item.label.as_deref()).expect("detail parses");
        out.push(serde_json::json!({
            "slug": slug(&item),
            "payload": raw.payload,
            "event": s.normalise(&raw).expect("normalise"),
        }));
    }
    insta::assert_json_snapshot!("mall_galleries_normalised", out);
}

#[test]
fn news_teasers_are_not_listed() {
    assert!(
        listing()
            .iter()
            .all(|i| i.path.starts_with("/exhibitions-events/"))
    );
    let home = fixture(&format!("{DIR}/home.html"));
    assert!(home.contains("/news/"), "fixture has news teasers");
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
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/home.html"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let items = fixture_items();
    // One detail page fails over HTTP and one returns an unparseable page:
    // both are reported as soft errors, the rest still parse. Listed pages
    // without a fixture get wiremock's 404 and are soft errors too.
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
            ResponseTemplate::new(200)
                .set_body_string("<html><body>not an exhibition</body></html>"),
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
    let s = MallGalleries::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let errors = ctx.take_errors();
    let unfixtured = listing().len() - items.len();
    assert_eq!(errors.len(), 2 + unfixtured, "{errors:?}");
    assert!(
        errors
            .iter()
            .any(|e| e.contains(&format!("{}:", broken.path))),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains(&malformed.path) && e.contains("no page title")),
        "{errors:?}"
    );
    let site = server.uri();
    let mut got: Vec<String> = raws
        .iter()
        .map(|r| {
            assert_eq!(r.payload["url"], r.source_url.clone().unwrap());
            r.source_url.clone().unwrap()
        })
        .collect();
    got.sort();
    let mut want: Vec<String> = ok.iter().map(|i| format!("{site}{}", i.path)).collect();
    want.sort();
    assert_eq!(got, want);
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
    let s = MallGalleries::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // Only news teasers: a template change must reach the health checker
    // instead of producing clean, empty runs.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><article class="o-node o-news-page o-teaser"><a href="/exhibitions-events/old-show">Old show</a></article></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = MallGalleries::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no exhibition links"), "{err}");
}
