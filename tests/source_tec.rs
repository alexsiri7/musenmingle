//! The Events Calendar platform: snapshot tests over each seeded venue's
//! saved API pages / list view (normalised with the venue's seed `config`),
//! robots.txt checks of the URLs each venue is read from, and end-to-end
//! fetches against wiremock.

mod common;

use common::fixture;
use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::{FetchContext, RobotsPolicy};
use musenmingle::model::RawEvent;
use musenmingle::sources::Source;
use musenmingle::sources::tec::{Tec, TecConfig, parse_api_page, parse_list_view};
use reqwest::StatusCode;
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path, query_param, query_param_contains};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SEED: &str = "migrations/20260927400002_seed_tec_venues.sql";
/// Later migrations merging into a seeded row's config.
const CONFIG_UPDATES: &[&str] = &["migrations/20261002000001_tec_bow_arts_categories.sql"];
const API_PATH: &str = "/wp-json/tribe/events/v1/events";

fn read_migration(file: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file)).unwrap()
}

/// Every `(key, base_url, config)` TEC row in the seed migration, with the
/// `CONFIG_UPDATES` applied.
fn seed_rows() -> Vec<(String, String, Value)> {
    let sql = read_migration(SEED);
    let mut rows = Vec::new();
    for (i, _) in sql.match_indices("('tec-") {
        let row = &sql[i + 2..];
        let key = &row[..row.find('\'').unwrap()];
        let base = row.split('\'').nth(4).unwrap();
        let config = &row[row.find("'{").unwrap() + 1..row.find("}'::jsonb").unwrap() + 1];
        let config: Value = serde_json::from_str(&config.replace("''", "'"))
            .unwrap_or_else(|e| panic!("{key}: config is not JSON: {e}"));
        rows.push((key.to_string(), base.to_string(), config));
    }
    for file in CONFIG_UPDATES {
        let sql = read_migration(file);
        for (i, _) in sql.match_indices("config || '") {
            let update = &sql[i + "config || '".len()..];
            let patch: Value = serde_json::from_str(
                &update[..update.find("'::jsonb").unwrap()].replace("''", "'"),
            )
            .unwrap_or_else(|e| panic!("{file}: config is not JSON: {e}"));
            let key = update.split("WHERE key = '").nth(1).unwrap();
            let key = &key[..key.find('\'').unwrap()];
            let (_, _, config) = rows
                .iter_mut()
                .find(|(k, _, _)| k == key)
                .unwrap_or_else(|| panic!("{file}: no seed row {key}"));
            for (k, v) in patch.as_object().unwrap() {
                config[k] = v.clone();
            }
        }
    }
    rows
}

fn seed_row(key: &str) -> (String, Value) {
    let (_, base, config) = seed_rows()
        .into_iter()
        .find(|(k, _, _)| k == key)
        .unwrap_or_else(|| panic!("no seed row {key}"));
    (base, config)
}

fn source(key: &str, base: &str) -> Tec {
    let (_, config) = seed_row(key);
    Tec::from_row(key, base.parse().unwrap(), Some(&config)).unwrap()
}

fn api_page(key: &str, page: u32) -> Vec<RawEvent> {
    let body: Value =
        serde_json::from_str(&fixture(&format!("scrapers/{key}/api-page-{page}.json"))).unwrap();
    let (items, _) = parse_api_page(&body).unwrap();
    items.into_iter().map(Result::unwrap).collect()
}

fn list_view(key: &str) -> Vec<RawEvent> {
    let (base, _) = seed_row(key);
    let (_, config) = seed_row(key);
    let list_path = config["list_path"].as_str().unwrap();
    let url = Url::parse(&base).unwrap().join(list_path).unwrap();
    parse_list_view(&fixture(&format!("scrapers/{key}/list.html")), &url)
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

fn normalised(key: &str, raws: &[RawEvent]) -> Vec<Value> {
    let (base, _) = seed_row(key);
    let s = source(key, &base);
    raws.iter()
        .map(
            |raw| json!({"id": raw.source_event_id, "event": s.normalise(raw).expect("normalise")}),
        )
        .collect()
}

#[test]
fn every_seeded_config_is_valid() {
    let rows = seed_rows();
    assert_eq!(rows.len(), 7);
    for (key, _, config) in rows {
        TecConfig::from_json(Some(&config)).unwrap_or_else(|e| panic!("{key}: {e}"));
    }
}

/// Every venue that leaves events out says so to the scraper check.
#[test]
fn every_seeded_venue_that_skips_events_has_a_qa_scope() {
    for (key, base, config) in seed_rows() {
        let skips = config.get("default_category").is_none()
            || config
                .get("skip_categories")
                .is_some_and(|s| s != &json!([]));
        if skips {
            assert!(source(&key, &base).qa_scope().is_some(), "{key}");
        }
    }
}

#[test]
fn housmans_api_normalised() {
    let raws = api_page("tec-housmans", 1);
    insta::assert_json_snapshot!(
        "tec_housmans_api_normalised",
        normalised("tec-housmans", &raws)
    );
}

#[test]
fn housmans_jsonld_normalised() {
    let raws = list_view("tec-housmans");
    insta::assert_json_snapshot!(
        "tec_housmans_jsonld_normalised",
        normalised("tec-housmans", &raws)
    );
}

#[test]
fn chats_palace_normalised() {
    let raws = api_page("tec-chats-palace", 1);
    insta::assert_json_snapshot!(
        "tec_chats_palace_normalised",
        normalised("tec-chats-palace", &raws)
    );
}

#[test]
fn select_gallery_normalised() {
    let raws = api_page("tec-select-gallery", 1);
    insta::assert_json_snapshot!(
        "tec_select_gallery_normalised",
        normalised("tec-select-gallery", &raws)
    );
}

#[test]
fn freud_museum_normalised() {
    let raws = api_page("tec-freud-museum", 1);
    insta::assert_json_snapshot!(
        "tec_freud_museum_normalised",
        normalised("tec-freud-museum", &raws)
    );
}

#[test]
fn cinema_museum_normalised() {
    let mut raws = api_page("tec-cinema-museum", 1);
    raws.extend(api_page("tec-cinema-museum", 2));
    insta::assert_json_snapshot!(
        "tec_cinema_museum_normalised",
        normalised("tec-cinema-museum", &raws)
    );
}

#[test]
fn bow_arts_jsonld_normalised() {
    let raws = list_view("tec-bow-arts");
    insta::assert_json_snapshot!(
        "tec_bow_arts_jsonld_normalised",
        normalised("tec-bow-arts", &raws)
    );
}

#[test]
fn slbi_jsonld_normalised() {
    let raws = list_view("tec-slbi");
    insta::assert_json_snapshot!("tec_slbi_jsonld_normalised", normalised("tec-slbi", &raws));
}

#[test]
fn ids_survive_a_switch_between_api_and_list_view() {
    let api: Vec<String> = api_page("tec-housmans", 1)
        .into_iter()
        .map(|r| r.source_event_id)
        .collect();
    let list: Vec<String> = list_view("tec-housmans")
        .into_iter()
        .map(|r| r.source_event_id)
        .collect();
    assert!(!list.is_empty());
    assert!(
        list.iter().all(|id| api.contains(id)),
        "{list:?} vs {api:?}"
    );
}

#[test]
fn saved_robots_allow_what_each_venue_reads() {
    for (key, base, config) in seed_rows() {
        let robots = RobotsPolicy::from_response(
            StatusCode::OK,
            fixture(&format!("scrapers/{key}/robots.txt")).as_bytes(),
        );
        let base = Url::parse(&base).unwrap();
        let api = base
            .join(&format!(
                "{API_PATH}?ends_after=2026-09-26&per_page=50&page=1"
            ))
            .unwrap();
        match config.get("api_path") {
            Some(Value::Null) => assert!(!robots.allowed(&api), "{key}: API is allowed after all"),
            _ => assert!(robots.allowed(&api), "{key}: {api}"),
        }
        if let Some(list_path) = config.get("list_path").and_then(Value::as_str) {
            let list = base.join(list_path).unwrap();
            assert!(robots.allowed(&list), "{key}: {list}");
        }
    }
}

async fn mount_open_robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_api_page(server: &MockServer, page: u32, body: String, expect: u64) {
    Mock::given(method("GET"))
        .and(path(API_PATH))
        .and(query_param("page", page.to_string()))
        .and(query_param("per_page", "50"))
        .and(query_param_contains("ends_after", "-"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(expect)
        .mount(server)
        .await;
}

async fn mount_status(server: &MockServer, at: &str, status: u16, expect: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(status))
        .expect(expect)
        .mount(server)
        .await;
}

async fn mount_list(server: &MockServer, at: &str, body: String, expect: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(expect)
        .mount(server)
        .await;
}

fn ctx() -> FetchContext {
    FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap()
}

fn tec(server: &MockServer, config: Value) -> Tec {
    Tec::from_row("tec-test", server.uri().parse().unwrap(), Some(&config)).unwrap()
}

fn ids(raws: &[RawEvent]) -> Vec<String> {
    raws.iter().map(|r| r.source_event_id.clone()).collect()
}

#[tokio::test]
async fn follows_api_pages_via_fetch_context() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    for page in [1, 2] {
        let body = fixture(&format!("scrapers/tec-cinema-museum/api-page-{page}.json"));
        mount_api_page(&server, page, body, 1).await;
    }
    let ctx = ctx();
    let (_, config) = seed_row("tec-cinema-museum");
    let s = tec(&server, config);
    let raws = s.fetch(&ctx).await.expect("fetch");
    assert!(ctx.take_errors().is_empty());
    let mut want = api_page("tec-cinema-museum", 1);
    want.extend(api_page("tec-cinema-museum", 2));
    assert_eq!(ids(&raws), ids(&want));
    for raw in &raws {
        s.normalise(raw).expect("normalise");
    }
}

#[tokio::test]
async fn api_pages_are_capped() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    for page in 1..=3 {
        let body = json!({"events": [{"url": format!("https://venue.example/event/e{page}/"),
                                      "title": "E"}],
                          "total": 10, "total_pages": 10});
        mount_api_page(&server, page, body.to_string(), 1).await;
    }
    mount_api_page(&server, 4, "{}".into(), 0).await;
    let raws = tec(&server, json!({})).fetch(&ctx()).await.expect("fetch");
    assert_eq!(ids(&raws), ["/event/e1/", "/event/e2/", "/event/e3/"]);
}

#[tokio::test]
async fn a_failed_later_page_keeps_what_was_fetched() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    let body = fixture("scrapers/tec-cinema-museum/api-page-1.json");
    mount_api_page(&server, 1, body, 1).await;
    mount_status(&server, API_PATH, 500, 1).await;
    let ctx = ctx();
    let raws = tec(&server, json!({})).fetch(&ctx).await.expect("fetch");
    assert_eq!(ids(&raws), ids(&api_page("tec-cinema-museum", 1)));
    let errors = ctx.take_errors();
    assert!(
        errors.len() == 1 && errors[0].contains("API page 2") && errors[0].contains("HTTP 500"),
        "{errors:?}"
    );
}

#[tokio::test]
async fn a_malformed_later_page_keeps_what_was_fetched() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    let body = fixture("scrapers/tec-cinema-museum/api-page-1.json");
    mount_api_page(&server, 1, body, 1).await;
    mount_api_page(&server, 2, r#"{"message": "busy"}"#.into(), 1).await;
    let ctx = ctx();
    let raws = tec(&server, json!({})).fetch(&ctx).await.expect("fetch");
    assert_eq!(ids(&raws), ids(&api_page("tec-cinema-museum", 1)));
    let errors = ctx.take_errors();
    assert!(
        errors.len() == 1 && errors[0].contains("API page 2") && errors[0].contains("no events"),
        "{errors:?}"
    );
}

#[tokio::test]
async fn an_empty_programme_is_not_an_error() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    let body = json!({"events": [], "total": 0, "total_pages": 0});
    mount_api_page(&server, 1, body.to_string(), 1).await;
    let raws = tec(&server, json!({})).fetch(&ctx()).await.expect("fetch");
    assert!(raws.is_empty());
}

#[tokio::test]
async fn falls_back_to_the_list_view_when_the_api_is_off() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_status(&server, API_PATH, 404, 1).await;
    let html = fixture("scrapers/tec-housmans/list.html");
    mount_list(&server, "/events/", html, 1).await;
    let (_, config) = seed_row("tec-housmans");
    let raws = tec(&server, config).fetch(&ctx()).await.expect("fetch");
    assert_eq!(ids(&raws), ids(&list_view("tec-housmans")));
}

#[tokio::test]
async fn list_only_venues_never_call_the_api() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_status(&server, API_PATH, 200, 0).await;
    let html = fixture("scrapers/tec-bow-arts/list.html");
    mount_list(&server, "/bow-arts-events/", html, 1).await;
    let (_, config) = seed_row("tec-bow-arts");
    let raws = tec(&server, config).fetch(&ctx()).await.expect("fetch");
    assert_eq!(ids(&raws), ids(&list_view("tec-bow-arts")));
}

#[tokio::test]
async fn api_off_without_a_list_view_is_an_error() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_status(&server, API_PATH, 404, 1).await;
    let err = tec(&server, json!({})).fetch(&ctx()).await.unwrap_err();
    assert!(err.to_string().contains("HTTP 404"), "{err}");
}

#[tokio::test]
async fn server_errors_do_not_fall_back() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    mount_status(&server, API_PATH, 500, 1).await;
    mount_list(&server, "/events/", "unused".into(), 0).await;
    let err = tec(&server, json!({"list_path": "/events/"}))
        .fetch(&ctx())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("HTTP 500"), "{err}");
}

#[tokio::test]
async fn a_list_view_without_events_is_an_error() {
    let server = MockServer::start().await;
    mount_open_robots(&server).await;
    let html = "<html><body><h1>Events</h1></body></html>".to_string();
    mount_list(&server, "/events/", html, 1).await;
    let err = tec(&server, json!({"api_path": null, "list_path": "/events/"}))
        .fetch(&ctx())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no Event JSON-LD"), "{err}");
}

#[tokio::test]
async fn robots_disallow_blocks_the_source() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("User-agent: MuseNMingleBot\nDisallow: /\n"),
        )
        .mount(&server)
        .await;
    mount_status(&server, API_PATH, 200, 0).await;
    mount_list(&server, "/events/", "unused".into(), 0).await;
    let err = tec(&server, json!({"list_path": "/events/"}))
        .fetch(&ctx())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("robots.txt disallows"), "{err}");
}
