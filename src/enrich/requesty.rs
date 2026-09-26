//! Requesty (https://requesty.ai) client: the OpenAI-compatible chat
//! completions and embeddings endpoints. It is an authenticated API client
//! like `crate::github`, not a source: it never fetches web pages, and it
//! only sends the event facts built by `crate::enrich::input`.
//!
//! The API key is sent as a bearer token and never logged; error messages
//! carry at most a short excerpt of the response body.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

/// Production base URL (without `/v1`).
pub const DEFAULT_BASE_URL: &str = "https://router.requesty.ai";

/// Token usage reported by one call.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// All prompt tokens, including cached and cache-write ones.
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// Prompt tokens read from the provider's cache.
    pub cached_tokens: i64,
    /// Prompt tokens written to the provider's cache.
    pub cache_write_tokens: i64,
    /// Requesty's own cost figure, when it sends one (cross-check only).
    pub provider_cost_usd: Option<f64>,
}

/// A chat completion's text and why it stopped.
#[derive(Debug, Clone)]
pub struct Completion {
    pub content: String,
    pub finish_reason: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CallError {
    /// The organisation's balance / the key's spend limit is used up: stop
    /// calling until it is topped up.
    #[error("Requesty credits exhausted ({status}): {message}")]
    CreditsExhausted { status: u16, message: String },
    /// Ordinary rate limiting (try again on a later tick).
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("request failed: {0}")]
    Transport(String),
    #[error("unexpected response: {0}")]
    BadResponse(String),
}

/// Words in an error body that mean money, not request rate.
const CREDIT_WORDS: &[&str] = &[
    "credit",
    "balance",
    "insufficient",
    "payment",
    "quota",
    "spend limit",
    "spending limit",
    "budget",
    "billing",
    "top up",
    "top-up",
];

fn excerpt(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("message"))
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.to_string());
    message.chars().take(300).collect()
}

/// Classify a non-success response. 402 is always credit exhaustion; a 403
/// or 429 is when its message talks about credits, balance, quota or
/// spend limits (a plain 429 is rate limiting).
pub fn classify_error(status: StatusCode, body: &str) -> CallError {
    let message = excerpt(body);
    let lower = message.to_lowercase();
    let about_money = CREDIT_WORDS.iter().any(|w| lower.contains(w));
    let code = status.as_u16();
    match code {
        402 => CallError::CreditsExhausted {
            status: code,
            message,
        },
        403 | 429 if about_money => CallError::CreditsExhausted {
            status: code,
            message,
        },
        429 => CallError::RateLimited(message),
        _ => CallError::Http {
            status: code,
            message,
        },
    }
}

fn int(v: &Value, path: &str) -> i64 {
    v.pointer(path).and_then(Value::as_i64).unwrap_or(0)
}

/// Parse the OpenAI-style `usage` object (with Requesty's cache fields).
pub fn parse_usage(usage: &Value) -> Usage {
    Usage {
        prompt_tokens: int(usage, "/prompt_tokens"),
        completion_tokens: int(usage, "/completion_tokens"),
        cached_tokens: int(usage, "/prompt_tokens_details/cached_tokens"),
        cache_write_tokens: int(usage, "/prompt_tokens_details/caching_tokens"),
        provider_cost_usd: usage.get("cost").and_then(Value::as_f64),
    }
}

pub struct Requesty {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl Requesty {
    pub fn new(base_url: &str, api_key: &str) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("musenmingle/", env!("CARGO_PKG_VERSION")))
                .build()?,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        })
    }

    async fn post(&self, path: &str, body: &Value, timeout: Duration) -> Result<Value, CallError> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.api_key)
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(|e| CallError::Transport(e.without_url().to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| CallError::Transport(e.without_url().to_string()))?;
        if !status.is_success() {
            return Err(classify_error(status, &text));
        }
        serde_json::from_str(&text).map_err(|e| CallError::BadResponse(format!("not JSON: {e}")))
    }

    /// `POST /v1/chat/completions` with `body` (model, messages, ...).
    pub async fn chat(&self, body: &Value, timeout: Duration) -> Result<Completion, CallError> {
        let v = self.post("/v1/chat/completions", body, timeout).await?;
        // Some gateways report errors inside a 200.
        if let Some(err) = v.get("error") {
            return Err(classify_error(
                StatusCode::BAD_GATEWAY,
                &json!({ "error": err }).to_string(),
            ));
        }
        let choice = v
            .pointer("/choices/0")
            .ok_or_else(|| CallError::BadResponse("no choices".into()))?;
        Ok(Completion {
            content: choice
                .pointer("/message/content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            finish_reason: choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            usage: parse_usage(v.get("usage").unwrap_or(&Value::Null)),
        })
    }

    /// `POST /v1/embeddings`: one vector per input, in input order.
    pub async fn embed(
        &self,
        model: &str,
        inputs: &[String],
        timeout: Duration,
    ) -> Result<(Vec<Vec<f32>>, Usage), CallError> {
        let v = self
            .post(
                "/v1/embeddings",
                &json!({ "model": model, "input": inputs }),
                timeout,
            )
            .await?;
        let data = v
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| CallError::BadResponse("no data".into()))?;
        let mut out: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
        for (pos, d) in data.iter().enumerate() {
            let i = d
                .get("index")
                .and_then(Value::as_u64)
                .map_or(pos, |i| i as usize);
            let vec: Vec<f32> = d
                .get("embedding")
                .and_then(Value::as_array)
                .ok_or_else(|| CallError::BadResponse("embedding missing".into()))?
                .iter()
                .map(|x| x.as_f64().map(|f| f as f32))
                .collect::<Option<_>>()
                .ok_or_else(|| CallError::BadResponse("embedding is not numeric".into()))?;
            let slot = out
                .get_mut(i)
                .ok_or_else(|| CallError::BadResponse(format!("index {i} out of range")))?;
            *slot = Some(vec);
        }
        let vectors = out
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| CallError::BadResponse("an input got no embedding".into()))?;
        Ok((vectors, parse_usage(v.get("usage").unwrap_or(&Value::Null))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_credit_exhaustion_apart_from_rate_limits() {
        let e = classify_error(
            StatusCode::PAYMENT_REQUIRED,
            r#"{"error":{"message":"organization balance exhausted"}}"#,
        );
        assert!(matches!(e, CallError::CreditsExhausted { status: 402, .. }));
        let e = classify_error(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"Insufficient credits, please top up"}}"#,
        );
        assert!(matches!(e, CallError::CreditsExhausted { status: 429, .. }));
        let e = classify_error(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"Rate limit exceeded, retry after 2s"}}"#,
        );
        assert!(matches!(e, CallError::RateLimited(_)));
        let e = classify_error(
            StatusCode::FORBIDDEN,
            r#"{"error":{"message":"invalid token"}}"#,
        );
        assert!(matches!(e, CallError::Http { status: 403, .. }));
        let e = classify_error(StatusCode::FORBIDDEN, "monthly spend limit reached");
        assert!(matches!(e, CallError::CreditsExhausted { .. }));
    }

    #[test]
    fn usage_reads_requesty_cache_fields() {
        let u = parse_usage(&json!({
            "completion_tokens": 216, "prompt_tokens": 3706,
            "prompt_tokens_details": { "cached_tokens": 3680, "caching_tokens": 0 },
            "cost": 0.00516
        }));
        assert_eq!(u.prompt_tokens, 3706);
        assert_eq!(u.cached_tokens, 3680);
        assert_eq!(u.cache_write_tokens, 0);
        assert_eq!(u.provider_cost_usd, Some(0.00516));
    }
}
