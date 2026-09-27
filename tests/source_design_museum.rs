//! Design Museum scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::design_museum::{DesignMuseum, parse_detail, parse_listing};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/design-museum";
const LISTINGS: &[(&str, &str)] = &[
    ("/exhibitions", "exhibitions.html"),
    (
        "/exhibitions/future-exhibitions-and-displays",
        "future-exhibitions-and-displays.html",
    ),
];

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

fn listing_paths() -> Vec<Vec<String>> {
    LISTINGS
        .iter()
        .map(|(_, file)| parse_listing(&fixture(&format!("{DIR}/{file}"))))
        .collect()
}

#[test]
fn listing_snapshot() {
    insta::assert_json_snapshot!("design_museum_listing", listing_paths());
}

#[test]
fn normalised_output_snapshot() {
    let s = DesignMuseum::new("https://designmuseum.org".parse().unwrap());
    let mut out = Vec::new();
    for slug in detail_slugs() {
        let html = fixture(&format!("{DIR}/detail/{slug}.html"));
        let raw = parse_detail(&html, &format!("/exhibitions/{slug}")).expect("detail parses");
        out.push(serde_json::json!({
            "slug": slug,
            "payload": raw.payload,
            "event": s.normalise(&raw).expect("normalise"),
        }));
    }
    insta::assert_json_snapshot!("design_museum_normalised", out);
}

#[test]
fn every_listing_link_has_a_fixture() {
    // Keeps the fixture set complete when a listing fixture is refreshed.
    let slugs = detail_slugs();
    for p in listing_paths().concat() {
        let slug = p.rsplit('/').next().unwrap().to_string();
        assert!(slugs.contains(&slug), "missing detail fixture for {p}");
    }
}

#[tokio::test]
async fn fetches_listings_and_details_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(&server)
        .await;
    for (p, file) in LISTINGS {
        Mock::given(method("GET"))
            .and(path(*p))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/{file}"))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let slugs = detail_slugs();
    // One detail page fails over HTTP and one returns an unparseable page:
    // both are reported as soft errors, the rest still parse.
    let [broken, malformed, ok @ ..] = slugs.as_slice() else {
        panic!("need at least three detail fixtures");
    };
    Mock::given(method("GET"))
        .and(path(format!("/exhibitions/{broken}")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/exhibitions/{malformed}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>not an exhibition page</body></html>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    for slug in ok {
        Mock::given(method("GET"))
            .and(path(format!("/exhibitions/{slug}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&format!("{DIR}/detail/{slug}.html"))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = DesignMuseum::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors.iter().any(|e| e.contains(broken.as_str())),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains(malformed.as_str()) && e.contains("no page title")),
        "{errors:?}"
    );
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
        .and(path("/exhibitions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = DesignMuseum::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
