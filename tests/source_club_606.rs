//! The 606 Club scraper: snapshot tests over saved HTML fixtures and an
//! end-to-end fetch against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::sources::Source;
use musenmingle::sources::club_606::{Club606, MAX_WEEKS, parse_detail, parse_week, raw_event};
use reqwest::StatusCode;
use serde_json::Value;
use url::Url;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIR: &str = "scrapers/606-club";
const SITE: &str = "https://www.606club.co.uk";

/// The shows of the week of 2026-11-09, the current one on that day.
const WEEK_1: &[&str] = &[
    "phil-mulfords-thunderthumbs-25",
    "dan-reinstein-59",
    "nigel-price-special-guest-lorne-lofsky-1",
    "fraser-urquhart-quartet-ft-sam-braysher",
    "imaani-with-special-guest-polly-gibbons",
    "peter-king-memorial-sax-summit-mornington-lockett-tommaso-starace-karen-sharp-donovan-haffner-the-rob-barron-trio",
    "world-heart-beat-music-academy-and-the-julian-joseph-jazz-academy-present-together-we-go-forward-5",
    "sandra-mae-lux",
];
/// The shows of the week of 2026-12-07, whose two "Closed for Private
/// Event" nights are left out.
const WEEK_DEC_7: &[&str] = &[
    "dino-baptiste-34",
    "dan-reinstein-celebrating-24-years-at-the-606",
    "jay-azzolina-ft-riley-stone-lonergan-1",
    "the-wonder-of-stevie-ft-noel-mccalla-derek-nash-2",
];

fn page(slug: &str) -> String {
    fixture(&format!("{DIR}/{slug}.html"))
}

fn week(name: &str) -> String {
    fixture(&format!("{DIR}/{name}.html"))
}

fn slugs(cards: &[Value]) -> Vec<String> {
    cards
        .iter()
        .map(|c| c["slug"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn saved_robots_allow_the_calendar_and_event_pages() {
    let robots = RobotsPolicy::from_response(
        StatusCode::OK,
        fixture(&format!("{DIR}/robots.txt")).as_bytes(),
    );
    for p in [
        "/events/",
        "/events/?d=2026-11-16",
        "/events/view/dan-reinstein-59/",
    ] {
        assert!(
            robots.allowed(&Url::parse(SITE).unwrap().join(p).unwrap()),
            "{p}"
        );
    }
}

#[test]
fn a_week_gives_its_shows_and_the_next_week() {
    let url = Url::parse(&format!("{SITE}/events/")).unwrap();
    let w = parse_week(&week("events"), &url).expect("calendar");
    assert_eq!(slugs(&w.cards), WEEK_1);
    assert_eq!(w.shows, WEEK_1.len());
    assert_eq!(
        w.next_week.unwrap().as_str(),
        "https://www.606club.co.uk/events/?d=2026-11-16"
    );
    let first = &w.cards[0];
    assert_eq!(
        first["url"],
        "https://www.606club.co.uk/events/view/phil-mulfords-thunderthumbs-25/"
    );
    assert_eq!(first["title"], "Phil Mulford's 'Thunderthumbs'");
    assert_eq!(first["label"], Value::Null);
    assert_eq!(w.cards[4]["label"], "EFG LONDON JAZZ FESTIVAL");

    let w = parse_week(&week("events-2026-12-07"), &url).expect("calendar");
    assert_eq!(slugs(&w.cards), WEEK_DEC_7);
    assert_eq!(w.shows, WEEK_DEC_7.len() + 2);

    // Past the programme: the calendar, with no shows.
    let w = parse_week(&week("events-2027-01-25"), &url).expect("calendar");
    assert!(w.cards.is_empty());
    assert_eq!(w.shows, 0);

    assert!(parse_week(&page(WEEK_1[0]), &url).is_none());
}

#[test]
fn normalised_output_snapshot() {
    let url = Url::parse(&format!("{SITE}/events/")).unwrap();
    let s = Club606::new(SITE.parse().unwrap());
    let out: Vec<_> = ["events", "events-2026-12-07"]
        .iter()
        .flat_map(|w| parse_week(&week(w), &url).expect("calendar").cards)
        .map(|card| {
            let slug = card["slug"].as_str().unwrap();
            let detail = parse_detail(&page(slug)).expect("event page");
            let raw = raw_event(&card, detail);
            match s.normalise(&raw) {
                Ok(event) => serde_json::json!({
                    "source_event_id": raw.source_event_id,
                    "when": raw.payload["detail"]["when"],
                    "event": event,
                }),
                Err(e) => serde_json::json!({
                    "source_event_id": raw.source_event_id,
                    "error": e.to_string(),
                }),
            }
        })
        .collect();
    insta::assert_json_snapshot!("club_606_normalised", out);
}

#[test]
fn a_calendar_page_is_not_an_event_page() {
    assert!(parse_detail(&week("events")).is_none());
}

async fn serve_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(&format!("{DIR}/robots.txt"))),
        )
        .expect(1) // cached for the rest of the run
        .mount(server)
        .await;
}

async fn serve_week(server: &MockServer, d: Option<&str>, body: String) {
    let mock = Mock::given(method("GET")).and(path("/events/"));
    let mock = match d {
        Some(d) => mock.and(query_param("d", d)),
        None => mock.and(query_param_is_missing("d")),
    };
    mock.respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

async fn serve_page(server: &MockServer, slug: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(format!("/events/view/{slug}/")))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

/// The saved week of 2026-12-07 with its "Next Week" link pointing at
/// `next` instead.
fn dec_7_linking(next: &str) -> String {
    week("events-2026-12-07").replace("/events/?d=2026-12-14\"", &format!("/events/?d={next}\""))
}

#[tokio::test]
async fn reads_the_weeks_up_to_an_empty_one_and_their_event_pages_via_fetch_context() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    // The current week links 2026-11-16; serve the saved December week
    // there, linking the saved empty week.
    serve_week(&server, None, week("events")).await;
    serve_week(&server, Some("2026-11-16"), dec_7_linking("2027-01-25")).await;
    serve_week(&server, Some("2027-01-25"), week("events-2027-01-25")).await;
    for slug in WEEK_1.iter().chain(WEEK_DEC_7) {
        serve_page(&server, slug, page(slug)).await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let s = Club606::new(server.uri().parse().unwrap());
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let ids: Vec<_> = raws.iter().map(|r| r.source_event_id.as_str()).collect();
    assert_eq!(ids, [WEEK_1, WEEK_DEC_7].concat());
    assert_eq!(
        raws[0].source_url.as_deref(),
        Some(format!("{}/events/view/{}/", server.uri(), WEEK_1[0]).as_str())
    );
    // robots.txt, three weeks and every show's page (not the closed nights').
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        4 + WEEK_1.len() + WEEK_DEC_7.len()
    );
}

/// A week page with no shows whose "Next Week" button links `next`.
fn empty_week_linking(next: &str) -> String {
    week("events-2027-01-25").replace("/events/?d=2027-02-01\"", &format!("/events/?d={next}\""))
}

/// A one-show week linking `next`.
fn one_show_week(next: usize) -> String {
    format!(
        r#"<html><body><div id="events">
        <div class="three columns event-listing"><div class="box dark"><div class="contained mh">
        <p><a href="/events/view/{slug}/" class="banner">Mon 09<sup>th</sup> Nov - 8:00pm</a></p>
        <p class="h4">Show</p></div></div></div>
        <a href="/events/?d=w{next}" class="button arrow purple ">Next Week</a>
        </div></body></html>"#,
        slug = WEEK_1[0]
    )
}

#[tokio::test]
async fn reads_at_most_max_weeks() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_week(&server, None, one_show_week(2)).await;
    for n in 2..=MAX_WEEKS {
        serve_week(&server, Some(&format!("w{n}")), one_show_week(n + 1)).await;
    }
    serve_page(&server, WEEK_1[0], page(WEEK_1[0])).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = Club606::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    // The same show in every week is read once.
    assert_eq!(raws.len(), 1);
    assert!(ctx.take_errors().is_empty());
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1 + MAX_WEEKS + 1
    );
}

#[tokio::test]
async fn caps_event_pages_and_reports_bad_ones() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_week(&server, None, week("events")).await;
    serve_week(&server, Some("2026-11-16"), empty_week_linking("x")).await;
    serve_page(&server, WEEK_1[0], page(WEEK_1[0])).await;
    serve_page(&server, WEEK_1[1], "<html></html>".into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = Club606::new(server.uri().parse().unwrap())
        .with_max_details(2)
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), 1);
    assert_eq!(raws[0].source_event_id, WEEK_1[0]);
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), WEEK_1.len() - 1, "{errors:?}");
    assert!(errors[0].contains("not an event page"), "{errors:?}");
    assert!(
        errors[1].contains("over the 2 event-page cap"),
        "{errors:?}"
    );
}

#[tokio::test]
async fn a_later_week_failing_keeps_the_shows_found_so_far() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_week(&server, None, week("events")).await;
    Mock::given(method("GET"))
        .and(path("/events/"))
        .and(query_param("d", "2026-11-16"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    for slug in WEEK_1 {
        serve_page(&server, slug, page(slug)).await;
    }
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = Club606::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert_eq!(raws.len(), WEEK_1.len());
    let errors = ctx.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("calendar week 2:"), "{errors:?}");
}

#[tokio::test]
async fn an_empty_calendar_is_not_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_week(&server, None, week("events-2027-01-25")).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let raws = Club606::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .expect("fetch");
    assert!(raws.is_empty());
    assert!(ctx.take_errors().is_empty());
}

#[tokio::test]
async fn a_page_that_is_not_the_calendar_is_an_error() {
    let server = MockServer::start().await;
    serve_robots(&server).await;
    serve_week(&server, None, "<html><body></body></html>".into()).await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = Club606::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no calendar"), "{err}");
}

#[tokio::test]
async fn robots_disallow_blocks_the_scraper() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: MuseNMingleBot\nDisallow: /events/\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should never be fetched"))
        .expect(0)
        .mount(&server)
        .await;
    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    let err = Club606::new(server.uri().parse().unwrap())
        .fetch(&ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("robots.txt disallows"), "{err}");
}
