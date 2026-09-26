//! The public contact form for venues and site owners (`GET/POST /contact`,
//! rendered by `crate::web`).
//!
//! A request is validated, stored in `events.contact_requests` and turned
//! into a `venue-request` GitHub issue through the same server-side
//! [`IssueFiler`] as site suggestions, so venues need no GitHub account and
//! never see the (private) repository. The issue references only the row id:
//! the optional reply email stays in the database and is never sent to
//! GitHub. All visitor text in the issue is inside a code fence with
//! backticks removed, so it cannot inject Markdown or HTML.
//!
//! A second request for the same registrable domain and type within
//! [`DEDUPE_WINDOW`] is added as a comment on the first one's issue. When
//! GitHub is not configured or fails, the row stays `pending_issue` and the
//! next ingest run files it ([`file_pending`]); the visitor sees the same
//! "Thanks" either way.
//!
//! Spam protection, without third-party scripts or CAPTCHAs: a honeypot
//! field, a signed form timestamp that must be at least [`MIN_FILL_SECS`]
//! old, a per-IP rate limit on the salted IP hash and a request body limit
//! (in `web`). Honeypot and too-fast submissions are answered with the
//! normal "Thanks" page (200) but nothing is stored or filed: a bot learns
//! nothing, and a human who was somehow too fast can simply send again.

use std::net::IpAddr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};

use crate::github::IssueFiler;
use crate::suggestions::{self, Invalid, Suggestions};

pub const LABEL: &str = "venue-request";
pub const MAX_DETAILS_CHARS: usize = 2000;
pub const MAX_EMAIL_CHARS: usize = 254;
/// Forms submitted faster than this after rendering are dropped.
pub const MIN_FILL_SECS: i64 = 3;
/// Form tokens older than this are rejected (reload the page).
pub const MAX_FORM_AGE_SECS: i64 = 24 * 3600;
pub const PER_HOUR: i64 = 3;
pub const PER_DAY: i64 = 10;
/// Same domain + type within this window → comment on the earlier issue.
pub const DEDUPE_WINDOW: chrono::Duration = chrono::Duration::days(7);
/// Pending rows younger than this are left to the request that made them.
pub const RETRY_GRACE: chrono::Duration = chrono::Duration::minutes(10);
const GITHUB_TIMEOUT: Duration = Duration::from_secs(10);

/// What the visitor asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestType {
    RemoveListings,
    CorrectEvent,
    Other,
}

impl RequestType {
    pub const ALL: [RequestType; 3] = [
        RequestType::RemoveListings,
        RequestType::CorrectEvent,
        RequestType::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RequestType::RemoveListings => "remove_listings",
            RequestType::CorrectEvent => "correct_event",
            RequestType::Other => "other",
        }
    }

    /// Form label.
    pub fn label(self) -> &'static str {
        match self {
            RequestType::RemoveListings => "Remove my venue's listings",
            RequestType::CorrectEvent => "Correct an event",
            RequestType::Other => "Something else",
        }
    }

    /// Short form for issue titles.
    pub fn short(self) -> &'static str {
        match self {
            RequestType::RemoveListings => "remove listings",
            RequestType::CorrectEvent => "correction",
            RequestType::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// The form as submitted (all fields optional at this point).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ContactForm {
    #[serde(default)]
    pub request_type: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub reply_email: String,
    /// Honeypot: hidden from people, filled by naive bots.
    #[serde(default)]
    pub website: String,
    /// Signed render time ([`form_token`]).
    #[serde(default)]
    pub token: String,
}

/// A validated request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub request_type: RequestType,
    pub url: String,
    pub domain: String,
    pub details: String,
    pub reply_email: Option<String>,
}

/// Field problems, shown next to the form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContactInvalid {
    #[error("choose what you'd like us to do")]
    Type,
    #[error("enter your venue's website address")]
    UrlMissing,
    #[error("the website address doesn't look right: {0}")]
    Url(Invalid),
    #[error("tell us a little about the request")]
    DetailsMissing,
    #[error("details must be at most {MAX_DETAILS_CHARS} characters")]
    DetailsTooLong,
    #[error("the reply email doesn't look like an email address")]
    Email,
}

pub fn validate(form: &ContactForm) -> Result<Request, ContactInvalid> {
    let request_type = RequestType::parse(form.request_type.trim()).ok_or(ContactInvalid::Type)?;
    let raw_url = form.url.trim();
    if raw_url.is_empty() {
        return Err(ContactInvalid::UrlMissing);
    }
    let with_scheme = if raw_url.contains("://") {
        raw_url.to_string()
    } else {
        format!("https://{raw_url}")
    };
    let sub = suggestions::validate(&with_scheme, None).map_err(ContactInvalid::Url)?;
    let details = form.details.replace("\r\n", "\n").trim().to_string();
    if details.is_empty() {
        return Err(ContactInvalid::DetailsMissing);
    }
    if details.chars().count() > MAX_DETAILS_CHARS {
        return Err(ContactInvalid::DetailsTooLong);
    }
    let email = form.reply_email.trim();
    let reply_email = if email.is_empty() {
        None
    } else {
        let ok = email.chars().count() <= MAX_EMAIL_CHARS
            && !email.chars().any(char::is_whitespace)
            && email
                .split_once('@')
                .is_some_and(|(local, host)| !local.is_empty() && host.contains('.'));
        if !ok {
            return Err(ContactInvalid::Email);
        }
        Some(email.to_string())
    };
    Ok(Request {
        request_type,
        url: sub.url,
        domain: sub.domain,
        details,
        reply_email,
    })
}

fn sign(secret: &str, ts: i64) -> String {
    let digest = Sha256::digest(format!("contact-form|{secret}|{ts}|{secret}"));
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Hidden-field token recording when the form was rendered.
pub fn form_token(secret: &str, now: DateTime<Utc>) -> String {
    let ts = now.timestamp();
    format!("{ts}.{}", sign(secret, ts))
}

/// Whether `token` is ours and was issued between [`MIN_FILL_SECS`] and
/// [`MAX_FORM_AGE_SECS`] before `now`.
pub fn token_ok(secret: &str, token: &str, now: DateTime<Utc>) -> bool {
    let Some((ts, sig)) = token.split_once('.') else {
        return false;
    };
    let Ok(ts) = ts.parse::<i64>() else {
        return false;
    };
    let age = now.timestamp() - ts;
    sig == sign(secret, ts) && (MIN_FILL_SECS..=MAX_FORM_AGE_SECS).contains(&age)
}

/// The answer to one submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Stored (and filed, or queued for the next ingest run).
    Received {
        id: i64,
    },
    /// Honeypot or too fast: answered like `Received`, nothing stored.
    Dropped,
    RateLimited,
    Invalid(ContactInvalid),
}

/// Issue title: "Venue request: example.org (remove listings)".
pub fn issue_title(domain: &str, t: RequestType) -> String {
    format!("Venue request: {domain} ({})", t.short())
}

/// Visitor text for an issue: backticks removed (so it cannot close the
/// fence), inside a code fence (so Markdown/HTML/@mentions stay inert).
fn quoted(text: &str) -> String {
    suggestions::fenced(&text.replace('`', "'"))
}

/// Issue (or comment) body. Never includes the reply email.
pub fn issue_body(
    id: i64,
    t: RequestType,
    url: &str,
    domain: &str,
    details: &str,
    has_email: bool,
) -> String {
    format!(
        "Sent with the contact form on the site: **contact request #{id}** \
         (`events.contact_requests`).\n\n\
         - **Type:** {}\n\
         - **Domain:** {domain}\n\
         - **Reply email:** {}\n\n\
         **Website (as given):**\n\n{}\n\n\
         **Details (unverified, as given):**\n\n{}\n\n\
         Procedure: `docs/venue-requests.md` (removals within 7 days).",
        t.label(),
        if has_email {
            "given; see the row in `events.contact_requests` (never posted here)"
        } else {
            "none"
        },
        quoted(url),
        quoted(details),
    )
}

#[derive(Debug, Clone, FromRow)]
struct Row {
    id: i64,
    request_type: String,
    url: String,
    domain: String,
    details: String,
    has_email: bool,
    created_at: DateTime<Utc>,
}

/// Validate, spam-check, rate-limit, store and deliver one submission.
pub async fn submit(
    pool: &PgPool,
    suggestions: &Suggestions,
    client: IpAddr,
    form: &ContactForm,
    now: DateTime<Utc>,
) -> anyhow::Result<Outcome> {
    if !form.website.trim().is_empty() || !token_ok(suggestions.ip_salt(), &form.token, now) {
        tracing::info!("contact form dropped (honeypot or too fast)");
        return Ok(Outcome::Dropped);
    }
    let req = match validate(form) {
        Ok(r) => r,
        Err(e) => return Ok(Outcome::Invalid(e)),
    };
    let ip_hash = suggestions::ip_hash(client, suggestions.ip_salt());
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 1))")
        .bind(&ip_hash)
        .execute(&mut *tx)
        .await?;
    let (last_hour, last_day): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE created_at > $2 - interval '1 hour'),
                count(*)
         FROM events.contact_requests
         WHERE ip_hash = $1 AND created_at > $2 - interval '1 day'",
    )
    .bind(&ip_hash)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    if last_hour >= PER_HOUR || last_day >= PER_DAY {
        return Ok(Outcome::RateLimited);
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO events.contact_requests
            (request_type, url, domain, details, reply_email, ip_hash, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(req.request_type.as_str())
    .bind(&req.url)
    .bind(&req.domain)
    .bind(&req.details)
    .bind(&req.reply_email)
    .bind(&ip_hash)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    if let Some(filer) = suggestions.filer() {
        match tokio::time::timeout(GITHUB_TIMEOUT, deliver(pool, filer, id)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(id, error = %e, "filing contact request failed; left pending")
            }
            Err(_) => tracing::warn!(id, "filing contact request timed out; left pending"),
        }
    } else {
        tracing::warn!(id, "no GitHub filer; contact request left pending");
    }
    Ok(Outcome::Received { id })
}

/// File one pending request: as a comment on the issue of an earlier
/// request for the same domain and type within [`DEDUPE_WINDOW`], else as a
/// new issue.
async fn deliver(pool: &PgPool, filer: &dyn IssueFiler, id: i64) -> anyhow::Result<()> {
    let Some(row): Option<Row> = sqlx::query_as(
        "SELECT id, request_type, url, domain, details, reply_email IS NOT NULL AS has_email,
                created_at
         FROM events.contact_requests WHERE id = $1 AND status = 'pending_issue'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(());
    };
    let t = RequestType::parse(&row.request_type).unwrap_or(RequestType::Other);
    let body = issue_body(
        row.id,
        t,
        &row.url,
        &row.domain,
        &row.details,
        row.has_email,
    );
    let earlier: Option<i64> = sqlx::query_scalar(
        "SELECT github_issue_number FROM events.contact_requests
         WHERE domain = $1 AND request_type = $2 AND id <> $3
           AND github_issue_number IS NOT NULL AND created_at > $4
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&row.domain)
    .bind(&row.request_type)
    .bind(row.id)
    .bind(row.created_at - DEDUPE_WINDOW)
    .fetch_optional(pool)
    .await?;
    let (number, status) = match earlier {
        Some(n) => {
            filer
                .comment(n, &format!("Another request for the same site:\n\n{body}"))
                .await?;
            (n, "commented")
        }
        None => {
            let n = filer
                .create_issue(&issue_title(&row.domain, t), &body, &[LABEL])
                .await?;
            (n, "filed")
        }
    };
    sqlx::query(
        "UPDATE events.contact_requests SET github_issue_number = $2, status = $3 WHERE id = $1",
    )
    .bind(row.id)
    .bind(number)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(())
}

/// File requests left `pending_issue` (older than [`RETRY_GRACE`]), oldest
/// first. Returns how many were delivered.
pub async fn file_pending(
    pool: &PgPool,
    filer: &dyn IssueFiler,
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM events.contact_requests
         WHERE status = 'pending_issue' AND created_at < $1 ORDER BY id",
    )
    .bind(now - RETRY_GRACE)
    .fetch_all(pool)
    .await?;
    for id in &ids {
        deliver(pool, filer, *id).await?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(t: &str, url: &str, details: &str, email: &str) -> ContactForm {
        ContactForm {
            request_type: t.into(),
            url: url.into(),
            details: details.into(),
            reply_email: email.into(),
            ..Default::default()
        }
    }

    #[test]
    fn validation() {
        let ok = validate(&form(
            "remove_listings",
            "www.example.org/whats-on",
            " Please remove us. ",
            "",
        ))
        .unwrap();
        assert_eq!(ok.domain, "example.org");
        assert_eq!(ok.url, "https://www.example.org/whats-on");
        assert_eq!(ok.details, "Please remove us.");
        assert_eq!(ok.reply_email, None);
        assert_eq!(
            validate(&form("other", "https://example.org", "x", "a@b.org"))
                .unwrap()
                .reply_email
                .as_deref(),
            Some("a@b.org")
        );
        for (f, err) in [
            (
                form("delete_all", "example.org", "x", ""),
                ContactInvalid::Type,
            ),
            (form("other", " ", "x", ""), ContactInvalid::UrlMissing),
            (
                form("other", "example.org", "  ", ""),
                ContactInvalid::DetailsMissing,
            ),
            (
                form(
                    "other",
                    "example.org",
                    &"x".repeat(MAX_DETAILS_CHARS + 1),
                    "",
                ),
                ContactInvalid::DetailsTooLong,
            ),
            (
                form("other", "example.org", "x", "not an email"),
                ContactInvalid::Email,
            ),
            (
                form("other", "example.org", "x", "@example.org"),
                ContactInvalid::Email,
            ),
        ] {
            assert_eq!(validate(&f), Err(err));
        }
        assert!(matches!(
            validate(&form("other", "http://localhost/", "x", "")),
            Err(ContactInvalid::Url(_))
        ));
    }

    #[test]
    fn tokens_expire_and_must_be_old_enough() {
        let t0: DateTime<Utc> = "2026-09-26T12:00:00Z".parse().unwrap();
        let tok = form_token("salt", t0);
        assert!(!token_ok("salt", &tok, t0 + chrono::Duration::seconds(2)));
        assert!(token_ok("salt", &tok, t0 + chrono::Duration::seconds(3)));
        assert!(!token_ok(
            "other-salt",
            &tok,
            t0 + chrono::Duration::seconds(10)
        ));
        assert!(!token_ok("salt", &tok, t0 + chrono::Duration::days(2)));
        let forged = format!("{}.{}", t0.timestamp() - 60, tok.split_once('.').unwrap().1);
        assert!(!token_ok("salt", &forged, t0));
        assert!(!token_ok("salt", "", t0));
    }

    #[test]
    fn issue_body_is_inert_and_has_no_email() {
        let evil = "<img src=x onerror=alert(1)> **bold** @alexsiri7\n```\n# heading\n[x](javascript:alert(1))";
        let body = issue_body(
            42,
            RequestType::Other,
            "https://example.org/`a`",
            "example.org",
            evil,
            true,
        );
        // Visitor text only appears inside one fence each, with no backticks.
        let fence_lines: Vec<&str> = body.lines().filter(|l| l.starts_with("```")).collect();
        assert_eq!(fence_lines, ["```text", "```", "```text", "```"], "{body}");
        let details_start = body.find("<img").unwrap();
        let open = body[..details_start].rfind("```text\n").unwrap();
        let close = body[details_start..].find("\n```").unwrap() + details_start;
        assert!(open < details_start && close > details_start);
        assert!(body.contains("'''\n# heading"), "{body}");
        assert!(body.contains("contact request #42"));
        assert_eq!(
            issue_title("example.org", RequestType::RemoveListings),
            "Venue request: example.org (remove listings)"
        );
    }
}
