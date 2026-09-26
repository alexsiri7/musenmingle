//! Whitechapel Gallery scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::whitechapel_gallery::{WhitechapelGallery, parse_detail, parse_listing};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/whitechapel-gallery";

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

fn listing_paths() -> Vec<String> {
    parse_listing(&fixture(&format!("{DIR}/exhibitions.html")))
}

const SITE: &str = "https://www.whitechapelgallery.org";

fn scraper() -> WhitechapelGallery {
    WhitechapelGallery::new(SITE.parse().unwrap())
}

fn detail_url(slug: &str) -> Url {
    format!("{SITE}/exhibitions/{slug}/").parse().unwrap()
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!("whitechapel_gallery_listing", listing_paths());
}

#[test]
fn normalised_output_snapshot() {
    let s = scraper();
    let mut out = Vec::new();
    for slug in detail_slugs() {
        let html = fixture(&format!("{DIR}/detail/{slug}.html"));
        let raw = parse_detail(&html, &detail_url(&slug)).expect("detail parses");
        out.push(serde_json::json!({
            "slug": slug,
            "payload": raw.payload,
            "event": s.normalise(&raw).expect("normalise"),
        }));
    }
    insta::assert_json_snapshot!("whitechapel_gallery_normalised", out);
}

#[test]
fn every_listing_link_has_a_fixture() {
    // Keeps the fixture set complete when the listing fixture is refreshed.
    let slugs = detail_slugs();
    for p in listing_paths() {
        let slug = p
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap()
            .to_string();
        assert!(slugs.contains(&slug), "missing detail fixture for {p}");
    }
}

#[test]
fn past_exhibition_page_is_skipped() {
    let html = fixture(&format!("{DIR}/past/common-rooms-displays.html"));
    let raw = parse_detail(&html, &detail_url("common-rooms-displays")).expect("detail parses");
    assert!(raw.payload["date_text"].is_null(), "{}", raw.payload);
    assert!(scraper().normalise(&raw).expect("normalise").is_none());
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
        .and(path("/exhibitions/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/exhibitions.html"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let slugs = detail_slugs();
    // One detail page fails over HTTP and one returns an unparseable page:
    // both are reported as soft errors, the rest still parse.
    let [broken, malformed, ok @ ..] = slugs.as_slice() else {
        panic!("need at least three detail fixtures");
    };
    let broken_path = format!("/exhibitions/{broken}/");
    let malformed_path = format!("/exhibitions/{malformed}/");
    Mock::given(method("GET"))
        .and(path(broken_path.as_str()))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(malformed_path.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>not an exhibition page</body></html>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    for slug in ok {
        Mock::given(method("GET"))
            .and(path(format!("/exhibitions/{slug}/")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&format!("{DIR}/detail/{slug}.html"))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WhitechapelGallery::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    // Full paths: the broken slug is a prefix of the malformed one.
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
        let expected = format!("{site}/exhibitions/{}/", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(expected.as_str()));
        assert_eq!(raw.payload["url"], expected);
    }
    let mut ids: Vec<String> = raws.into_iter().map(|r| r.source_event_id).collect();
    ids.sort();
    assert_eq!(&ids, ok);
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
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WhitechapelGallery::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // Only past-exhibition cards: a template change must reach the health
    // checker instead of producing clean, empty runs.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/exhibitions/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><div class="packeryItem"><a href="/exhibitions/old-show/">Old show</a></div></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = WhitechapelGallery::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no exhibition links"), "{err}");
}
