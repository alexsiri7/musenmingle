//! V&A scraper: snapshot tests over the saved `/whatson` fixture and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::vam::{Vam, parse_listing};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/vam";

fn source() -> Vam {
    Vam::new("https://www.vam.ac.uk".parse().unwrap())
}

#[test]
fn listing_snapshot() {
    let raws = parse_listing(&fixture(&format!("{DIR}/whatson.html")));
    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    insta::assert_json_snapshot!("vam_listing", ids);
}

#[test]
fn normalised_output_snapshot() {
    let s = source();
    let out: Vec<_> = parse_listing(&fixture(&format!("{DIR}/whatson.html")))
        .into_iter()
        .map(|raw| {
            serde_json::json!({
                "id": raw.source_event_id,
                "type": raw.payload["type"],
                "venue": raw.payload["venue"],
                "date_text": raw.payload["date_text"],
                "event": s.normalise(&raw).expect("normalise"),
            })
        })
        .collect();
    insta::assert_json_snapshot!("vam_normalised", out);
}

#[test]
fn timed_events_match_the_event_pages() {
    // Checked by hand against the event pages on 2026-09-27.
    let s = source();
    let raws = parse_listing(&fixture(&format!("{DIR}/whatson.html")));
    let find = |id: &str| {
        let raw = raws
            .iter()
            .find(|r| r.source_event_id == id)
            .expect("in fixture");
        s.normalise(raw).unwrap().expect("kept")
    };
    // "Thursday, 1 October 2026 13.00 – 14.00"
    let e = find("event/eqa5mnwxGM/lunchtime-lecture-1-october-2026");
    assert_eq!(e.starts_at.to_rfc3339(), "2026-10-01T12:00:00+00:00");
    assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-01T13:00:00+00:00");
    assert!(!e.all_day);
    assert!(e.price.is_free);
    // "Talk 19:00 - 20:00, followed by wine reception; closing 20:45"
    let e = find("event/vqDb3EY1O2e/sandy-powell-and-annie-symons");
    assert_eq!(e.starts_at.to_rfc3339(), "2026-09-28T18:00:00+00:00");
    assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-09-28T19:45:00+00:00");
    // "Closes Sunday, 18 October 2026": date-only, all day, inclusive end.
    let e = find("festival/2026/london-design-festival-2026");
    assert!(e.all_day);
    assert_eq!(e.starts_at.to_rfc3339(), "2026-09-11T23:00:00+00:00");
    assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-17T23:00:00+00:00");
}

/// Issue #261: the scraper check of 2026-09-29 found On the Sly's start
/// wrong (the microdata says 28 June; the card, like the rest of V&A East's
/// opening season, says 18 April) and ten listings "missed" that are skipped
/// on purpose (`qa_scope`). Expected values are the page's.
#[test]
fn qa_2026_09_29_page_values() {
    const SKIPPED: [&str; 10] = [
        "Digital Art Season: On Agency",
        "Taster Lecture Recording: Aphrodite and the Greek Art of Love",
        "Antique to Early Christian (in-person)",
        "Arts of the Islamic World (In-Person)",
        "National Art Library and Archive talks Monday at 14.00",
        "V&A East Educators Connect: The Music is Black",
        "Mini Play for Babies - Autumn Series",
        "African & Caribbean Heritage tour: Past Meets Present",
        "Members' Welcome Tour",
        "Objects of Fashion: Unravelling Stories (Online)",
    ];
    let s = source();
    let raws = parse_listing(&fixture(&format!("{DIR}/whatson.html")));
    for title in SKIPPED {
        let raw = raws
            .iter()
            .find(|r| r.payload["title"] == title)
            .unwrap_or_else(|| panic!("{title} not listed"));
        assert!(s.normalise(raw).expect("normalise").is_none(), "{title}");
    }

    let raw = raws
        .iter()
        .find(|r| r.source_event_id == "event/7QWn70064G/on-the-sly-justinien-tribillon")
        .expect("in fixture");
    let e = s.normalise(raw).unwrap().expect("kept");
    // "Saturday, 18 April 2026 - Sunday, 18 October 2026"
    assert!(e.all_day);
    assert_eq!(e.starts_at.to_rfc3339(), "2026-04-17T23:00:00+00:00");
    assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-10-17T23:00:00+00:00");
    insta::assert_json_snapshot!(
        "vam_qa_2026_09_29",
        serde_json::json!({
            "title": e.title,
            "starts_at": e.starts_at,
            "ends_at": e.ends_at,
            "all_day": e.all_day,
            "category": e.category,
        })
    );
}

#[tokio::test]
async fn fetches_the_listing_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whatson"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/whatson.html"))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Vam::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let expected = parse_listing(&fixture(&format!("{DIR}/whatson.html")));
    assert_eq!(raws.len(), expected.len());
    assert!(raws.len() > 50, "{}", raws.len());
}

#[tokio::test]
async fn an_empty_listing_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whatson"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body></body></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Vam::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no microdata events"), "{err}");
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /whatson\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whatson"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Vam::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
