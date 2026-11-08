//! The English National Opera scraper: snapshot tests over saved HTML
//! fixtures and an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::sources::Source;
use musenmingle::sources::english_national_opera::{
    EnglishNationalOpera, parse_detail, parse_listing, raw_event,
};
use reqwest::StatusCode;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/english-national-opera";
const SITE: &str = "https://www.eno.org";

/// The listing's events on 2026-11-08: the two featured, then the grid,
/// less the Relaxed Programme.
const SLUGS: &[&str] = &[
    "tosca",
    "adriana-mater",
    "iphigenie-en-tauride",
    "sessions-at-the-london-coliseum-black-country-new-road",
    "to-die-for-a-comedy",
    "la-traviata",
    "abracadopera",
    "breaking-the-waves",
    "trial-by-jury-the-zoo",
    "einstein-on-the-beach",
];

fn page(slug: &str) -> String {
    fixture(&format!("{DIR}/{slug}.html"))
}

fn listing() -> String {
    fixture(&format!("{DIR}/whats-on.html"))
}

#[test]
fn a_missing_robots_txt_allows_the_events_pages() {
    let robots = RobotsPolicy::from_response(
        StatusCode::NOT_FOUND,
        fixture(&format!("{DIR}/robots-404.html")).as_bytes(),
    );
    for p in ["/whats-on/", "/events/tosca/"] {
        assert!(
            robots.allowed(&Url::parse(SITE).unwrap().join(p).unwrap()),
            "{p}"
        );
    }
}

#[test]
fn the_listing_links_every_event_page_once() {
    let url = Url::parse(&format!("{SITE}/whats-on/")).unwrap();
    assert_eq!(parse_listing(&listing(), &url).unwrap(), SLUGS);
    assert_eq!(parse_listing("<html><body></body></html>", &url), None);
}

#[test]
fn normalised_output_snapshot() {
    let s = EnglishNationalOpera::new(SITE.parse().unwrap());
    let raws = SLUGS
        .iter()
        .map(|slug| {
            let url = Url::parse(&format!("{SITE}/events/{slug}/")).unwrap();
            let detail = parse_detail(&page(slug))
                .expect("readable")
                .expect("event page");
            raw_event(slug, &url, detail)
        })
        .collect::<Vec<_>>();
    let out: Vec<_> = raws
        .iter()
        .map(|raw| match s.normalise(raw) {
            Ok(event) => serde_json::json!({
                "source_event_id": raw.source_event_id,
                "venue": raw.payload["detail"]["venue"],
                "running_time": raw.payload["detail"]["running_time"],
                "tickets": raw.payload["detail"]["tickets"],
                "event": event,
            }),
            Err(e) => serde_json::json!({
                "source_event_id": raw.source_event_id,
                "error": e.to_string(),
            }),
        })
        .collect();
    insta::assert_json_snapshot!("english_national_opera_normalised", out);
}

async fn serve(server: &MockServer, at: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

async fn serve_robots_404(server: &MockServer) {
    let body = fixture(&format!("{DIR}/robots-404.html"));
    serve(
        server,
        "/robots.txt",
        ResponseTemplate::new(404).set_body_string(body),
    )
    .await;
}

#[tokio::test]
async fn fetches_the_listing_and_its_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    serve_robots_404(&server).await;
    serve(
        &server,
        "/whats-on/",
        ResponseTemplate::new(200).set_body_string(listing()),
    )
    .await;
    for slug in SLUGS {
        serve(
            &server,
            &format!("/events/{slug}/"),
            ResponseTemplate::new(200).set_body_string(page(slug)),
        )
        .await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = EnglishNationalOpera::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, SLUGS);
    // The saved listing links the live site; pages are read from its own
    // origin.
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(format!("{}/events/tosca/", server.uri()).as_str())
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2 + SLUGS.len()
    );
}

#[tokio::test]
async fn caps_event_pages_and_reports_unreadable_ones() {
    let server = MockServer::start().await;
    serve_robots_404(&server).await;
    serve(
        &server,
        "/whats-on/",
        ResponseTemplate::new(200).set_body_string(listing()),
    )
    .await;
    serve(
        &server,
        "/events/tosca/",
        ResponseTemplate::new(200).set_body_string(page("tosca")),
    )
    .await;
    serve(
        &server,
        "/events/adriana-mater/",
        ResponseTemplate::new(200).set_body_string("<html></html>"),
    )
    .await;
    serve(
        &server,
        "/events/iphigenie-en-tauride/",
        ResponseTemplate::new(500),
    )
    .await;
    let broken_booking_data = page("tosca").replace(
        r#"<script id="json-data" class="">"#,
        r#"<script id="json-data" class="">[{"#,
    );
    serve(
        &server,
        "/events/sessions-at-the-london-coliseum-black-country-new-road/",
        ResponseTemplate::new(200).set_body_string(broken_booking_data),
    )
    .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = EnglishNationalOpera::new(server.uri().parse().unwrap())
        .with_max_details(4)
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 1);
    assert_eq!(raws[0].source_event_id, "tosca");
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 3 + SLUGS.len() - 4, "{errors:?}");
    assert!(
        errors[0].contains("adriana-mater: not an event page"),
        "{errors:?}"
    );
    assert!(errors[1].starts_with("iphigenie-en-tauride:"), "{errors:?}");
    assert!(
        errors[2].starts_with(
            "sessions-at-the-london-coliseum-black-country-new-road: parse error: booking json-data"
        ),
        "{errors:?}"
    );
    assert!(
        errors[3].contains("over the 4 event-page cap"),
        "{errors:?}"
    );
}

#[tokio::test]
async fn a_page_that_is_not_the_listing_is_an_error() {
    let server = MockServer::start().await;
    serve_robots_404(&server).await;
    serve(
        &server,
        "/whats-on/",
        ResponseTemplate::new(200).set_body_string("<html><body></body></html>"),
    )
    .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = EnglishNationalOpera::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no What's On listing"), "{err}");
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
    let err = EnglishNationalOpera::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
