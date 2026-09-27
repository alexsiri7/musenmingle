//! Public-transport journey times (`GET /v1/transit`).
//!
//! The browser never talks to a journey planner: it sends its position to
//! us, we round it to a ~200 m grid ([`round_origin`]) and ask the
//! [`TransitProvider`] configured for the event's city (London → TfL, see
//! [`TflProvider`]). Answers are cached in memory for ~15 minutes per
//! (rounded origin, destination, 15-minute departure bucket), and planner
//! calls are throttled per client and globally. Nothing here logs
//! coordinates or the planner key; any failure (timeout, HTTP error, bad
//! JSON, throttling) is just "no transit time", and pages fall back to the
//! walking time.
//!
//! The provider is an authenticated API client, not a source: it never
//! fetches web pages. Another city can plug in a different provider (for
//! example Transitous or Google Routes) without changing the API, cache or
//! pages.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use chrono_tz::Europe::London;
use serde::{Deserialize, Serialize};

use crate::listing::{WALK_DETOUR, WALK_KMH};

/// Grid the origin is rounded to before it is cached or sent anywhere:
/// 0.002° of latitude (~220 m) by 0.003° of longitude (~210 m in London).
pub const GRID_LAT: f64 = 0.002;
pub const GRID_LNG: f64 = 0.003;

/// Below this straight-line distance, walking wins; we don't ask the planner.
pub const MIN_TRANSIT_KM: f64 = 1.5;

/// How long an answer is reused, and the departure bucket it belongs to.
pub const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
pub const BUCKET_SECS: i64 = 15 * 60;
/// Failures are remembered briefly so an outage doesn't hammer the planner.
pub const FAILURE_TTL: Duration = Duration::from_secs(2 * 60);
const CACHE_MAX: usize = 20_000;

/// Planner calls (cache misses) allowed per minute, per client and in all.
/// TfL's fair use is 500/min with a key; stay well below it.
pub const PER_CLIENT_PER_MIN: usize = 40;
pub const GLOBAL_PER_MIN: usize = 250;

/// The planner timeout: past this, pages show walking only.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatLng {
    pub lat: f64,
    pub lng: f64,
}

impl LatLng {
    /// Parse `lat,lng` (finite, in range).
    pub fn parse(s: &str) -> Option<Self> {
        let (a, b) = s.split_once(',')?;
        let lat: f64 = a.trim().parse().ok()?;
        let lng: f64 = b.trim().parse().ok()?;
        (lat.is_finite() && lng.is_finite() && lat.abs() <= 90.0 && lng.abs() <= 180.0)
            .then_some(Self { lat, lng })
    }
}

/// `origin` snapped to the [`GRID_LAT`] × [`GRID_LNG`] grid, and its cell.
pub fn round_origin(origin: LatLng) -> (LatLng, (i32, i32)) {
    let i = (origin.lat / GRID_LAT).round() as i32;
    let j = (origin.lng / GRID_LNG).round() as i32;
    let snap = |n: i32, g: f64| ((f64::from(n) * g) * 1e4).round() / 1e4;
    (
        LatLng {
            lat: snap(i, GRID_LAT),
            lng: snap(j, GRID_LNG),
        },
        (i, j),
    )
}

/// Great-circle distance in km.
pub fn distance_km(a: LatLng, b: LatLng) -> f64 {
    let r = 6371.0_f64;
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dp = p2 - p1;
    let dl = (b.lng - a.lng).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
}

/// Estimated walking minutes for a straight-line distance (the same pace
/// and detour as the Near me filter and the map's "≈ N min walk").
pub fn walk_minutes(km: f64) -> u32 {
    (km * WALK_DETOUR / WALK_KMH * 60.0).round().max(1.0) as u32
}

/// One journey-planner request. `from` is always the rounded origin.
#[derive(Debug, Clone)]
pub struct TransitQuery {
    pub from: LatLng,
    pub to: LatLng,
    pub to_name: Option<String>,
    pub depart: DateTime<Utc>,
}

/// The planner's best public-transport journey.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Journey {
    pub minutes: u32,
    /// Modes in order, deduplicated (`walking` included), e.g. `["walking", "tube"]`.
    pub modes: Vec<String>,
    /// First public-transport leg plus the walking, e.g. "Overground + 5 min walk".
    pub summary: String,
}

/// A link to full directions in a planner app or site.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TransitError {
    #[error("journey planner timed out")]
    Timeout,
    #[error("journey planner returned HTTP {0}")]
    Status(u16),
    #[error("journey planner request failed")]
    Request,
    #[error("journey planner response was not understood")]
    Parse,
}

/// A city's journey planner.
#[async_trait]
pub trait TransitProvider: Send + Sync {
    /// Shown as "by public transport (TfL)" and in the API's `provider`.
    fn name(&self) -> &str;
    /// The fastest public-transport journey, or `None` when there isn't one
    /// (only walking, or no route).
    async fn journey(&self, q: &TransitQuery) -> Result<Option<Journey>, TransitError>;
    /// Links to full directions ("Plan on TfL", "Citymapper").
    fn plan_links(&self, q: &TransitQuery) -> Vec<PlanLink>;
}

/// A city served by one provider: a bounding box both ends must be inside.
pub struct City {
    pub name: &'static str,
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
    pub provider: Arc<dyn TransitProvider>,
}

impl City {
    fn contains(&self, p: LatLng) -> bool {
        (self.south..=self.north).contains(&p.lat) && (self.west..=self.east).contains(&p.lng)
    }

    /// Greater London (and a little beyond), served by `provider`.
    pub fn london(provider: Arc<dyn TransitProvider>) -> Self {
        Self {
            name: "london",
            south: 51.25,
            west: -0.56,
            north: 51.72,
            east: 0.35,
            provider,
        }
    }
}

/// What [`Transit::lookup`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Journey {
        journey: Journey,
        provider: String,
        links: Vec<PlanLink>,
    },
    /// Close enough that walking is the answer; the planner wasn't asked.
    ShortWalk,
    /// The planner's best journey isn't quicker than walking.
    NotFaster,
    /// No provider for these places.
    OutsideArea,
    /// Failure, timeout, no route or throttled: show walking only.
    Unavailable,
}

type CacheKey = ((i32, i32), (i32, i32), i64);

struct Entry {
    until: Instant,
    journey: Option<Journey>,
}

#[derive(Default)]
struct Limiter {
    global: VecDeque<Instant>,
    per_client: HashMap<IpAddr, VecDeque<Instant>>,
}

impl Limiter {
    fn allow(&mut self, client: IpAddr, now: Instant, per_client: usize, global: usize) -> bool {
        let window = Duration::from_secs(60);
        let prune = |q: &mut VecDeque<Instant>| {
            while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
                q.pop_front();
            }
        };
        prune(&mut self.global);
        if self.global.len() >= global {
            return false;
        }
        if self.per_client.len() > 10_000 {
            self.per_client.retain(|_, q| {
                prune(q);
                !q.is_empty()
            });
        }
        let q = self.per_client.entry(client).or_default();
        prune(q);
        if q.len() >= per_client {
            return false;
        }
        q.push_back(now);
        self.global.push_back(now);
        true
    }
}

/// The transit service: providers per city, the cache and the throttle.
pub struct Transit {
    cities: Vec<City>,
    cache: Mutex<HashMap<CacheKey, Entry>>,
    limiter: Mutex<Limiter>,
    per_client_per_min: usize,
    global_per_min: usize,
}

impl Transit {
    pub fn new(cities: Vec<City>) -> Self {
        Self {
            cities,
            cache: Mutex::default(),
            limiter: Mutex::default(),
            per_client_per_min: PER_CLIENT_PER_MIN,
            global_per_min: GLOBAL_PER_MIN,
        }
    }

    /// Override the throttle (tests).
    pub fn with_limits(mut self, per_client_per_min: usize, global_per_min: usize) -> Self {
        self.per_client_per_min = per_client_per_min;
        self.global_per_min = global_per_min;
        self
    }

    /// The configured providers; `None` when every city is switched off.
    pub fn from_config(cfg: &TransitConfig) -> anyhow::Result<Option<Self>> {
        let mut cities = Vec::new();
        if cfg.london == ProviderChoice::Tfl {
            let tfl =
                TflProvider::new(&cfg.tfl_base_url, cfg.tfl_app_key.clone(), DEFAULT_TIMEOUT)?;
            cities.push(City::london(Arc::new(tfl)));
        }
        Ok((!cities.is_empty()).then(|| Self::new(cities)))
    }

    /// The journey from `origin` (rounded here, whatever the caller did) to
    /// `to`, departing now.
    pub async fn lookup(
        &self,
        origin: LatLng,
        to: LatLng,
        to_name: Option<&str>,
        client: IpAddr,
        now: DateTime<Utc>,
    ) -> Outcome {
        let (from, cell) = round_origin(origin);
        if distance_km(from, to) < MIN_TRANSIT_KM {
            return Outcome::ShortWalk;
        }
        let Some(city) = self
            .cities
            .iter()
            .find(|c| c.contains(to) && c.contains(from))
        else {
            return Outcome::OutsideArea;
        };
        let q = TransitQuery {
            from,
            to,
            to_name: to_name.map(str::to_owned),
            depart: now,
        };
        let dest = ((to.lat * 1e4).round() as i32, (to.lng * 1e4).round() as i32);
        let key: CacheKey = (cell, dest, now.timestamp().div_euclid(BUCKET_SECS));
        let cached = {
            let cache = self.cache.lock().expect("transit cache");
            cache
                .get(&key)
                .filter(|e| e.until > Instant::now())
                .map(|e| e.journey.clone())
        };
        let journey = match cached {
            Some(j) => j,
            None => {
                let allowed = self.limiter.lock().expect("transit limiter").allow(
                    client,
                    Instant::now(),
                    self.per_client_per_min,
                    self.global_per_min,
                );
                if !allowed {
                    return Outcome::Unavailable;
                }
                let (journey, ttl) = match city.provider.journey(&q).await {
                    Ok(j) => (j, CACHE_TTL),
                    Err(e) => {
                        tracing::warn!(city = city.name, error = %e, "transit lookup failed");
                        (None, FAILURE_TTL)
                    }
                };
                self.remember(key, journey.clone(), ttl);
                journey
            }
        };
        let Some(journey) = journey else {
            return Outcome::Unavailable;
        };
        if journey.minutes >= walk_minutes(distance_km(from, to)) {
            return Outcome::NotFaster;
        }
        Outcome::Journey {
            journey,
            provider: city.provider.name().to_owned(),
            links: city.provider.plan_links(&q),
        }
    }

    fn remember(&self, key: CacheKey, journey: Option<Journey>, ttl: Duration) {
        let now = Instant::now();
        let mut cache = self.cache.lock().expect("transit cache");
        if cache.len() >= CACHE_MAX {
            cache.retain(|_, e| e.until > now);
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(
            key,
            Entry {
                until: now + ttl,
                journey,
            },
        );
    }
}

/// Which provider serves a city.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderChoice {
    Off,
    Tfl,
}

/// `TRANSIT_LONDON` (`tfl`, the default, or `off`), `TFL_APP_KEY` (optional:
/// without it TfL's anonymous limits apply) and `TFL_BASE_URL` (tests).
#[derive(Debug, Clone)]
pub struct TransitConfig {
    pub london: ProviderChoice,
    pub tfl_app_key: Option<String>,
    pub tfl_base_url: String,
}

impl TransitConfig {
    pub fn parse(
        london: Option<&str>,
        tfl_app_key: Option<String>,
        tfl_base_url: Option<String>,
    ) -> anyhow::Result<Self> {
        let london = match london.map(str::to_ascii_lowercase).as_deref() {
            None | Some("tfl") => ProviderChoice::Tfl,
            Some("off") => ProviderChoice::Off,
            Some(other) => anyhow::bail!("TRANSIT_LONDON must be tfl or off, not {other:?}"),
        };
        Ok(Self {
            london,
            tfl_app_key,
            tfl_base_url: tfl_base_url.unwrap_or_else(|| TFL_BASE_URL.into()),
        })
    }
}

pub const TFL_BASE_URL: &str = "https://api.tfl.gov.uk";

/// Transport for London's Unified API Journey Planner
/// (`/Journey/JourneyResults/{from}/to/{to}`): Tube, bus, Overground,
/// Elizabeth line, DLR, tram and National Rail within London.
pub struct TflProvider {
    client: reqwest::Client,
    base: String,
    app_key: Option<String>,
}

impl TflProvider {
    pub fn new(base: &str, app_key: Option<String>, timeout: Duration) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("MuseNMingle/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            base: base.trim_end_matches('/').to_owned(),
            app_key,
        })
    }
}

fn coord(p: LatLng) -> String {
    format!("{:.4},{:.4}", p.lat, p.lng)
}

#[async_trait]
impl TransitProvider for TflProvider {
    fn name(&self) -> &str {
        "TfL"
    }

    async fn journey(&self, q: &TransitQuery) -> Result<Option<Journey>, TransitError> {
        let url = format!(
            "{}/Journey/JourneyResults/{}/to/{}",
            self.base,
            coord(q.from),
            coord(q.to)
        );
        let local = q.depart.with_timezone(&London);
        let mut params = vec![
            ("date", local.format("%Y%m%d").to_string()),
            ("time", local.format("%H%M").to_string()),
            ("timeIs", "Departing".to_string()),
        ];
        if let Some(k) = &self.app_key {
            params.push(("app_key", k.clone()));
        }
        // Errors are mapped, never displayed: reqwest's include the URL,
        // and so the key and coordinates.
        let resp = self
            .client
            .get(&url)
            .query(&params)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    TransitError::Timeout
                } else {
                    TransitError::Request
                }
            })?;
        let status = resp.status();
        if !status.is_success() {
            // 300 (ambiguous places) and 404 (no journey) mean "no route".
            return if status.as_u16() == 300 || status.as_u16() == 404 {
                Ok(None)
            } else {
                Err(TransitError::Status(status.as_u16()))
            };
        }
        let body = resp.bytes().await.map_err(|e| {
            if e.is_timeout() {
                TransitError::Timeout
            } else {
                TransitError::Request
            }
        })?;
        parse_tfl(&body)
    }

    fn plan_links(&self, q: &TransitQuery) -> Vec<PlanLink> {
        let mut tfl = url::Url::parse("https://tfl.gov.uk/plan-a-journey/results").expect("url");
        tfl.query_pairs_mut()
            .append_pair("InputFrom", &coord(q.from))
            .append_pair("From", &coord(q.from))
            .append_pair("InputTo", &coord(q.to))
            .append_pair("To", &coord(q.to));
        let mut cm = url::Url::parse("https://citymapper.com/directions").expect("url");
        {
            let mut p = cm.query_pairs_mut();
            p.append_pair("startcoord", &coord(q.from))
                .append_pair("endcoord", &coord(q.to));
            if let Some(n) = &q.to_name {
                p.append_pair("endname", n);
            }
        }
        vec![
            PlanLink {
                label: "Plan on TfL".into(),
                url: tfl.into(),
            },
            PlanLink {
                label: "Citymapper".into(),
                url: cm.into(),
            },
        ]
    }
}

#[derive(Deserialize)]
struct TflResult {
    #[serde(default)]
    journeys: Vec<TflJourney>,
}

#[derive(Deserialize)]
struct TflJourney {
    duration: u32,
    #[serde(default)]
    legs: Vec<TflLeg>,
}

#[derive(Deserialize)]
struct TflLeg {
    #[serde(default)]
    duration: u32,
    mode: TflMode,
    #[serde(default, rename = "routeOptions")]
    route_options: Vec<TflRouteOption>,
}

#[derive(Deserialize)]
struct TflMode {
    id: String,
}

#[derive(Deserialize)]
struct TflRouteOption {
    #[serde(default)]
    name: Option<String>,
}

/// The fastest journey that uses public transport, from a TfL
/// `ItineraryResult` body.
pub fn parse_tfl(body: &[u8]) -> Result<Option<Journey>, TransitError> {
    let r: TflResult = serde_json::from_slice(body).map_err(|_| TransitError::Parse)?;
    let best = r
        .journeys
        .into_iter()
        .filter(|j| j.legs.iter().any(|l| !is_walk(&l.mode.id)))
        .min_by_key(|j| j.duration);
    Ok(best.map(|j| {
        let mut modes: Vec<String> = Vec::new();
        for l in &j.legs {
            if !modes.contains(&l.mode.id) {
                modes.push(l.mode.id.clone());
            }
        }
        let walk: u32 = j
            .legs
            .iter()
            .filter(|l| is_walk(&l.mode.id))
            .map(|l| l.duration)
            .sum();
        let first = j
            .legs
            .iter()
            .find(|l| !is_walk(&l.mode.id))
            .map(|l| {
                let route = l
                    .route_options
                    .first()
                    .and_then(|r| r.name.as_deref())
                    .unwrap_or("");
                mode_label(&l.mode.id, route)
            })
            .unwrap_or_default();
        let summary = if walk > 0 {
            format!("{first} + {walk} min walk")
        } else {
            first
        };
        Journey {
            minutes: j.duration,
            modes,
            summary,
        }
    }))
}

fn is_walk(mode: &str) -> bool {
    mode == "walking"
}

/// "Tube", "Bus 38", "Elizabeth line", …
fn mode_label(mode: &str, route: &str) -> String {
    let base = match mode {
        "tube" => "Tube",
        "bus" => "Bus",
        "overground" => "Overground",
        "elizabeth-line" => "Elizabeth line",
        "dlr" => "DLR",
        "national-rail" => "National Rail",
        "tram" => "Tram",
        "river-bus" => "River bus",
        "cable-car" => "Cable car",
        "coach" => "Coach",
        other => {
            let mut s = other.replace('-', " ");
            if let Some(c) = s.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            return s;
        }
    };
    let route = route.trim();
    if mode == "bus" && !route.is_empty() && route.len() <= 6 {
        format!("{base} {route}")
    } else {
        base.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_is_snapped_to_a_200m_grid() {
        let a = round_origin(LatLng {
            lat: 51.53212,
            lng: -0.12348,
        });
        let b = round_origin(LatLng {
            lat: 51.53189,
            lng: -0.12301,
        });
        assert_eq!(a, b, "nearby points share a cell");
        assert_eq!(
            a.0,
            LatLng {
                lat: 51.532,
                lng: -0.123
            }
        );
        let far = round_origin(LatLng {
            lat: 51.5345,
            lng: -0.1234,
        });
        assert_ne!(a.1, far.1);
    }

    #[test]
    fn parses_lat_lng() {
        assert_eq!(
            LatLng::parse("51.5,-0.12"),
            Some(LatLng {
                lat: 51.5,
                lng: -0.12
            })
        );
        for bad in ["", "51.5", "x,y", "91,0", "NaN,0", "51,200"] {
            assert_eq!(LatLng::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn walk_minutes_match_the_map() {
        // map.mjs: max(1, round(km * 1.3 / 5 * 60)).
        assert_eq!(walk_minutes(2.2), 34);
        assert_eq!(walk_minutes(0.0), 1);
    }

    #[test]
    fn mode_labels() {
        assert_eq!(
            mode_label("elizabeth-line", "Elizabeth line"),
            "Elizabeth line"
        );
        assert_eq!(mode_label("bus", "38"), "Bus 38");
        assert_eq!(mode_label("tube", "Northern"), "Tube");
        assert_eq!(mode_label("replacement-bus", ""), "Replacement bus");
    }
}
