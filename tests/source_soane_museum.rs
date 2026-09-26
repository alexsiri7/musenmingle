//! Sir John Soane's Museum scraper: snapshot tests over saved HTML fixtures
//! and an end-to-end fetch against wiremock.
//!
//! The three listing pages are saved in full, plus the detail page of every
//! in-scope non-exhibition card (the only detail pages the scraper fetches).

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::soane_museum::{
    MAX_DETAIL_PAGES, MAX_LISTING_PAGES, SoaneMuseum, card_event, parse_detail_location,
    parse_listing,
};
use url::Url;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/soane-museum";
const SITE: &str = "https://www.soane.org";

/// Listing fixtures in crawl order: (page number, file stem).
const LISTINGS: &[(u32, &str)] = &[
    (0, "whats-on"),
    (1, "whats-on-page-1"),
    (2, "whats-on-page-2"),
];

/// Slugs of every saved detail page, sorted.
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

fn detail_html(slug: &str) -> String {
    fixture(&format!("{DIR}/detail/{slug}.html"))
}

fn listing_html(stem: &str) -> String {
    fixture(&format!("{DIR}/{stem}.html"))
}

fn page_url(site: &str, page: u32) -> Url {
    let url = format!("{site}/whats-on");
    let url = if page == 0 {
        url
    } else {
        format!("{url}?page={page}")
    };
    url.parse().unwrap()
}

#[test]
fn listing_snapshot() {
    let pages: Vec<_> = LISTINGS
        .iter()
        .map(|(_, stem)| {
            serde_json::json!({
                "fixture": stem,
                "page": parse_listing(&listing_html(stem)),
            })
        })
        .collect();
    insta::assert_json_snapshot!("soane_museum_listing", pages);
}

#[test]
fn normalised_output_snapshot() {
    let s = SoaneMuseum::new(SITE.parse().unwrap());
    let details = detail_slugs();
    let mut out: Vec<serde_json::Value> = Vec::new();
    for (page, stem) in LISTINGS {
        for card in parse_listing(&listing_html(stem)).cards {
            let (_, mut raw) = card_event(&card, &page_url(SITE, *page)).unwrap();
            if out.iter().any(|o| o["id"] == raw.source_event_id) {
                continue;
            }
            let slug = card.path.rsplit('/').next().unwrap();
            if details.iter().any(|d| d == slug) {
                raw.payload["location"] =
                    serde_json::json!(parse_detail_location(&detail_html(slug)));
            }
            out.push(serde_json::json!({
                "id": raw.source_event_id,
                "payload": raw.payload,
                "event": s.normalise(&raw).expect("normalise"),
            }));
        }
    }
    insta::assert_json_snapshot!("soane_museum_normalised", out);
}

#[tokio::test]
async fn fetches_listing_pages_and_off_site_details_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(&server)
        .await;
    let mut paths: Vec<String> = Vec::new();
    for (page, stem) in LISTINGS {
        let body = listing_html(stem);
        for card in parse_listing(&body).cards {
            if !paths.contains(&card.path) {
                paths.push(card.path);
            }
        }
        let listing = Mock::given(method("GET")).and(path("/whats-on"));
        let listing = if *page == 0 {
            listing.and(query_param_is_missing("page"))
        } else {
            listing.and(query_param("page", page.to_string()))
        };
        listing
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
    }
    for slug in detail_slugs() {
        Mock::given(method("GET"))
            .and(path(format!("/whats-on/{slug}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(detail_html(&slug)))
            .expect(1)
            .mount(&server)
            .await;
    }

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = SoaneMuseum::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    let errors = ctx.take_errors();
    assert!(errors.is_empty(), "{errors:?}");
    let site = server.uri();
    let got: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    let expected: Vec<&str> = paths.iter().map(|p| p.trim_matches('/')).collect();
    assert_eq!(got, expected);
    for raw in &raws {
        let url = format!("{site}/{}", raw.source_event_id);
        assert_eq!(raw.source_url.as_deref(), Some(url.as_str()));
    }
    let medal = raws
        .iter()
        .find(|r| r.source_event_id == "whats-on/2026-soane-medal-lecture")
        .unwrap();
    let event = s.normalise(medal).unwrap().unwrap();
    assert_eq!(event.venue_name.as_deref(), Some("Royal Academy of Arts"));
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
        .and(path("/whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = SoaneMuseum::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listing_is_an_error() {
    // A template change must reach the health checker instead of producing
    // clean, empty runs.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                r#"<html><body><div class="o-view__listing"></div></body></html>"#,
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = SoaneMuseum::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event cards"), "{err}");
}

#[tokio::test]
async fn listing_and_detail_fetches_stop_at_their_caps() {
    // Every listing page links to a further one and holds five single-date
    // talks; the page past the cap and details past theirs are never
    // requested.
    const TALKS_PER_PAGE: usize = 5;
    const { assert!(MAX_LISTING_PAGES * TALKS_PER_PAGE > MAX_DETAIL_PAGES) };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    for page in 0..=MAX_LISTING_PAGES {
        let cards: String = (0..TALKS_PER_PAGE)
            .map(|i| {
                format!(
                    r#"<article about="/whats-on/talk-{page}-{i}"><div class="o-teaser">
                    <span class="o-teaser__event-type">Talks</span>
                    <div class="o-teaser__content"><h2 class="o-teaser__title"><a href="/whats-on/talk-{page}-{i}">Talk</a></h2>
                    <div class="o-teaser__date"><p><time datetime="2026-11-24T18:30:00Z">24 November, 2026</time></p></div>
                    </div></div></article>"#
                )
            })
            .collect();
        let body = format!(
            r#"<html><body><div class="o-view__listing">{cards}</div>
            <nav class="pager"><a href="?page={}" rel="next">Next</a></nav></body></html>"#,
            page + 1
        );
        let listing = Mock::given(method("GET")).and(path("/whats-on"));
        let listing = if page == 0 {
            listing.and(query_param_is_missing("page"))
        } else {
            listing.and(query_param("page", page.to_string()))
        };
        listing
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(u64::from(page < MAX_LISTING_PAGES))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex("^/whats-on/talk-"))
        .respond_with(ResponseTemplate::new(404))
        .expect(MAX_DETAIL_PAGES as u64)
        .mount(&server)
        .await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = SoaneMuseum::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    // Every fetched detail page was a 404: one soft error each.
    assert!(raws.is_empty());
    assert_eq!(ctx.take_errors().len(), MAX_DETAIL_PAGES);
}

#[tokio::test]
async fn cards_with_bad_times_or_sidebars_become_errors() {
    // Template drift must reach the health checker: a card whose date no
    // longer parses is kept for normalise to reject, and a detail page whose
    // last sidebar line is not a location is reported, not taken as a venue.
    let card = |slug: &str, event_type: &str, datetime: &str| {
        format!(
            r#"<article about="/whats-on/{slug}"><div class="o-teaser">
            <span class="o-teaser__event-type">{event_type}</span>
            <div class="o-teaser__content"><h2 class="o-teaser__title"><a href="/whats-on/{slug}">{slug}</a></h2>
            <div class="o-teaser__date"><p><time datetime="{datetime}">24 November, 2026</time></p></div>
            </div></div></article>"#
        )
    };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    let cards = [
        card("undated-talk", "Talks", "24 November"),
        card("priced-talk", "Talks", "2026-11-24T18:30:00Z"),
    ]
    .concat();
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"<html><body><div class="o-view__listing">{cards}</div></body></html>"#
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/priced-talk"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><aside class="o-sidebar__info-box">
            <p>18:00 - 19:30</p><p>Tickets: £15</p></aside></body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on/undated-talk"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = SoaneMuseum::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    let ids: Vec<&str> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, ["whats-on/undated-talk"]);
    let err = s.normalise(&raws[0]).unwrap_err().to_string();
    assert!(err.contains("bad starts time"), "{err}");
    assert_eq!(
        ctx.take_errors(),
        ["whats-on/priced-talk: no location on the detail page"]
    );
}
