//! Polite HTTP fetching for sources.
//!
//! [`FetchContext`] is the ONLY way a [`crate::sources::Source`] may talk to
//! the network. It enforces, for every request:
//!
//! * a descriptive bot User-Agent ([`user_agent`]);
//! * robots.txt, fetched once per origin and cached for the lifetime of the
//!   context (one ingest run). Per RFC 9309: 2xx → parse the rules, 4xx →
//!   everything allowed, 5xx / network error → everything disallowed.
//!   A `Crawl-delay` longer than the configured interval is honoured,
//!   counting the robots.txt fetch itself as a request;
//! * a per-domain rate limit (default one request every 2 s per host);
//! * public addresses only ([`crate::netguard`]): http(s) URLs whose host
//!   resolves only to public IPs, checked before the request and again on
//!   each of at most [`MAX_REDIRECTS`] redirects;
//! * bodies of at most [`MAX_BODY_BYTES`] (or a caller's lower limit).
//!
//! The underlying `reqwest::Client` is private on purpose; do not add an
//! accessor for it.
//!
//! While capture is on ([`FetchContext::start_capture`]), the bodies of
//! [`FetchContext::get_text`] and [`FetchContext::get_json`] responses are
//! also kept for the scraper QA check (`crate::qa`), which compares them with
//! what the run extracted: bodies only, the client stays private.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use texting_robots::Robot;
use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

use crate::config::RateLimitConfig;
use crate::netguard::{self, GuardedResolver};

/// Robots.txt product token.
pub const ROBOTS_AGENT: &str = "MuseNMingleBot";

/// Where site owners learn what the bot does and how to reach us (the
/// "For venues" section of the public About page).
pub const BOT_INFO_URL: &str = "https://musenmingle.interstellarai.net/about#for-venues";

/// The User-Agent sent with every request.
pub fn user_agent() -> String {
    format!("{ROBOTS_AGENT}/{} (+{BOT_INFO_URL})", crate::VERSION)
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("robots.txt disallows {0}")]
    RobotsDisallowed(String),
    #[error("HTTP {status} from {url}")]
    Status { status: StatusCode, url: String },
    #[error("request to {url} failed: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("invalid URL {0:?}")]
    InvalidUrl(String),
    #[error("could not decode response from {url}: {message}")]
    Decode { url: String, message: String },
    #[error("response from {url} is larger than {limit} bytes")]
    TooLarge { url: String, limit: usize },
    #[error("{reason}: {url}")]
    Blocked { reason: String, url: String },
}

/// A body fetched by [`FetchContext::get_bytes_limited`].
#[derive(Debug, Clone)]
pub struct FetchedBytes {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// A response body kept while capture is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedPage {
    /// Redacted ([`redact`]): query strings may hold API keys.
    pub url: String,
    pub body: String,
    /// Fetched with [`FetchContext::get_json`] (an API response, not HTML).
    pub json: bool,
}

/// Redirects followed per request; each target is checked again.
pub const MAX_REDIRECTS: usize = 3;
/// The largest text or JSON body [`FetchContext`] reads.
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// The [`netguard::Blocked`] reason anywhere in a request error's chain.
fn blocked_reason(e: &reqwest::Error) -> Option<String> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = source {
        if let Some(blocked) = err.downcast_ref::<netguard::Blocked>() {
            return Some(blocked.to_string());
        }
        source = err.source();
    }
    None
}

/// Bodies larger than this are not captured.
pub const MAX_CAPTURE_BYTES: usize = 2_000_000;
/// At most this many pages are captured per capture.
pub const MAX_CAPTURED_PAGES: usize = 60;

/// Strip the query string (which may contain API keys) for logs and errors.
pub fn redact(url: &Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    u.to_string()
}

/// A parsed robots.txt policy for one origin.
#[derive(Debug)]
pub enum RobotsPolicy {
    AllowAll,
    DisallowAll,
    Rules(Box<Robot>),
}

impl RobotsPolicy {
    /// Build a policy from a robots.txt HTTP status and body.
    pub fn from_response(status: StatusCode, body: &[u8]) -> Self {
        if status.is_success() {
            match Robot::new(ROBOTS_AGENT, body) {
                Ok(r) => RobotsPolicy::Rules(Box::new(r)),
                // Unparseable robots.txt: be conservative.
                Err(_) => RobotsPolicy::DisallowAll,
            }
        } else if status.is_client_error() {
            RobotsPolicy::AllowAll
        } else {
            RobotsPolicy::DisallowAll
        }
    }

    pub fn allowed(&self, url: &Url) -> bool {
        match self {
            RobotsPolicy::AllowAll => true,
            RobotsPolicy::DisallowAll => false,
            RobotsPolicy::Rules(r) => r.allowed(url.as_str()),
        }
    }

    pub fn crawl_delay(&self) -> Option<Duration> {
        match self {
            RobotsPolicy::Rules(r) => r
                .delay
                .filter(|d| d.is_finite() && *d > 0.0)
                .map(|d| Duration::from_secs_f32(d.min(60.0))),
            _ => None,
        }
    }
}

/// Per-host rate limiter using slot reservation, so concurrent callers are
/// serialised correctly. Uses `tokio::time` so it can be tested with paused
/// time.
#[derive(Debug)]
pub struct RateLimiter {
    config: RateLimitConfig,
    next_slot: StdMutex<HashMap<String, Instant>>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            next_slot: StdMutex::new(HashMap::new()),
        }
    }

    /// Wait until a request to `host` is permitted and return the slot it
    /// was given. `min_interval` (e.g. a robots.txt Crawl-delay) raises the
    /// configured interval if larger.
    pub async fn acquire(&self, host: &str, min_interval: Option<Duration>) -> Instant {
        let interval = self
            .config
            .interval_for(host)
            .max(min_interval.unwrap_or(Duration::ZERO));
        let slot = {
            let mut map = self.next_slot.lock().expect("rate limiter poisoned");
            let now = Instant::now();
            let slot = map.get(host).copied().map_or(now, |n| n.max(now));
            map.insert(host.to_string(), slot + interval);
            slot
        };
        tokio::time::sleep_until(slot).await;
        slot
    }

    /// Hold the next request to `host` until at least `interval` after the
    /// request made at `slot`, for an interval learned only after that
    /// request was sent (a Crawl-delay read from robots.txt).
    pub fn space_after(&self, host: &str, slot: Instant, interval: Duration) {
        let mut map = self.next_slot.lock().expect("rate limiter poisoned");
        let next = map.entry(host.to_string()).or_insert(slot);
        *next = (*next).max(slot + interval);
    }
}

/// Everything a source needs to fetch data politely.
pub struct FetchContext {
    client: reqwest::Client,
    limiter: RateLimiter,
    robots: Mutex<HashMap<String, Arc<RobotsPolicy>>>,
    soft_errors: StdMutex<Vec<String>>,
    capture: StdMutex<Option<Vec<CapturedPage>>>,
    allow_loopback: bool,
}

impl FetchContext {
    pub fn new(rate_limit: RateLimitConfig) -> Result<Self, reqwest::Error> {
        Self::build(rate_limit, false)
    }

    /// For tests against local mock servers only: loopback addresses are
    /// allowed; every other rule still applies.
    pub fn new_allowing_loopback(rate_limit: RateLimitConfig) -> Result<Self, reqwest::Error> {
        Self::build(rate_limit, true)
    }

    fn build(rate_limit: RateLimitConfig, allow_loopback: bool) -> Result<Self, reqwest::Error> {
        let redirects = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > MAX_REDIRECTS {
                attempt.error(netguard::Blocked("too many redirects".into()))
            } else if let Err(e) = netguard::check_url(attempt.url(), allow_loopback) {
                attempt.error(e)
            } else {
                attempt.follow()
            }
        });
        let client = reqwest::Client::builder()
            .user_agent(user_agent())
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .dns_resolver(GuardedResolver { allow_loopback })
            .redirect(redirects)
            // A proxy would resolve names itself, past the resolver.
            .no_proxy()
            .build()?;
        Ok(Self {
            client,
            allow_loopback,
            limiter: RateLimiter::new(rate_limit),
            robots: Mutex::new(HashMap::new()),
            soft_errors: StdMutex::new(Vec::new()),
            capture: StdMutex::new(None),
        })
    }

    /// Record a non-fatal error (e.g. one detail page of many failed). The
    /// runner counts these into `source_runs.errors`.
    pub fn report_error(&self, message: impl Into<String>) {
        let message = message.into();
        tracing::warn!(%message, "source reported a non-fatal error");
        self.soft_errors
            .lock()
            .expect("soft errors poisoned")
            .push(message);
    }

    /// Drain the non-fatal errors reported since the last call.
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.soft_errors.lock().expect("soft errors poisoned"))
    }

    /// Start keeping the bodies of `get_text` / `get_json` responses.
    pub fn start_capture(&self) {
        *self.capture.lock().expect("capture poisoned") = Some(Vec::new());
    }

    /// Stop capturing and return what was kept, in fetch order.
    pub fn finish_capture(&self) -> Vec<CapturedPage> {
        self.capture
            .lock()
            .expect("capture poisoned")
            .take()
            .unwrap_or_default()
    }

    fn keep(&self, url: &Url, body: &str, json: bool) {
        let mut capture = self.capture.lock().expect("capture poisoned");
        if let Some(pages) = capture.as_mut()
            && body.len() <= MAX_CAPTURE_BYTES
            && pages.len() < MAX_CAPTURED_PAGES
        {
            pages.push(CapturedPage {
                url: redact(url),
                body: body.to_string(),
                json,
            });
        }
    }

    fn origin_and_host(url: &Url) -> Result<(String, String), FetchError> {
        let host = url
            .host_str()
            .ok_or_else(|| FetchError::InvalidUrl(url.to_string()))?
            .to_ascii_lowercase();
        Ok((url.origin().ascii_serialization(), host))
    }

    async fn robots_for(&self, url: &Url) -> Result<Arc<RobotsPolicy>, FetchError> {
        let (origin, host) = Self::origin_and_host(url)?;
        let mut cache = self.robots.lock().await;
        if let Some(p) = cache.get(&origin) {
            return Ok(p.clone());
        }
        let robots_url = Url::parse(&format!("{origin}/robots.txt"))
            .map_err(|_| FetchError::InvalidUrl(origin.clone()))?;
        let slot = self.limiter.acquire(&host, None).await;
        let policy = match self.client.get(robots_url.clone()).send().await {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.bytes().await.unwrap_or_default();
                RobotsPolicy::from_response(status, &body)
            }
            Err(e) => {
                tracing::warn!(url = %robots_url, error = %e, "robots.txt unreachable; disallowing origin");
                RobotsPolicy::DisallowAll
            }
        };
        tracing::debug!(%origin, ?policy, "loaded robots.txt");
        if let Some(delay) = policy.crawl_delay() {
            self.limiter.space_after(&host, slot, delay);
        }
        let policy = Arc::new(policy);
        cache.insert(origin, policy.clone());
        Ok(policy)
    }

    /// GET `url` after the address, robots.txt and rate-limit checks.
    /// Non-2xx statuses are errors.
    pub async fn get(&self, url: &Url) -> Result<reqwest::Response, FetchError> {
        netguard::check_url(url, self.allow_loopback).map_err(|e| FetchError::Blocked {
            reason: e.to_string(),
            url: redact(url),
        })?;
        let robots = self.robots_for(url).await?;
        if !robots.allowed(url) {
            return Err(FetchError::RobotsDisallowed(redact(url)));
        }
        let (_, host) = Self::origin_and_host(url)?;
        self.limiter.acquire(&host, robots.crawl_delay()).await;
        tracing::debug!(url = %redact(url), "GET");
        let resp =
            self.client
                .get(url.clone())
                .send()
                .await
                .map_err(|e| match blocked_reason(&e) {
                    Some(reason) => FetchError::Blocked {
                        reason,
                        url: redact(url),
                    },
                    None => FetchError::Http {
                        url: redact(url),
                        source: e.without_url(),
                    },
                })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(FetchError::Status {
                status,
                url: redact(url),
            });
        }
        Ok(resp)
    }

    /// GET and return the body as text (invalid UTF-8 replaced).
    pub async fn get_text(&self, url: &Url) -> Result<String, FetchError> {
        let fetched = self.get_bytes_limited(url, MAX_BODY_BYTES).await?;
        let body = String::from_utf8_lossy(&fetched.bytes).into_owned();
        self.keep(url, &body, false);
        Ok(body)
    }

    /// GET a binary body (e.g. an image for the thumbnailer), refusing
    /// bodies over `limit` bytes: by `Content-Length` before reading, and
    /// while streaming otherwise. Same robots.txt / rate-limit rules as
    /// [`FetchContext::get`].
    pub async fn get_bytes_limited(
        &self,
        url: &Url,
        limit: usize,
    ) -> Result<FetchedBytes, FetchError> {
        let too_large = || FetchError::TooLarge {
            url: redact(url),
            limit,
        };
        let mut resp = self.get(url).await?;
        if resp
            .content_length()
            .is_some_and(|n| n > u64::try_from(limit).unwrap_or(u64::MAX))
        {
            return Err(too_large());
        }
        let header = |name: reqwest::header::HeaderName| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let content_type = header(reqwest::header::CONTENT_TYPE);
        let etag = header(reqwest::header::ETAG);
        let last_modified = header(reqwest::header::LAST_MODIFIED);
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| FetchError::Http {
            url: redact(url),
            source: e.without_url(),
        })? {
            if bytes.len() + chunk.len() > limit {
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(FetchedBytes {
            bytes,
            content_type,
            etag,
            last_modified,
        })
    }

    /// GET and decode the body as JSON.
    pub async fn get_json<T: DeserializeOwned>(&self, url: &Url) -> Result<T, FetchError> {
        let bytes = self.get_bytes_limited(url, MAX_BODY_BYTES).await?.bytes;
        self.keep(url, &String::from_utf8_lossy(&bytes), true);
        serde_json::from_slice(&bytes).map_err(|e| FetchError::Decode {
            url: redact(url),
            message: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERPENTINE_ROBOTS: &[u8] =
        include_bytes!("../tests/fixtures/scrapers/serpentine-galleries/robots.txt");

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn user_agent_is_descriptive() {
        let ua = user_agent();
        assert_eq!(
            ua,
            format!(
                "MuseNMingleBot/{} (+https://musenmingle.interstellarai.net/about#for-venues)",
                crate::VERSION
            )
        );
    }

    #[test]
    fn robots_serpentine_fixture_allows_whats_on() {
        let p = RobotsPolicy::from_response(StatusCode::OK, SERPENTINE_ROBOTS);
        assert!(p.allowed(&u("https://www.serpentinegalleries.org/whats-on/")));
        assert!(p.allowed(&u(
            "https://www.serpentinegalleries.org/whats-on/amar-kanwar-exhibition/"
        )));
        assert!(!p.allowed(&u("https://www.serpentinegalleries.org/cms/wp-admin/")));
        assert!(p.allowed(&u(
            "https://www.serpentinegalleries.org/cms/wp-admin/admin-ajax.php"
        )));
    }

    #[test]
    fn robots_rules_for_our_agent_and_wildcard() {
        let txt = b"User-agent: *\nDisallow: /private\n\nUser-agent: MuseNMingleBot\nDisallow: /events/secret\nAllow: /events/secret/ok\nCrawl-delay: 5\n";
        let p = RobotsPolicy::from_response(StatusCode::OK, txt);
        // Our specific group wins over `*`, so /private is allowed for us.
        assert!(p.allowed(&u("https://x.test/private")));
        assert!(!p.allowed(&u("https://x.test/events/secret/a")));
        assert!(p.allowed(&u("https://x.test/events/secret/ok")));
        assert_eq!(p.crawl_delay(), Some(Duration::from_secs(5)));

        let all = RobotsPolicy::from_response(StatusCode::OK, b"User-agent: *\nDisallow: /\n");
        assert!(!all.allowed(&u("https://x.test/anything")));
        assert!(all.allowed(&u("https://x.test/robots.txt")));
    }

    #[test]
    fn robots_status_semantics() {
        assert!(
            RobotsPolicy::from_response(StatusCode::NOT_FOUND, b"").allowed(&u("https://x.test/a"))
        );
        assert!(
            !RobotsPolicy::from_response(StatusCode::SERVICE_UNAVAILABLE, b"")
                .allowed(&u("https://x.test/a"))
        );
    }

    #[test]
    fn redact_strips_query() {
        assert_eq!(
            redact(&u("https://api.test/x.json?apikey=SECRET&page=1")),
            "https://api.test/x.json"
        );
    }

    #[tokio::test]
    async fn capture_keeps_text_and_json_bodies_only_while_on() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<p>Hi</p>"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"a":1}"#))
            .mount(&server)
            .await;
        let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
        let page = u(&format!("{}/page?key=SECRET", server.uri()));
        let api = u(&format!("{}/api?apikey=SECRET", server.uri()));

        ctx.get_text(&page).await.unwrap();
        assert!(ctx.finish_capture().is_empty(), "capture is off by default");

        ctx.start_capture();
        ctx.get_text(&page).await.unwrap();
        let _: serde_json::Value = ctx.get_json(&api).await.unwrap();
        let pages = ctx.finish_capture();
        assert_eq!(
            pages,
            vec![
                CapturedPage {
                    url: format!("{}/page", server.uri()),
                    body: "<p>Hi</p>".into(),
                    json: false,
                },
                CapturedPage {
                    url: format!("{}/api", server.uri()),
                    body: r#"{"a":1}"#.into(),
                    json: true,
                },
            ]
        );

        ctx.get_text(&page).await.unwrap();
        assert!(
            ctx.finish_capture().is_empty(),
            "finish_capture turns it off"
        );
    }

    #[tokio::test]
    async fn strict_context_refuses_loopback_without_any_request() {
        let server = wiremock::MockServer::start().await;
        let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
        let err = ctx
            .get_text(&u(&format!("{}/page?key=SECRET", server.uri())))
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Blocked { .. }), "{err}");
        assert!(!err.to_string().contains("SECRET"), "{err}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    async fn redirecting_server(hops: &[(&str, String)]) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(path("/robots.txt"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        for (from, to) in hops {
            let to = to.replace("{base}", &server.uri()).replace(
                "{localhost}",
                &server.uri().replace("127.0.0.1", "localhost"),
            );
            Mock::given(method("GET"))
                .and(path(*from))
                .respond_with(ResponseTemplate::new(302).insert_header("location", to.as_str()))
                .mount(&server)
                .await;
        }
        server
    }

    #[tokio::test]
    async fn redirects_to_non_public_targets_are_refused() {
        let server = redirecting_server(&[
            ("/to-private", "http://10.0.0.1/x".into()),
            (
                "/to-metadata",
                "http://169.254.169.254/latest/meta-data/".into(),
            ),
            ("/to-railway", "http://api.railway.internal/".into()),
            ("/to-localhost-name", "{localhost}/ok".into()),
            ("/to-ftp", "ftp://example.org/".into()),
        ])
        .await;
        let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
        for from in [
            "/to-private",
            "/to-metadata",
            "/to-railway",
            "/to-localhost-name",
            "/to-ftp",
        ] {
            let err = ctx
                .get_text(&u(&format!("{}{from}", server.uri())))
                .await
                .unwrap_err();
            assert!(matches!(err, FetchError::Blocked { .. }), "{from}: {err}");
        }
    }

    #[tokio::test]
    async fn at_most_three_redirects_are_followed() {
        let hop = |to: &str| format!("{{base}}{to}");
        let server = redirecting_server(&[
            ("/a1", hop("/a2")),
            ("/a2", hop("/a3")),
            ("/a3", hop("/ok")),
            ("/b1", hop("/b2")),
            ("/b2", hop("/b3")),
            ("/b3", hop("/b4")),
            ("/b4", hop("/ok")),
        ])
        .await;
        let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
        let three = ctx.get_text(&u(&format!("{}/a1", server.uri()))).await;
        assert_eq!(three.unwrap(), "ok");
        let four = ctx
            .get_text(&u(&format!("{}/b1", server.uri())))
            .await
            .unwrap_err();
        assert!(matches!(four, FetchError::Blocked { .. }), "{four}");
    }

    #[tokio::test]
    async fn text_and_json_bodies_over_the_cap_are_refused() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b' '; MAX_BODY_BYTES + 1]))
            .mount(&server)
            .await;
        let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
        let url = u(&format!("{}/big", server.uri()));
        let text = ctx.get_text(&url).await.unwrap_err();
        assert!(matches!(text, FetchError::TooLarge { .. }), "{text}");
        let json = ctx.get_json::<serde_json::Value>(&url).await.unwrap_err();
        assert!(matches!(json, FetchError::TooLarge { .. }), "{json}");
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_spaces_requests_per_host() {
        let rl = RateLimiter::new(RateLimitConfig::default());
        let start = Instant::now();
        rl.acquire("a.test", None).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        rl.acquire("a.test", None).await;
        assert_eq!(start.elapsed(), Duration::from_secs(2));
        // Another host is independent.
        let t = Instant::now();
        rl.acquire("b.test", None).await;
        assert_eq!(t.elapsed(), Duration::ZERO);
        // Crawl-delay larger than the configured interval wins.
        rl.acquire("a.test", Some(Duration::from_secs(10))).await;
        assert_eq!(start.elapsed(), Duration::from_secs(4));
        rl.acquire("a.test", None).await;
        assert_eq!(start.elapsed(), Duration::from_secs(14));
    }

    #[test]
    fn robots_crawl_delay_group_precedence_and_fractions() {
        let ours_wins =
            b"User-agent: *\nCrawl-delay: 30\n\nUser-agent: MuseNMingleBot\nCrawl-delay: 5\n";
        let p = RobotsPolicy::from_response(StatusCode::OK, ours_wins);
        assert_eq!(p.crawl_delay(), Some(Duration::from_secs(5)));
        let wildcard =
            RobotsPolicy::from_response(StatusCode::OK, b"User-agent: *\nCrawl-delay: 7\n");
        assert_eq!(wildcard.crawl_delay(), Some(Duration::from_secs(7)));
        let fractional =
            RobotsPolicy::from_response(StatusCode::OK, b"User-agent: *\nCrawl-delay: 2.5\n");
        assert_eq!(fractional.crawl_delay(), Some(Duration::from_millis(2500)));
    }

    #[tokio::test(start_paused = true)]
    async fn crawl_delay_spaces_the_first_request_after_robots_txt() {
        let rl = RateLimiter::new(RateLimitConfig::default());
        let delay = Duration::from_secs(20);
        let start = Instant::now();
        let robots_slot = rl.acquire("a.test", None).await;
        rl.space_after("a.test", robots_slot, delay);
        rl.acquire("a.test", Some(delay)).await;
        assert_eq!(start.elapsed(), delay);
        rl.acquire("a.test", Some(delay)).await;
        assert_eq!(start.elapsed(), 2 * delay);
    }

    #[tokio::test(start_paused = true)]
    async fn crawl_delay_below_the_interval_does_not_speed_us_up() {
        let rl = RateLimiter::new(RateLimitConfig::default());
        let delay = Duration::from_secs(1);
        let start = Instant::now();
        let robots_slot = rl.acquire("a.test", None).await;
        rl.space_after("a.test", robots_slot, delay);
        rl.acquire("a.test", Some(delay)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(2));
        rl.acquire("a.test", Some(delay)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(4));
    }

    #[tokio::test(start_paused = true)]
    async fn crawl_delay_on_one_host_does_not_slow_another() {
        let rl = RateLimiter::new(RateLimitConfig::default());
        let robots_slot = rl.acquire("a.test", None).await;
        rl.acquire("b.test", None).await;
        rl.space_after("a.test", robots_slot, Duration::from_secs(60));
        let t = Instant::now();
        rl.acquire("b.test", None).await;
        assert_eq!(t.elapsed(), Duration::from_secs(2));
        rl.acquire("c.test", None).await;
        assert_eq!(t.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_serialises_concurrent_callers() {
        let rl = Arc::new(RateLimiter::new(RateLimitConfig::default()));
        let start = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..3 {
            let rl = rl.clone();
            handles.push(tokio::spawn(async move {
                rl.acquire("a.test", None).await;
                Instant::now()
            }));
        }
        let mut times: Vec<Duration> = Vec::new();
        for h in handles {
            times.push(h.await.unwrap() - start);
        }
        times.sort();
        assert_eq!(
            times,
            vec![
                Duration::ZERO,
                Duration::from_secs(2),
                Duration::from_secs(4)
            ]
        );
    }
}
