//! V&A scraper: snapshot tests over the saved `/whatson` fixture and an
//! end-to-end fetch against wiremock.

mod common;

use chrono::NaiveDate;
use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use musenmingle::model::Category;
use musenmingle::normalise::london_date;
use musenmingle::sources::vam::{
    Vam, parse_exhibition_carousels, parse_listing, with_exhibition_page,
};
use musenmingle::sources::{Source, jsonld};
use rust_decimal::Decimal;
use scraper::Html;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/vam";
/// The exhibitions in the carousels of both listing fixtures.
const CAROUSEL_SLUGS: [&str; 9] = [
    "inside-aardman-wallace-gromit-and-friends",
    "schiaparelli",
    "the-music-is-black-a-british-story",
    "rising-voices-contemporary-art-from-asia-australia-and-the-pacific",
    "constantinople-to-istanbul-one-city-two-empires",
    "punk-to-pop",
    "south-asia-now-fashion-art-design",
    "sculpture-in-clay",
    "chintz",
];

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

/// Issue #371: the scraper check of 2026-10-24 found "upperground: DJ sets"
/// missed. It is a weekly Thursday DJ residency (the card says "On now until
/// …"; the microdata spans 8 October – 17 December, and the sessions,
/// including the gap on 29 October, are in prose only): a series of
/// sessions over several days, skipped on purpose (`qa_scope`). Expected
/// values are the page's.
#[test]
fn qa_2026_10_24_page_values() {
    let raws = parse_listing(&fixture(&format!("{DIR}/qa-2026-10-24.html")));
    let raw = raws
        .iter()
        .find(|r| {
            r.source_event_id == "event/p9PR3vALR5D/upperground-dj-sets-october-december-2026"
        })
        .expect("listed");
    let skipped = source().normalise(raw).expect("normalise").is_none();
    insta::assert_json_snapshot!(
        "vam_qa_2026_10_24",
        serde_json::json!({
            "title": raw.payload["title"],
            "type": raw.payload["type"],
            "date_text": raw.payload["date_text"],
            "venue": raw.payload["venue"],
            "start": raw.payload["start"],
            "end": raw.payload["end"],
            "skipped": skipped,
        })
    );
}

/// Issue #397: the scraper check of 2026-11-04 found the nine exhibitions of
/// the listing's "Current" and "Upcoming" carousels missed (they are not
/// grid cards). Their dates come from each exhibition's page; expected
/// values are the page's.
#[test]
fn qa_2026_11_04_page_values() {
    let s = source();
    let listing = fixture(&format!("{DIR}/qa-2026-11-04.html"));
    let grid = parse_listing(&listing);
    let carousels = parse_exhibition_carousels(&listing);
    let ids: Vec<String> = CAROUSEL_SLUGS
        .iter()
        .map(|slug| format!("exhibitions/{slug}"))
        .collect();
    assert_eq!(
        carousels
            .iter()
            .map(|r| r.source_event_id.as_str())
            .collect::<Vec<_>>(),
        ids
    );
    let mut out = Vec::new();
    for (slug, raw) in CAROUSEL_SLUGS.iter().zip(carousels) {
        assert!(
            grid.iter()
                .all(|g| g.source_event_id != raw.source_event_id),
            "{slug} is a grid card"
        );
        let page = fixture(&format!("{DIR}/exhibitions/{slug}.html"));
        let raw = with_exhibition_page(raw, &page).expect("JSON-LD event");
        let e = s.normalise(&raw).expect("normalise").expect("kept");
        assert!(e.all_day, "{slug}");
        assert_eq!(e.category, Category::Exhibition, "{slug}");
        // The card's line: "Closes Sunday, 15 November 2026" or "Opens …".
        let line = raw.payload["date_text"].as_str().unwrap();
        let (prefix, date) = line.split_once(' ').unwrap();
        let date = NaiveDate::parse_from_str(date, "%A, %d %B %Y").unwrap();
        match prefix {
            "Closes" => assert_eq!(e.ends_at.map(london_date), Some(date), "{slug}"),
            "Opens" => assert_eq!(london_date(e.starts_at), date, "{slug}"),
            _ => panic!("{slug}: {line}"),
        }
        out.push(serde_json::json!({
            "id": raw.source_event_id,
            "title": e.title,
            "venue": e.venue_name,
            "date_text": line,
            "starts_at": e.starts_at,
            "ends_at": e.ends_at,
            "all_day": e.all_day,
            "category": e.category,
            "price": e.price,
        }));
        match *slug {
            "schiaparelli" => {
                assert_eq!(e.title, "Schiaparelli: Fashion Becomes Art");
                assert_eq!(e.starts_at.to_rfc3339(), "2026-03-28T00:00:00+00:00");
                assert_eq!(e.ends_at.unwrap().to_rfc3339(), "2026-11-15T00:00:00+00:00");
                assert_eq!(e.venue_name.as_deref(), Some("V&A South Kensington"));
                assert_eq!(e.price.min, Some(Decimal::from(28)));
            }
            "inside-aardman-wallace-gromit-and-friends" => {
                assert_eq!(e.title, "Inside Aardman: Wallace & Gromit and Friends");
                assert_eq!(e.venue_name.as_deref(), Some("Young V&A"));
            }
            // "Tickets available soon": no offer yet, so not free.
            "punk-to-pop" => {
                assert!(!e.price.is_free);
                assert_eq!(e.price.min, None);
            }
            _ => {}
        }
    }
    insta::assert_json_snapshot!("vam_qa_2026_11_04", out);
}

/// Issue #443: on presale (2026-11-12) Punk to Pop's JSON-LD offer has an
/// empty price (`"price": ,`), invalid JSON that dropped the whole event.
/// It reads as an offer without a price, like "Tickets available soon".
#[test]
fn issue_443_presale_offer_without_a_price() {
    let page = fixture(&format!("{DIR}/exhibitions/punk-to-pop-2026-11-12.html"));
    assert!(jsonld::extract_events(&Html::parse_document(&page)).is_empty());
    let raw = parse_exhibition_carousels(&fixture(&format!("{DIR}/qa-2026-11-04.html")))
        .into_iter()
        .find(|r| r.source_event_id == "exhibitions/punk-to-pop")
        .unwrap();
    let raw = with_exhibition_page(raw, &page).expect("JSON-LD event");
    assert!(raw.payload["price"].is_null());
    assert_eq!(raw.payload["currency"], "GBP");
    let e = source().normalise(&raw).expect("normalise").expect("kept");
    assert!(e.all_day);
    assert_eq!(e.category, Category::Exhibition);
    assert_eq!(
        london_date(e.starts_at),
        NaiveDate::from_ymd_opt(2027, 3, 13).unwrap()
    );
    assert!(!e.price.is_free);
    assert_eq!(e.price.min, None);
}

#[tokio::test]
async fn fetches_the_listing_via_fetch_context() {
    let server = MockServer::start().await;
    // The site's rules, without its `Crawl-delay: 2` (FetchContext honours
    // it, which would add 2 s per exhibition page).
    let robots: String = fixture(&format!("{DIR}/robots.txt"))
        .lines()
        .filter(|l| !l.starts_with("Crawl-delay"))
        .map(|l| format!("{l}\n"))
        .collect();
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(robots))
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
    for slug in CAROUSEL_SLUGS {
        Mock::given(method("GET"))
            .and(path(format!("/exhibitions/{slug}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&format!("{DIR}/exhibitions/{slug}.html"))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Vam::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let expected = parse_listing(&fixture(&format!("{DIR}/whatson.html")));
    assert_eq!(raws.len(), expected.len() + CAROUSEL_SLUGS.len());
    assert!(expected.len() > 50, "{}", expected.len());
    let schiaparelli = raws
        .iter()
        .find(|r| r.source_event_id == "exhibitions/schiaparelli")
        .expect("fetched");
    assert_eq!(schiaparelli.payload["start"], "2026-03-28");
}

#[tokio::test]
async fn a_failed_exhibition_page_is_reported_not_fatal() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whatson"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/whatson.html"))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/exhibitions/schiaparelli"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body></body></html>"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Vam::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    let expected = parse_listing(&fixture(&format!("{DIR}/whatson.html")));
    assert_eq!(raws.len(), expected.len());
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), CAROUSEL_SLUGS.len(), "{errors:?}");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("exhibitions/schiaparelli: no JSON-LD event")),
        "{errors:?}"
    );
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
