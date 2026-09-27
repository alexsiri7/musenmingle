//! Ibraaz scraper: snapshot tests over the saved What's On and event pages,
//! and end-to-end fetches against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::ibraaz::{Ibraaz, parse_detail, parse_listing};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/ibraaz";
const SITE: &str = "https://ibraaz.org";
/// Listing order, as on `/whats-on/`.
const SLUGS: [&str; 12] = [
    "cosmic-breath-joe-namy",
    "taring-padi",
    "who-is-your-we",
    "colonial-sensitivities",
    "earshot-library-in-residence",
    "elegy-gabrielle-goliath",
    "autumn-opening",
    "ibraaz-one-year",
    "global-majority-as-idea-and-practice",
    "another-kind-of-relation",
    "after-the-lecture-money",
    "eden-at-dawn",
];
/// The event pages saved as fixtures.
const DETAILS: [&str; 7] = [
    "cosmic-breath-joe-namy",
    "taring-padi",
    "who-is-your-we",
    "colonial-sensitivities",
    "elegy-gabrielle-goliath",
    "autumn-opening",
    "eden-at-dawn",
];

fn detail(slug: &str) -> String {
    fixture(&format!("{DIR}/detail/{slug}.html"))
}

fn slug(raw: &RawEvent) -> &str {
    raw.payload["slug"].as_str().unwrap()
}

fn raws() -> Vec<RawEvent> {
    let (mut raws, problems) = parse_listing(
        &fixture(&format!("{DIR}/whats-on.html")),
        &SITE.parse().unwrap(),
    )
    .expect("calendar");
    assert!(problems.is_empty(), "{problems:?}");
    for raw in &mut raws {
        let slug = slug(raw).to_string();
        if DETAILS.contains(&slug.as_str()) {
            raw.payload["description"] = json!(parse_detail(&detail(&slug), &slug).unwrap());
        }
    }
    raws
}

#[test]
fn listing_has_every_current_and_forthcoming_event() {
    let raws = raws();
    let slugs: Vec<&str> = raws.iter().map(slug).collect();
    assert_eq!(slugs, SLUGS);
    assert_eq!(
        raws[2].source_url.as_deref(),
        Some("https://ibraaz.org/whats-on/who-is-your-we")
    );
}

#[test]
fn raw_snapshot() {
    insta::assert_json_snapshot!("ibraaz_raw", raws());
}

#[test]
fn normalised_output_snapshot() {
    let s = Ibraaz::new(SITE.parse().unwrap());
    let out: Vec<_> = raws()
        .iter()
        .map(|raw| {
            json!({
                "id": raw.source_event_id,
                "event": s.normalise(raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("ibraaz_normalised", out);
}

#[test]
fn event_page_without_the_event_is_an_error() {
    let err = parse_detail(&detail("taring-padi"), "who-is-your-we").unwrap_err();
    assert!(err.contains("no event \"who-is-your-we\""), "{err}");
}

async fn mount(server: &MockServer, at: &str, body: String, times: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(times)
        .mount(server)
        .await;
}

/// The site with only the saved event pages; the others answer 404.
async fn mount_site(server: &MockServer, detail_fetches: usize) {
    mount(
        server,
        "/robots.txt",
        fixture(&format!("{DIR}/robots.txt")),
        1,
    )
    .await;
    mount(
        server,
        "/whats-on/",
        fixture(&format!("{DIR}/whats-on.html")),
        1,
    )
    .await;
    for slug in DETAILS {
        let position = SLUGS.iter().position(|s| *s == slug).unwrap();
        mount(
            server,
            &format!("/whats-on/{slug}"),
            detail(slug),
            u64::from(position < detail_fetches),
        )
        .await;
    }
}

#[tokio::test]
async fn fetches_listing_and_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    mount_site(&server, SLUGS.len()).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Ibraaz::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    // An event whose page fails keeps its facts, without a description.
    let unsaved: Vec<&str> = SLUGS.into_iter().filter(|s| !DETAILS.contains(s)).collect();
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), unsaved.len(), "{errors:?}");
    for (error, slug) in errors.iter().zip(&unsaved) {
        assert!(
            error.starts_with(&format!("/whats-on/{slug}: HTTP 404")),
            "{error}"
        );
    }
    let slugs: Vec<&str> = raws.iter().map(slug).collect();
    assert_eq!(slugs, SLUGS);
    for raw in &raws {
        let described = raw.payload["description"].is_string();
        assert_eq!(described, DETAILS.contains(&slug(raw)), "{}", slug(raw));
        s.normalise(raw).expect("normalise");
    }
    assert_eq!(
        raws[2].source_url,
        Some(format!("{}/whats-on/who-is-your-we", server.uri()))
    );
}

#[tokio::test]
async fn event_page_fetches_are_capped() {
    let server = MockServer::start().await;
    mount_site(&server, 2).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Ibraaz::new(server.uri().parse().unwrap()).with_max_detail_pages(2);
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    assert_eq!(raws.len(), SLUGS.len());
    let described: Vec<&str> = raws
        .iter()
        .filter(|r| r.payload["description"].is_string())
        .map(slug)
        .collect();
    assert_eq!(described, SLUGS[..2]);
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/robots.txt",
        "User-agent: MuseNMingleBot\nDisallow: /\n".into(),
        1,
    )
    .await;
    mount(&server, "/whats-on/", "should never be fetched".into(), 0).await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Ibraaz::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn listing_without_the_calendar_is_an_error() {
    // A page without the calendar payload (a template change) must reach
    // the health checker instead of producing clean, empty runs.
    let server = MockServer::start().await;
    mount(
        &server,
        "/robots.txt",
        fixture(&format!("{DIR}/robots.txt")),
        1,
    )
    .await;
    mount(
        &server,
        "/whats-on/",
        "<html><body><h1>What’s On</h1></body></html>".into(),
        1,
    )
    .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Ibraaz::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("/whats-on/: no Nuxt payload"), "{err}");
}
