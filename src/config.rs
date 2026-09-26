//! Configuration from environment variables. See `.env.example`.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::http::HeaderValue;
use rust_decimal::Decimal;

use crate::enrich::{EnrichConfig, OutputMode};

/// Default GitHub repository for health issues.
pub const DEFAULT_GITHUB_REPO: &str = "alexsiri7/musenmingle";
/// Default minimum interval between two requests to the same domain.
pub const DEFAULT_RATE_LIMIT_MS: u64 = 2_000;
/// Built-in minimum intervals between requests to particular hosts, in ms.
/// These are floors: `RATE_LIMIT_MS` / `RATE_LIMIT_OVERRIDES` can make a
/// host slower but never faster than listed here, so politeness promised to
/// a site doesn't depend on deployment configuration.
pub const BUILTIN_MIN_INTERVALS: &[(&str, u64)] = &[
    // ArtRabbit: an aggregator whose terms restrict reuse; we read only its
    // listing pages, at most one every 5 s (src/sources/artrabbit.rs).
    ("www.artrabbit.com", 5_000),
    ("artrabbit.com", 5_000),
];
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

    /// Interval for a given host: its override (else the default), raised
    /// to the host's [`BUILTIN_MIN_INTERVALS`] floor if lower.
    pub fn interval_for(&self, host: &str) -> Duration {
        let configured = self
            .overrides
            .get(host)
            .copied()
            .unwrap_or(self.default_interval);
        let floor = BUILTIN_MIN_INTERVALS
            .iter()
            .find(|(h, _)| h.eq_ignore_ascii_case(host))
            .map_or(Duration::ZERO, |(_, ms)| Duration::from_millis(*ms));
        configured.max(floor)
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
    /// Requesty API key (`REQUESTY_API_KEY`); unset = no AI enrichment or
    /// embeddings (the ingest only applies default tags).
    pub requesty_api_key: Option<String>,
    /// `REQUESTY_BASE_URL` (tests point it at a mock server).
    pub requesty_base_url: String,
    pub enrich: EnrichConfig,
    /// ntfy topic for owner alerts (`NTFY_TOPIC`); unset = alerts are logged.
    pub ntfy_topic: Option<String>,
    pub ntfy_base_url: String,
}

fn parse_usd(name: &str, default: Decimal) -> Result<Decimal> {
    match non_empty(name) {
        Some(v) => {
            let d: Decimal = v
                .parse()
                .map_err(|_| anyhow::anyhow!("{name} is not a dollar amount: {v:?}"))?;
            if d < Decimal::ZERO {
                bail!("{name} must not be negative");
            }
            Ok(d)
        }
        None => Ok(default),
    }
}

impl EnrichConfig {
    /// `ENRICH_*` and `EMBED_MODEL` (see README).
    pub fn from_env() -> Result<Self> {
        let d = EnrichConfig::default();
        let embed_model = match non_empty("EMBED_MODEL") {
            Some(m) if matches!(m.as_str(), "off" | "none") => None,
            Some(m) => Some(m),
            None => d.embed_model.clone(),
        };
        let reasoning_effort = match non_empty("ENRICH_REASONING_EFFORT") {
            Some(e) if e == "default" => None,
            Some(e) => Some(e),
            None => d.reasoning_effort.clone(),
        };
        let output_mode = match non_empty("ENRICH_OUTPUT_MODE").as_deref() {
            None => d.output_mode,
            Some("json_object") => OutputMode::JsonObject,
            Some("json_schema") => OutputMode::JsonSchema,
            Some(other) => {
                bail!("ENRICH_OUTPUT_MODE must be json_object or json_schema, got {other:?}")
            }
        };
        let c = EnrichConfig {
            model: non_empty("ENRICH_MODEL").unwrap_or(d.model),
            output_mode,
            embed_model,
            daily_cap_usd: parse_usd("ENRICH_DAILY_CAP_USD", d.daily_cap_usd)?,
            run_cap_usd: parse_usd("ENRICH_RUN_CAP_USD", d.run_cap_usd)?,
            batch_size: parse_env("ENRICH_BATCH_SIZE", d.batch_size)?,
            max_events_per_run: parse_env("ENRICH_MAX_EVENTS_PER_RUN", d.max_events_per_run)?,
            embed_batch_size: d.embed_batch_size,
            call_timeout: d.call_timeout,
            run_budget: Duration::from_secs(parse_env(
                "ENRICH_RUN_BUDGET_SECS",
                d.run_budget.as_secs(),
            )?),
            reasoning_effort,
            temperature: d.temperature,
        };
        if !(1..=25).contains(&c.batch_size) {
            bail!("ENRICH_BATCH_SIZE must be between 1 and 25");
        }
        Ok(c)
    }
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
            requesty_api_key: non_empty("REQUESTY_API_KEY"),
            requesty_base_url: non_empty("REQUESTY_BASE_URL")
                .unwrap_or_else(|| crate::enrich::requesty::DEFAULT_BASE_URL.into()),
            enrich: EnrichConfig::from_env()?,
            ntfy_topic: non_empty("NTFY_TOPIC"),
            ntfy_base_url: non_empty("NTFY_BASE_URL")
                .unwrap_or_else(|| crate::notify::DEFAULT_NTFY_BASE.into()),
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
    fn builtin_floors_cannot_be_lowered() {
        let c = RateLimitConfig::parse(Some("1000"), Some("www.artrabbit.com=100")).unwrap();
        assert_eq!(c.interval_for("www.artrabbit.com"), Duration::from_secs(5));
        assert_eq!(c.interval_for("artrabbit.com"), Duration::from_secs(5));
        assert_eq!(
            RateLimitConfig::disabled().interval_for("www.artrabbit.com"),
            Duration::from_secs(5)
        );
        // ... but can be raised.
        let slow = RateLimitConfig::parse(None, Some("www.artrabbit.com=9000")).unwrap();
        assert_eq!(
            slow.interval_for("www.artrabbit.com"),
            Duration::from_secs(9)
        );
        assert_eq!(c.interval_for("example.org"), Duration::from_secs(1));
    }

    #[test]
    fn cors_origins_parse_to_browser_origins() {
        assert!(parse_cors_origins(None).unwrap().is_empty());
        let origins = parse_cors_origins(Some(
            "https://MuseNMingle.example/, http://localhost:5173 ,https://a.example:443",
        ))
        .unwrap();
        assert_eq!(
            origins,
            [
                "https://musenmingle.example",
                "http://localhost:5173",
                "https://a.example"
            ]
        );
    }

    #[test]
    fn cors_origins_reject_non_origins() {
        for bad in [
            "*",
            "musenmingle.example",
            "https://musenmingle.example/app",
            "https://musenmingle.example?x=1",
            "file:///tmp",
            "ftp://musenmingle.example",
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
