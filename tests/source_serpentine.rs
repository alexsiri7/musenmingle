//! Serpentine Galleries scraper: snapshot tests over saved HTML fixtures and
//! an end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::Category;
use musenmingle::normalise::{is_london_midnight, london_date};
use musenmingle::sources::Source;
use musenmingle::sources::serpentine::{
    Serpentine, normalise_payload, parse_detail, parse_listing,
};
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

/// Issue #256: the scraper check of 2026-09-29 found the Pavilion stored with
/// a 00:00 start the page never states (its JSON-LD has a date-only start and
/// an 18:00 end). Pages captured 2026-10-03; expected values are the pages'.
#[test]
fn qa_2026_09_29_page_values() {
    const PAVILION: &str =
        "serpentine-pavilion-2026-by-isabel-abascal-and-alessandro-arienzo-lanza-atelier";
    const KANWAR: &str = "amar-kanwar-exhibition";
    let listing = parse_listing(&fixture(&format!("{DIR}/qa-2026-09-29.html")));
    for slug in [PAVILION, KANWAR] {
        let p = format!("/whats-on/{slug}/");
        assert!(listing.iter().any(|l| l.ends_with(&p)), "{p} not listed");
    }

    let s = Serpentine::new("https://www.serpentinegalleries.org".parse().unwrap());
    let mut out = Vec::new();
    for (slug, file) in [(PAVILION, "qa-2026-09-29-2"), (KANWAR, "qa-2026-09-29-3")] {
        let html = fixture(&format!("{DIR}/{file}.html"));
        let raw = parse_detail(&html, &format!("/whats-on/{slug}/")).expect("event page");
        let e = s.normalise(&raw).expect("normalise").expect("in scope");
        out.push((slug, e));
    }

    let (_, pavilion) = &out[0];
    assert!(pavilion.all_day);
    assert_eq!(london_date(pavilion.starts_at).to_string(), "2026-06-06");
    let (_, kanwar) = &out[1];
    assert!(!kanwar.all_day);
    assert_eq!(
        kanwar
            .starts_at
            .with_timezone(&chrono_tz::Europe::London)
            .format("%H:%M")
            .to_string(),
        "10:00"
    );

    let compact: Vec<_> = out
        .iter()
        .map(|(slug, e)| {
            serde_json::json!({
                "slug": slug,
                "title": e.title,
                "starts_at": e.starts_at,
                "ends_at": e.ends_at,
                "all_day": e.all_day,
            })
        })
        .collect();
    insta::assert_json_snapshot!("serpentine_qa_2026_09_29", compact);
}

#[test]
fn date_only_start_is_all_day_even_with_a_closing_time_end() {
    let event = |start: &str, end: &str| {
        normalise_payload(&serde_json::json!({
            "jsonld": {
                "name": "X",
                "startDate": start,
                "endDate": end,
                "location": {"name": "Serpentine Pavilion"},
            },
            "price_text": null,
        }))
        .unwrap()
        .unwrap()
    };

    let date_only = event("2026-06-06T00:00:00+00:00", "2026-10-25T18:00:00+00:00");
    assert!(date_only.all_day);
    assert!(is_london_midnight(date_only.starts_at));
    assert_eq!(
        date_only.ends_at.map(|e| e.to_rfc3339()).as_deref(),
        Some("2026-10-24T23:00:00+00:00")
    );

    let timed = event("2026-09-23T10:00:00+00:00", "2027-01-31T18:00:00+00:00");
    assert!(!timed.all_day);
    assert_eq!(
        timed.ends_at.map(|e| e.to_rfc3339()).as_deref(),
        Some("2027-01-31T18:00:00+00:00")
    );

    // Over two days only counting the closing hour: still an exhibition.
    let short_run = event("2026-07-01T00:00:00+00:00", "2026-07-03T18:00:00+00:00");
    assert!(short_run.all_day);
    assert_eq!(short_run.category, Category::Exhibition);

    // Closing hour on the opening day: no span left once floored.
    let one_day = event("2026-06-06T00:00:00+00:00", "2026-06-06T18:00:00+00:00");
    assert!(one_day.all_day);
    assert_eq!(one_day.ends_at, None);
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
