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
//! * a per-domain rate limit (default one request every 2 s per host).
//!
//! The underlying `reqwest::Client` is private on purpose; do not add an
//! accessor for it.

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
}

/// A body fetched by [`FetchContext::get_bytes_limited`].
#[derive(Debug, Clone)]
pub struct FetchedBytes {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

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
}

impl FetchContext {
    pub fn new(rate_limit: RateLimitConfig) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .user_agent(user_agent())
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            limiter: RateLimiter::new(rate_limit),
            robots: Mutex::new(HashMap::new()),
            soft_errors: StdMutex::new(Vec::new()),
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

    /// GET `url` after robots.txt and rate-limit checks. Non-2xx statuses
    /// are errors.
    pub async fn get(&self, url: &Url) -> Result<reqwest::Response, FetchError> {
        let robots = self.robots_for(url).await?;
        if !robots.allowed(url) {
            return Err(FetchError::RobotsDisallowed(redact(url)));
        }
        let (_, host) = Self::origin_and_host(url)?;
        self.limiter.acquire(&host, robots.crawl_delay()).await;
        tracing::debug!(url = %redact(url), "GET");
        let resp = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| FetchError::Http {
                url: redact(url),
                source: e.without_url(),
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

    /// GET and return the body as text.
    pub async fn get_text(&self, url: &Url) -> Result<String, FetchError> {
        self.get(url)
            .await?
            .text()
            .await
            .map_err(|e| FetchError::Http {
                url: redact(url),
                source: e.without_url(),
            })
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
        let bytes = self
            .get(url)
            .await?
            .bytes()
            .await
            .map_err(|e| FetchError::Http {
                url: redact(url),
                source: e.without_url(),
            })?;
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
