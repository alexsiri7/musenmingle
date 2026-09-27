//! Public-transport times (#169): the TfL response parser (saved fixture),
//! the provider against a mock TfL, origin rounding + the cache, the
//! throttle, the silent fallback, and `GET /v1/transit` + the event page.

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::SuggestionConfig;
use musenmingle::suggestions::Suggestions;
use musenmingle::transit::{
    City, LatLng, Outcome, TflProvider, Transit, TransitProvider, TransitQuery, parse_tfl,
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use wiremock::matchers::{method, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Trafalgar Square → Angel (~2.9 km): the fixture's journey.
const FROM: LatLng = LatLng {
    lat: 51.5074,
    lng: -0.1278,
};
const TO: LatLng = LatLng {
    lat: 51.532,
    lng: -0.106,
};
const CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));

fn now() -> DateTime<Utc> {
    "2026-09-27T14:04:00Z".parse().unwrap()
}

fn fixture() -> String {
    common::fixture("transit/tfl_journey.json")
}

fn transit(server: &MockServer, timeout: Duration) -> Transit {
    let tfl = TflProvider::new(&server.uri(), Some("SECRET".into()), timeout).unwrap();
    Transit::new(vec![City::london(Arc::new(tfl))])
}

#[test]
fn fixture_parses_to_the_fastest_public_transport_journey() {
    let j = parse_tfl(fixture().as_bytes()).unwrap().unwrap();
    assert_eq!(j.minutes, 29);
    assert_eq!(j.modes, ["walking", "tube"]);
    assert_eq!(j.summary, "Tube + 16 min walk");
}

#[test]
fn walking_only_or_empty_answers_are_no_journey() {
    let walk_only =
        r#"{"journeys":[{"duration":20,"legs":[{"duration":20,"mode":{"id":"walking"}}]}]}"#;
    assert_eq!(parse_tfl(walk_only.as_bytes()).unwrap(), None);
    assert_eq!(parse_tfl(b"{}").unwrap(), None);
    assert!(parse_tfl(b"<html>").is_err());
}

#[tokio::test]
async fn tfl_is_asked_with_the_rounded_origin_and_key_then_cached() {
    let server = MockServer::start().await;
    // 51.5074,-0.1278 snaps to 51.508,-0.129 (the 200 m grid).
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/Journey/JourneyResults/51\.5080,-0\.1290/to/51\.5320,-0\.1060$",
        ))
        .and(query_param("app_key", "SECRET"))
        .and(query_param("timeIs", "Departing"))
        .and(query_param("date", "20260927"))
        .and(query_param("time", "1504"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture()))
        .expect(1)
        .mount(&server)
        .await;
    let t = transit(&server, Duration::from_secs(3));
    let first = t.lookup(FROM, TO, Some("Venue"), CLIENT, now()).await;
    let Outcome::Journey {
        journey,
        provider,
        links,
    } = &first
    else {
        panic!("expected a journey, got {first:?}");
    };
    assert_eq!(journey.minutes, 29);
    assert_eq!(provider, "TfL");
    assert_eq!(links[0].label, "Plan on TfL");
    assert!(
        links[0]
            .url
            .starts_with("https://tfl.gov.uk/plan-a-journey/results?")
    );
    assert!(links[1].url.contains("endname=Venue"));
    assert!(!links.iter().any(|l| l.url.contains("SECRET")));
    // A nearby point in the same cell, later in the same 15 minutes: cached.
    let near = LatLng {
        lat: 51.5078,
        lng: -0.1284,
    };
    let later = now() + chrono::Duration::minutes(5);
    assert_eq!(
        t.lookup(near, TO, Some("Venue"), CLIENT, later).await,
        first
    );
}

#[tokio::test]
async fn short_walks_never_call_the_planner() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture()))
        .expect(0)
        .mount(&server)
        .await;
    let t = transit(&server, Duration::from_secs(3));
    let close = LatLng {
        lat: 51.525,
        lng: -0.106,
    };
    assert_eq!(
        t.lookup(close, TO, None, CLIENT, now()).await,
        Outcome::ShortWalk
    );
    let paris = LatLng {
        lat: 48.85,
        lng: 2.35,
    };
    assert_eq!(
        t.lookup(paris, TO, None, CLIENT, now()).await,
        Outcome::OutsideArea
    );
}

#[tokio::test]
async fn slow_or_failing_planner_falls_back_to_walking() {
    let slow = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixture())
                .set_delay(Duration::from_secs(2)),
        )
        .mount(&slow)
        .await;
    let t = transit(&slow, Duration::from_millis(200));
    assert_eq!(
        t.lookup(FROM, TO, None, CLIENT, now()).await,
        Outcome::Unavailable
    );

    let broken = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1) // the failure is remembered briefly
        .mount(&broken)
        .await;
    let t = transit(&broken, Duration::from_secs(3));
    for _ in 0..2 {
        assert_eq!(
            t.lookup(FROM, TO, None, CLIENT, now()).await,
            Outcome::Unavailable
        );
    }
}

#[tokio::test]
async fn slower_than_walking_is_not_shown() {
    let server = MockServer::start().await;
    let slow_bus = r#"{"journeys":[{"duration":90,"legs":[{"duration":90,"mode":{"id":"bus"},"routeOptions":[{"name":"38"}]}]}]}"#;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(slow_bus))
        .mount(&server)
        .await;
    let t = transit(&server, Duration::from_secs(3));
    assert_eq!(
        t.lookup(FROM, TO, None, CLIENT, now()).await,
        Outcome::NotFaster
    );
}

#[tokio::test]
async fn planner_calls_are_throttled_per_client() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture()))
        .expect(2)
        .mount(&server)
        .await;
    let t = transit(&server, Duration::from_secs(3)).with_limits(1, 10);
    assert!(matches!(
        t.lookup(FROM, TO, None, CLIENT, now()).await,
        Outcome::Journey { .. }
    ));
    let elsewhere = LatLng {
        lat: 51.49,
        lng: -0.15,
    };
    assert_eq!(
        t.lookup(elsewhere, TO, None, CLIENT, now()).await,
        Outcome::Unavailable
    );
    let other = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 8));
    assert!(matches!(
        t.lookup(elsewhere, TO, None, other, now()).await,
        Outcome::Journey { .. }
    ));
}

/// A provider that always has an 18-minute Overground journey.
struct Fixed;

#[async_trait::async_trait]
impl TransitProvider for Fixed {
    fn name(&self) -> &str {
        "Test"
    }
    async fn journey(
        &self,
        _q: &TransitQuery,
    ) -> Result<Option<musenmingle::transit::Journey>, musenmingle::transit::TransitError> {
        Ok(Some(musenmingle::transit::Journey {
            minutes: 18,
            modes: vec!["walking".into(), "overground".into()],
            summary: "Overground + 5 min walk".into(),
        }))
    }
    fn plan_links(&self, _q: &TransitQuery) -> Vec<musenmingle::transit::PlanLink> {
        Vec::new()
    }
}

fn router(pool: PgPool, transit: Option<Transit>) -> Router {
    let suggestions = Suggestions::new(
        SuggestionConfig {
            ip_salt: Some("salt".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let settings = ApiSettings {
        github_repo: "owner/repo".into(),
        cors_origins: Vec::new(),
    };
    musenmingle::api::router_with(pool, suggestions, settings, None, transit)
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 1234))))
}

async fn get(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, bytes.to_vec())
}

async fn insert(pool: &PgPool, title: &str, at: Option<(f64, f64)>) -> uuid::Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events (title, venue_name, starts_at, category, lat, lng, dedupe_key)
         VALUES ($1, 'The Venue', now() + interval '1 day', 'exhibition', $2, $3, $1)
         RETURNING id",
    )
    .bind(title)
    .bind(at.map(|a| a.0))
    .bind(at.map(|a| a.1))
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn transit_endpoint_and_event_page() {
    let Some(db) = TestDb::create("transit_endpoint_and_event_page").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let angel = insert(&pool, "At Angel", Some((TO.lat, TO.lng))).await;
    let nowhere = insert(&pool, "No place", None).await;
    let app = router(
        pool.clone(),
        Some(Transit::new(vec![City::london(Arc::new(Fixed))])),
    );

    let (status, headers, body) = get(
        &app,
        &format!("/v1/transit?from=51.5074,-0.1278&event={angel}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "private, max-age=300");
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["transit"]["minutes"], 18);
    assert_eq!(v["transit"]["summary"], "Overground + 5 min walk");
    assert_eq!(v["transit"]["provider"], "Test");
    assert!(v["walk"]["minutes"].as_u64().unwrap() > 18);

    let (_, _, body) = get(
        &app,
        &format!("/v1/transit?from=51.531,-0.106&event={angel}"),
    )
    .await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "short_walk");
    assert!(v["transit"].is_null());

    let (_, _, body) = get(
        &app,
        &format!("/v1/transit?from=51.5074,-0.1278&event={nowhere}"),
    )
    .await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "no_location");

    for bad in [
        format!("/v1/transit?from=nope&event={angel}"),
        "/v1/transit?from=51.5,-0.1&event=nope".to_string(),
    ] {
        assert_eq!(get(&app, &bad).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    let unknown = format!("/v1/transit?from=51.5,-0.1&event={}", uuid::Uuid::new_v4());
    assert_eq!(get(&app, &unknown).await.0, StatusCode::NOT_FOUND);

    // Without a provider: walking only.
    let off = router(pool.clone(), None);
    let (_, _, body) = get(
        &off,
        &format!("/v1/transit?from=51.5074,-0.1278&event={angel}"),
    )
    .await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "unavailable");
    assert!(v["walk"]["minutes"].is_u64());

    // The event page may ask for location, and renders "Getting there"
    // hidden (JavaScript fills it); events without a place don't get it.
    let (status, headers, body) = get(&app, &format!("/events/{angel}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers["permissions-policy"]
            .to_str()
            .unwrap()
            .contains("geolocation=(self)")
    );
    let html = String::from_utf8(body).unwrap();
    assert!(html.contains(&format!(
        r#"class="getting-there" data-transit-event="{angel}""#
    )));
    assert!(html.contains("data-transit-locate hidden"));
    let (_, _, body) = get(&app, &format!("/events/{nowhere}")).await;
    assert!(!String::from_utf8(body).unwrap().contains("getting-there"));

    db.drop_db().await;
}
