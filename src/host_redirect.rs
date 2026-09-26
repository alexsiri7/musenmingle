//! Permanent redirect from the project's former host name to the canonical one.
//!
//! The site used to live at `thaleia.interstellarai.net`; it is now
//! `musenmingle.interstellarai.net`. Both domains point at the same service,
//! and a request whose `Host` is one of the legacy hosts is redirected to the
//! same path and query on the canonical host.
//!
//! Only listed legacy hosts redirect (never "anything that isn't canonical"),
//! so Railway's `*.up.railway.app` domain, its health checks and local
//! development are unaffected. `/healthz` never redirects. Safe methods get a
//! 301; anything else gets a 308 so a POST stays a POST. With no canonical
//! host configured (`CANONICAL_HOST` unset) the layer does nothing.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Env var naming the canonical host (e.g. `musenmingle.interstellarai.net`).
pub const CANONICAL_HOST_ENV: &str = "CANONICAL_HOST";
/// Env var listing legacy hosts to redirect, comma-separated.
pub const LEGACY_HOSTS_ENV: &str = "LEGACY_HOSTS";
/// Legacy hosts when `LEGACY_HOSTS` is unset.
pub const DEFAULT_LEGACY_HOSTS: &str = "thaleia.interstellarai.net";

/// Which hosts redirect where. Build with [`HostRedirect::new`].
#[derive(Debug, Clone, Default)]
pub struct HostRedirect {
    canonical: Option<String>,
    legacy: Vec<String>,
}

/// Lower-cases a host and drops any `:port` and trailing dot.
fn normalise_host(h: &str) -> String {
    let h = h.trim().to_ascii_lowercase();
    let h = h.split(':').next().unwrap_or("");
    h.trim_end_matches('.').to_string()
}

impl HostRedirect {
    /// `canonical`: the host to send legacy traffic to (None = inert).
    /// `legacy`: comma-separated host names that redirect.
    pub fn new(canonical: Option<&str>, legacy: &str) -> Self {
        let canonical = canonical.map(normalise_host).filter(|h| !h.is_empty());
        let legacy = legacy
            .split(',')
            .map(normalise_host)
            .filter(|h| !h.is_empty() && Some(h) != canonical.as_ref())
            .collect();
        Self { canonical, legacy }
    }

    /// From `CANONICAL_HOST` and `LEGACY_HOSTS` (default [`DEFAULT_LEGACY_HOSTS`]).
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let legacy = get(LEGACY_HOSTS_ENV).unwrap_or_else(|| DEFAULT_LEGACY_HOSTS.to_string());
        Self::new(get(CANONICAL_HOST_ENV).as_deref(), &legacy)
    }

    /// Whether any request can be redirected at all.
    pub fn is_active(&self) -> bool {
        self.canonical.is_some() && !self.legacy.is_empty()
    }

    /// The redirect for `req`, if it should be redirected.
    fn redirect_for(&self, req: &Request) -> Option<Response> {
        let canonical = self.canonical.as_ref()?;
        if req.uri().path() == "/healthz" {
            return None;
        }
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .or_else(|| req.uri().host())?;
        if !self.legacy.contains(&normalise_host(host)) {
            return None;
        }
        let path_and_query = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let location =
            HeaderValue::try_from(format!("https://{canonical}{path_and_query}")).ok()?;
        let status = if req.method() == Method::GET || req.method() == Method::HEAD {
            StatusCode::MOVED_PERMANENTLY
        } else {
            StatusCode::PERMANENT_REDIRECT
        };
        Some((status, [(header::LOCATION, location)]).into_response())
    }
}

/// Middleware (use with `axum::middleware::from_fn_with_state`).
pub async fn redirect_legacy_host(
    State(cfg): State<HostRedirect>,
    req: Request,
    next: Next,
) -> Response {
    match cfg.redirect_for(&req) {
        Some(resp) => resp,
        None => next.run(req).await,
    }
}

/// Wraps `router` with the redirect (a no-op when `cfg` is inactive).
pub fn apply(router: axum::Router, cfg: HostRedirect) -> axum::Router {
    if !cfg.is_active() {
        return router;
    }
    router.layer(axum::middleware::from_fn_with_state(
        cfg,
        redirect_legacy_host,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalises_hosts() {
        let r = HostRedirect::new(
            Some("MuseNMingle.interstellarai.net."),
            " Thaleia.Interstellarai.net:443 ,,",
        );
        assert_eq!(
            r.canonical.as_deref(),
            Some("musenmingle.interstellarai.net")
        );
        assert_eq!(r.legacy, vec!["thaleia.interstellarai.net".to_string()]);
        assert!(r.is_active());
        assert!(!HostRedirect::new(None, DEFAULT_LEGACY_HOSTS).is_active());
        assert!(!HostRedirect::new(Some(""), DEFAULT_LEGACY_HOSTS).is_active());
        // A legacy host equal to the canonical one would loop: dropped.
        assert!(!HostRedirect::new(Some("a.example"), "a.example").is_active());
    }
}
