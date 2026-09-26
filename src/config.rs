//! Configuration from environment variables. See `.env.example`.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::http::HeaderValue;

/// Default GitHub repository for health issues.
pub const DEFAULT_GITHUB_REPO: &str = "alexsiri7/thaleia";
/// Default minimum interval between two requests to the same domain.
pub const DEFAULT_RATE_LIMIT_MS: u64 = 2_000;
/// Environment variable holding the Ticketmaster Discovery API key.
pub const TICKETMASTER_API_KEY_ENV: &str = "TICKETMASTER_API_KEY";

/// Per-domain rate limit configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// Minimum interval between requests to one domain.
    pub default_interval: Duration,
    /// Domain-specific overrides (exact host match).
    pub overrides: HashMap<String, Duration>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            default_interval: Duration::from_millis(DEFAULT_RATE_LIMIT_MS),
            overrides: HashMap::new(),
        }
    }
}

impl RateLimitConfig {
    /// No rate limiting at all (tests only).
    pub fn disabled() -> Self {
        Self {
            default_interval: Duration::ZERO,
            overrides: HashMap::new(),
        }
    }

    /// Interval for a given host.
    pub fn interval_for(&self, host: &str) -> Duration {
        self.overrides
            .get(host)
            .copied()
            .unwrap_or(self.default_interval)
    }

    /// Parse `RATE_LIMIT_MS` and `RATE_LIMIT_OVERRIDES`
    /// (`host=ms,host=ms`).
    pub fn parse(default_ms: Option<&str>, overrides: Option<&str>) -> Result<Self> {
        let default_interval = match default_ms.map(str::trim).filter(|s| !s.is_empty()) {
            Some(v) => Duration::from_millis(
                v.parse()
                    .with_context(|| format!("RATE_LIMIT_MS is not an integer: {v:?}"))?,
            ),
            None => Duration::from_millis(DEFAULT_RATE_LIMIT_MS),
        };
        let mut map = HashMap::new();
        for pair in overrides
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let Some((host, ms)) = pair.split_once('=') else {
                bail!("RATE_LIMIT_OVERRIDES entry must be host=ms, got {pair:?}");
            };
            let ms: u64 = ms
                .trim()
                .parse()
                .with_context(|| format!("RATE_LIMIT_OVERRIDES: bad ms in {pair:?}"))?;
            map.insert(host.trim().to_ascii_lowercase(), Duration::from_millis(ms));
        }
        Ok(Self {
            default_interval,
            overrides: map,
        })
    }
}

/// Public site submissions (`POST /v1/suggestions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionConfig {
    /// Salt for the stored client-IP hash; the API refuses to start without it.
    pub ip_salt: Option<String>,
    pub per_hour: u32,
    pub per_day: u32,
    /// Reverse proxies in front of the API whose `X-Forwarded-For` entries
    /// are trusted (Railway: 1). 0 = use the TCP peer address.
    pub trusted_proxies: usize,
}

impl Default for SuggestionConfig {
    fn default() -> Self {
        Self {
            ip_salt: None,
            per_hour: 5,
            per_day: 20,
            trusted_proxies: 0,
        }
    }
}

fn parse_env<T: std::str::FromStr>(name: &str, default: T) -> Result<T> {
    match non_empty(name) {
        Some(v) => v
            .parse()
            .map_err(|_| anyhow::anyhow!("{name} is not a valid number: {v:?}")),
        None => Ok(default),
    }
}

impl SuggestionConfig {
    fn from_env() -> Result<Self> {
        let d = Self::default();
        let c = Self {
            ip_salt: non_empty("SUGGESTION_IP_SALT"),
            per_hour: parse_env("SUGGESTION_RATE_PER_HOUR", d.per_hour)?,
            per_day: parse_env("SUGGESTION_RATE_PER_DAY", d.per_day)?,
            trusted_proxies: parse_env("TRUSTED_PROXY_COUNT", d.trusted_proxies)?,
        };
        if c.per_hour == 0 || c.per_day == 0 {
            bail!("SUGGESTION_RATE_PER_HOUR and SUGGESTION_RATE_PER_DAY must be at least 1");
        }
        Ok(c)
    }
}

/// Parse `CORS_ORIGINS` (comma-separated `scheme://host[:port]`) into the
/// exact `Origin` header values browsers send.
pub fn parse_cors_origins(value: Option<&str>) -> Result<Vec<HeaderValue>> {
    let mut origins = Vec::new();
    for entry in value
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let url = url::Url::parse(entry)
            .with_context(|| format!("CORS_ORIGINS: not an origin: {entry:?}"))?;
        let origin = url.origin();
        if !matches!(url.scheme(), "http" | "https")
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !origin.is_tuple()
        {
            bail!("CORS_ORIGINS: expected scheme://host[:port], got {entry:?}");
        }
        origins.push(HeaderValue::from_str(&origin.ascii_serialization())?);
    }
    Ok(origins)
}

/// Process configuration shared by both binaries.
#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub ticketmaster_api_key: Option<String>,
    pub github_token: Option<String>,
    pub github_repo: String,
    pub port: u16,
    pub rate_limit: RateLimitConfig,
    /// Per-source fetch timeout for the ingest runner.
    pub source_timeout: Duration,
    pub suggestions: SuggestionConfig,
    /// Browser origins allowed to call the API; empty = none.
    pub cors_origins: Vec<HeaderValue>,
}

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl Config {
    /// Load from the process environment.
    pub fn from_env() -> Result<Self> {
        let database_url = non_empty("DATABASE_URL").context("DATABASE_URL must be set")?;
        let port = match non_empty("PORT") {
            Some(p) => p
                .parse()
                .with_context(|| format!("PORT is not a u16: {p:?}"))?,
            None => 8080,
        };
        let source_timeout = match non_empty("SOURCE_TIMEOUT_SECS") {
            Some(s) => Duration::from_secs(
                s.parse()
                    .with_context(|| format!("SOURCE_TIMEOUT_SECS is not an integer: {s:?}"))?,
            ),
            None => Duration::from_secs(300),
        };
        Ok(Self {
            database_url,
            ticketmaster_api_key: non_empty(TICKETMASTER_API_KEY_ENV),
            github_token: non_empty("GITHUB_TOKEN"),
            github_repo: non_empty("GITHUB_REPO").unwrap_or_else(|| DEFAULT_GITHUB_REPO.into()),
            port,
            rate_limit: RateLimitConfig::parse(
                non_empty("RATE_LIMIT_MS").as_deref(),
                non_empty("RATE_LIMIT_OVERRIDES").as_deref(),
            )?,
            source_timeout,
            suggestions: SuggestionConfig::from_env()?,
            cors_origins: parse_cors_origins(non_empty("CORS_ORIGINS").as_deref())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_defaults_to_two_seconds() {
        let c = RateLimitConfig::parse(None, None).unwrap();
        assert_eq!(c.interval_for("example.com"), Duration::from_secs(2));
    }

    #[test]
    fn rate_limit_overrides_parse() {
        let c = RateLimitConfig::parse(Some("1500"), Some("app.ticketmaster.com=500, Foo.org=10"))
            .unwrap();
        assert_eq!(c.interval_for("example.com"), Duration::from_millis(1500));
        assert_eq!(
            c.interval_for("app.ticketmaster.com"),
            Duration::from_millis(500)
        );
        assert_eq!(c.interval_for("foo.org"), Duration::from_millis(10));
    }

    #[test]
    fn cors_origins_parse_to_browser_origins() {
        assert!(parse_cors_origins(None).unwrap().is_empty());
        let origins = parse_cors_origins(Some(
            "https://Thaleia.example/, http://localhost:5173 ,https://a.example:443",
        ))
        .unwrap();
        assert_eq!(
            origins,
            [
                "https://thaleia.example",
                "http://localhost:5173",
                "https://a.example"
            ]
        );
    }

    #[test]
    fn cors_origins_reject_non_origins() {
        for bad in [
            "*",
            "thaleia.example",
            "https://thaleia.example/app",
            "https://thaleia.example?x=1",
            "file:///tmp",
            "ftp://thaleia.example",
        ] {
            assert!(parse_cors_origins(Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn rate_limit_rejects_garbage() {
        assert!(RateLimitConfig::parse(Some("abc"), None).is_err());
        assert!(RateLimitConfig::parse(None, Some("nohost")).is_err());
    }
}
