//! Serpentine Galleries scraper: snapshot tests over saved HTML fixtures and
//! an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::serpentine::{Serpentine, parse_detail, parse_listing};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/serpentine-galleries";

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

#[test]
fn listing_snapshot() {
    let paths = parse_listing(&fixture(&format!("{DIR}/whats-on.html")));
    insta::assert_json_snapshot!("serpentine_listing", paths);
}

#[test]
fn normalised_output_snapshot() {
    let s = Serpentine::new("https://www.serpentinegalleries.org".parse().unwrap());
    let mut out = Vec::new();
    for slug in detail_slugs() {
        let html = fixture(&format!("{DIR}/detail/{slug}.html"));
        let raw = parse_detail(&html, &format!("/whats-on/{slug}/"));
        let event = raw.as_ref().map(|r| s.normalise(r).expect("normalise"));
        out.push(serde_json::json!({
            "slug": slug,
            "source_event_id": raw.as_ref().map(|r| r.source_event_id.clone()),
            "price_text": raw.as_ref().and_then(|r| r.payload.get("price_text").cloned()),
            "event": event,
        }));
    }
    insta::assert_json_snapshot!("serpentine_normalised", out);
}

#[test]
fn every_listing_link_has_a_fixture() {
    // Keeps the fixture set complete when the listing fixture is refreshed.
    let slugs = detail_slugs();
    for p in parse_listing(&fixture(&format!("{DIR}/whats-on.html"))) {
        let slug = p.trim_matches('/').rsplit('/').next().unwrap().to_string();
        assert!(slugs.contains(&slug), "missing detail fixture for {p}");
    }
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
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/whats-on.html"))),
        )
        .mount(&server)
        .await;
    let slugs = detail_slugs();
    // One detail page fails: reported as a soft error, the rest still parse.
    let (broken, ok) = slugs.split_first().unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/whats-on/{broken}/")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    for slug in ok {
        Mock::given(method("GET"))
            .and(path(format!("/whats-on/{slug}/")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&format!("{DIR}/detail/{slug}.html"))),
            )
            .mount(&server)
            .await;
    }

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Serpentine::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains(broken.as_str()));
    let expected = ok
        .iter()
        .filter(|slug| {
            parse_detail(&fixture(&format!("{DIR}/detail/{slug}.html")), "/x/").is_some()
        })
        .count();
    assert!(expected > 5);
    assert_eq!(raws.len(), expected);
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
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = Serpentine::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
