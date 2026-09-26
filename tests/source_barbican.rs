//! Barbican scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.
//!
//! The listings link to ~60 events at ~150 KB each, so only a representative
//! set of detail pages is saved (every category, every skip reason, and the
//! same slug under two years); the fetch test serves 404 for the rest.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::sources::Source;
use musenmingle::sources::barbican::{
    Barbican, MAX_DETAIL_PAGES, MAX_LISTING_PAGES, parse_detail, parse_listing,
};
use url::Url;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/barbican";
const SITE: &str = "https://www.barbican.org.uk";

/// Listing fixtures in crawl order: (art form, page number, file stem).
const LISTINGS: &[(&str, u32, &str)] = &[
    ("art-design", 0, "art-design"),
    ("art-design", 1, "art-design-page-1"),
    ("talks-events", 0, "talks-events"),
    ("talks-events", 1, "talks-events-page-1"),
    ("talks-events", 2, "talks-events-page-2"),
];

/// `<year>/<slug>` of every saved detail page, sorted.
fn detail_ids() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(DIR)
        .join("detail");
    let mut ids = Vec::new();
    for year in std::fs::read_dir(dir).unwrap() {
        let year = year.unwrap().path();
        let y = year.file_name().unwrap().to_string_lossy().into_owned();
        for page in std::fs::read_dir(&year).unwrap() {
            let stem = page.unwrap().path();
            ids.push(format!(
                "{y}/{}",
                stem.file_stem().unwrap().to_string_lossy()
            ));
        }
    }
    ids.sort();
    ids
}

fn detail_html(id: &str) -> String {
    fixture(&format!("{DIR}/detail/{id}.html"))
}

fn event_path(id: &str) -> String {
    let (year, slug) = id.split_once('/').unwrap();
    format!("/whats-on/{year}/event/{slug}")
}

fn scraper() -> Barbican {
    Barbican::new(SITE.parse().unwrap())
}

#[test]
fn listing_snapshot() {
    let pages: Vec<_> = LISTINGS
        .iter()
        .map(|(_, _, stem)| {
            serde_json::json!({
                "fixture": stem,
                "page": parse_listing(&fixture(&format!("{DIR}/{stem}.html"))),
            })
        })
        .collect();
    insta::assert_json_snapshot!("barbican_listing", pages);
}

#[test]
fn normalised_output_snapshot() {
    let s = scraper();
    let mut out = Vec::new();
    for id in detail_ids() {
        let url: Url = format!("{SITE}{}", event_path(&id)).parse().unwrap();
        let raw = parse_detail(&detail_html(&id), &url).expect("detail parses");
        assert_eq!(raw.source_event_id, id);
        out.push(serde_json::json!({
            "id": id,
            "payload": raw.payload,
            "event": s.normalise(&raw).expect("normalise"),
        }));
    }
    insta::assert_json_snapshot!("barbican_normalised", out);
}

#[test]
fn every_detail_fixture_is_linked_from_a_listing() {
    let linked: Vec<String> = LISTINGS
        .iter()
        .flat_map(|(_, _, stem)| parse_listing(&fixture(&format!("{DIR}/{stem}.html"))).event_paths)
        .collect();
    for id in detail_ids() {
        assert!(
            linked.contains(&event_path(&id)),
            "{id} is not on any listing"
        );
    }
}

#[tokio::test]
async fn fetches_paginated_listings_and_details_via_fetch_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(&server)
        .await;
    let mut linked: Vec<String> = Vec::new();
    for (form, page, stem) in LISTINGS {
        let body = fixture(&format!("{DIR}/{stem}.html"));
        for p in parse_listing(&body).event_paths {
            if !linked.contains(&p) {
                linked.push(p);
            }
        }
        let listing = Mock::given(method("GET")).and(path(format!("/whats-on/{form}")));
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
    let requested = &linked[..MAX_DETAIL_PAGES.min(linked.len())];
    assert!(
        linked.len() > MAX_DETAIL_PAGES,
        "fixtures should exercise the detail cap"
    );
    let ids = detail_ids();
    let mut expected_ids = Vec::new();
    for id in &ids {
        let p = event_path(id);
        let fetched = requested.contains(&p);
        if fetched {
            expected_ids.push(id.clone());
        }
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_string(detail_html(id)))
            .expect(u64::from(fetched))
            .mount(&server)
            .await;
    }

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Barbican::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    // Linked pages without a fixture got wiremock's 404: soft errors.
    let errors = ctx.take_errors();
    let missing: Vec<&String> = requested
        .iter()
        .filter(|p| !ids.iter().any(|id| event_path(id) == **p))
        .collect();
    assert_eq!(errors.len(), missing.len(), "{errors:?}");
    for p in missing {
        assert!(
            errors.iter().any(|e| e.starts_with(&format!("{p}: "))),
            "no error for {p}: {errors:?}"
        );
    }
    let site = server.uri();
    for raw in &raws {
        let expected = format!("{site}{}", event_path(&raw.source_event_id));
        assert_eq!(raw.source_url.as_deref(), Some(expected.as_str()));
    }
    let mut got: Vec<String> = raws.into_iter().map(|r| r.source_event_id).collect();
    got.sort();
    assert_eq!(got, expected_ids);
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
        .and(path("/whats-on/art-design"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Barbican::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}

#[tokio::test]
async fn empty_listings_are_an_error() {
    // Only a series hub card: a template change must reach the health
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
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><article class="listing--event"><a class="search-listing__link" href="/whats-on/2026/series/some-series"></a></article></body></html>"#,
        ))
        .expect(2)
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Barbican::new(server.uri().parse().unwrap());
    let err = s.fetch(&ctx).await.unwrap_err().to_string();
    assert!(err.contains("no event links"), "{err}");
}

#[tokio::test]
async fn listing_pagination_stops_at_the_page_cap() {
    // Every listing page links to a further one; the page past the cap must
    // never be requested.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .mount(&server)
        .await;
    for form in ["art-design", "talks-events"] {
        for page in 0..=MAX_LISTING_PAGES {
            let body = format!(
                r#"<html><body>
                <article class="listing--event"><a class="search-listing__link" href="/whats-on/2026/event/{form}-{page}"></a></article>
                <div class="pager"><a rel="next" href="?page={}">Load More</a></div>
                </body></html>"#,
                page + 1
            );
            let listing = Mock::given(method("GET")).and(path(format!("/whats-on/{form}")));
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
    }

    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let s = Barbican::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");

    // Detail pages are unmocked (404), so each collected event is one error.
    assert!(raws.is_empty());
    assert_eq!(ctx.take_errors().len(), 2 * MAX_LISTING_PAGES);
}
