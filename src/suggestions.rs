//! Public site submissions: `POST /v1/suggestions` (see [`crate::api`]).
//!
//! A submitted URL is validated and reduced to its registrable domain
//! (`https://www.example.org/events` → `example.org`), which is what dedupe
//! works on: against `events.sources.domain` (normalised the same way) and
//! against pending/accepted suggestions (a unique partial index). Every
//! stored row, duplicates included, counts toward the submitter's rate limit,
//! keyed by a salted hash of the client IP. An accepted suggestion becomes
//! one `new-scraper` GitHub issue; if GitHub is unreachable the row stays
//! `pending` and the next ingest run files it ([`file_pending`]).
//!
//! The submitted URL is never fetched.

use std::net::IpAddr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::config::SuggestionConfig;
use crate::github::IssueFiler;
use crate::repo::{self, NewSuggestion, RefusedSourceRow};

pub const LABEL: &str = "new-scraper";
pub const MAX_URL_LEN: usize = 2048;
pub const MAX_NOTE_CHARS: usize = 500;

/// Pending suggestions younger than this are left to the API request that
/// created them (which may still be talking to GitHub).
pub const RETRY_GRACE: chrono::Duration = chrono::Duration::minutes(10);

/// Upper bound on the GitHub call inside a request; on expiry the ingest
/// run files the issue instead.
const GITHUB_TIMEOUT: Duration = Duration::from_secs(10);

const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(24 * 3600);

const TEMPLATE: &str = include_str!("../.github/ISSUE_TEMPLATE/new-scraper.md");

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("url must be at most {MAX_URL_LEN} characters")]
    UrlTooLong,
    #[error("url is not a valid absolute URL")]
    Unparseable,
    #[error("only http and https URLs are accepted")]
    Scheme,
    #[error("URLs containing credentials are not accepted")]
    Credentials,
    #[error("the host must be a public domain name")]
    NotPublic,
    #[error("note must be at most {MAX_NOTE_CHARS} characters")]
    NoteTooLong,
}

/// A validated submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub url: String,
    pub domain: String,
    pub note: Option<String>,
}

/// Registrable domain of `host` under the public suffix list, lowercased,
/// or `None` for hosts that are not under a known public suffix
/// (`localhost`, `printer.local`, single labels, bare suffixes).
pub fn registrable_domain(host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let domain = psl::domain(host.as_bytes())?;
    if !domain.suffix().is_known() {
        return None;
    }
    std::str::from_utf8(domain.as_bytes())
        .ok()
        .map(str::to_owned)
}

pub fn validate(url: &str, note: Option<&str>) -> Result<Submission, Invalid> {
    let url = url.trim();
    if url.chars().count() > MAX_URL_LEN {
        return Err(Invalid::UrlTooLong);
    }
    let parsed = url::Url::parse(url).map_err(|_| Invalid::Unparseable)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Invalid::Scheme);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Invalid::Credentials);
    }
    let domain = match parsed.host() {
        Some(url::Host::Domain(host)) => registrable_domain(host).ok_or(Invalid::NotPublic)?,
        _ => return Err(Invalid::NotPublic),
    };
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS) {
        return Err(Invalid::NoteTooLong);
    }
    Ok(Submission {
        url: parsed.to_string(),
        domain,
        note: note.map(str::to_owned),
    })
}

/// The address to rate-limit. With `trusted_proxies` = n > 0, the client is
/// the n-th `X-Forwarded-For` entry from the right (the one the outermost
/// trusted proxy appended); entries further left are client-controlled. If
/// that entry is missing or not an IP, the peer address is used.
pub fn client_ip<'a>(
    peer: IpAddr,
    forwarded_for: impl IntoIterator<Item = &'a str>,
    trusted_proxies: usize,
) -> IpAddr {
    if trusted_proxies == 0 {
        return peer;
    }
    let hops: Vec<&str> = forwarded_for
        .into_iter()
        .flat_map(|h| h.split(','))
        .map(str::trim)
        .collect();
    hops.len()
        .checked_sub(trusted_proxies)
        .and_then(|i| hops[i].parse().ok())
        .unwrap_or(peer)
}

pub fn ip_hash(ip: IpAddr, salt: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{ip}{salt}")))
}

pub fn issue_title(domain: &str) -> String {
    format!("New scraper: {domain}")
}

/// The `new-scraper` issue template (without its front matter), filled in.
pub fn issue_body(url: &str, domain: &str, note: Option<&str>) -> String {
    let template = TEMPLATE
        .strip_prefix("---")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(TEMPLATE, |(_, body)| body)
        .trim();
    let filled =
        template
            .replacen("<name>", domain, 1)
            .replacen("<https://...>", &format!("<{url}>"), 1);
    let note = note
        .map(|n| format!("**Submitter's note** (unverified):\n\n{}\n\n", fenced(n)))
        .unwrap_or_default();
    format!(
        "Suggested via `POST /v1/suggestions`: **{domain}**\n\n{note}{filled}\n\n\
         _Filed automatically by thaleia-api._"
    )
}

/// "We looked at <name> on <date> and couldn't include it: <reason>".
pub fn refused_message(r: &RefusedSourceRow) -> String {
    format!(
        "We looked at {} on {} and couldn't include it: {}",
        r.name,
        r.checked_on.format("%-d %B %Y"),
        r.reason_text
    )
}

/// `text` in a code fence it cannot close early.
fn fenced(text: &str) -> String {
    let longest_run = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest_run.max(2) + 1);
    format!("{fence}text\n{text}\n{fence}")
}

/// The answer to one submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Queued; `issue` is `None` when filing is deferred to the ingest run.
    Accepted {
        domain: String,
        issue: Option<i64>,
    },
    AlreadySuggested {
        domain: String,
    },
    AlreadyCovered {
        source: String,
    },
    /// We looked at this site before and decided not to scrape it
    /// (`events.refused_sources`); nothing is filed.
    Refused(Box<RefusedSourceRow>),
    RateLimited {
        retry_after_secs: u64,
    },
    Invalid(Invalid),
}

pub struct Suggestions {
    ip_salt: String,
    per_hour: u32,
    per_day: u32,
    trusted_proxies: usize,
    filer: Option<Box<dyn IssueFiler>>,
}

impl Suggestions {
    pub fn new(
        config: SuggestionConfig,
        filer: Option<Box<dyn IssueFiler>>,
    ) -> anyhow::Result<Self> {
        let Some(ip_salt) = config.ip_salt else {
            anyhow::bail!("SUGGESTION_IP_SALT must be set");
        };
        Ok(Self {
            ip_salt,
            per_hour: config.per_hour,
            per_day: config.per_day,
            trusted_proxies: config.trusted_proxies,
            filer,
        })
    }

    pub fn client_ip<'a>(
        &self,
        peer: IpAddr,
        forwarded_for: impl IntoIterator<Item = &'a str>,
    ) -> IpAddr {
        client_ip(peer, forwarded_for, self.trusted_proxies)
    }

    pub async fn submit(
        &self,
        pool: &PgPool,
        url: &str,
        note: Option<&str>,
        client: IpAddr,
    ) -> anyhow::Result<Outcome> {
        let sub = match validate(url, note) {
            Ok(s) => s,
            Err(e) => return Ok(Outcome::Invalid(e)),
        };
        let ip_hash = ip_hash(client, &self.ip_salt);
        let mut tx = pool.begin().await?;
        repo::lock_suggestion_submitter(&mut tx, &ip_hash).await?;
        let mut retry_after: Option<f64> = None;
        for (window, limit) in [(HOUR, self.per_hour), (DAY, self.per_day)] {
            if let Some(s) = repo::suggestion_retry_after(&mut tx, &ip_hash, window, limit).await? {
                retry_after = Some(retry_after.map_or(s, |r| r.max(s)));
            }
        }
        if let Some(secs) = retry_after {
            return Ok(Outcome::RateLimited {
                retry_after_secs: secs.ceil().max(1.0) as u64,
            });
        }

        let new = NewSuggestion {
            url: sub.url.clone(),
            domain: sub.domain.clone(),
            note: sub.note.clone(),
            submitter_ip_hash: ip_hash,
        };
        let covered_by = repo::source_domains(&mut tx)
            .await?
            .into_iter()
            .find(|(_, d)| registrable_domain(d).as_deref() == Some(sub.domain.as_str()));
        let refused = repo::refused_source_list(&mut tx)
            .await?
            .into_iter()
            .find(|r| registrable_domain(&r.domain).as_deref() == Some(sub.domain.as_str()));
        if let Some(refused) = refused {
            repo::insert_refused_suggestion(&mut tx, &new).await?;
            tx.commit().await?;
            return Ok(Outcome::Refused(Box::new(refused)));
        }
        if let Some((source, _)) = covered_by {
            repo::insert_duplicate_suggestion(&mut tx, &new).await?;
            tx.commit().await?;
            return Ok(Outcome::AlreadyCovered { source });
        }
        let Some(id) = repo::insert_pending_suggestion(&mut tx, &new).await? else {
            repo::insert_duplicate_suggestion(&mut tx, &new).await?;
            tx.commit().await?;
            return Ok(Outcome::AlreadySuggested { domain: sub.domain });
        };
        tx.commit().await?;

        let issue = match self.filer.as_deref() {
            None => {
                tracing::warn!(domain = %sub.domain, "no GitHub filer; suggestion left pending");
                None
            }
            Some(filer) => {
                match tokio::time::timeout(GITHUB_TIMEOUT, file_issue(pool, filer, id, &sub)).await
                {
                    Ok(Ok(n)) => Some(n),
                    Ok(Err(e)) => {
                        tracing::warn!(domain = %sub.domain, error = %e, "filing suggestion failed; left pending");
                        None
                    }
                    Err(_) => {
                        tracing::warn!(domain = %sub.domain, "filing suggestion timed out; left pending");
                        None
                    }
                }
            }
        };
        Ok(Outcome::Accepted {
            domain: sub.domain,
            issue,
        })
    }
}

async fn file_issue(
    pool: &PgPool,
    filer: &dyn IssueFiler,
    id: i64,
    sub: &Submission,
) -> anyhow::Result<i64> {
    let body = issue_body(&sub.url, &sub.domain, sub.note.as_deref());
    let n = filer
        .create_issue(&issue_title(&sub.domain), &body, &[LABEL])
        .await?;
    repo::mark_suggestion_filed(pool, id, n).await?;
    Ok(n)
}

/// File issues for suggestions left `pending` (created before
/// `now - RETRY_GRACE`). An open `new-scraper` issue with the same title is
/// adopted instead of filing a second one. Returns the issue numbers.
pub async fn file_pending(
    pool: &PgPool,
    filer: &dyn IssueFiler,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<i64>> {
    let pending = repo::pending_suggestions(pool, now - RETRY_GRACE).await?;
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    let open = filer.list_open_issues(LABEL).await?;
    let mut filed = Vec::new();
    for s in pending {
        let title = issue_title(&s.domain);
        let n = match open.iter().find(|i| i.title == title) {
            Some(existing) => existing.number,
            None => {
                let body = issue_body(&s.url, &s.domain, s.note.as_deref());
                filer.create_issue(&title, &body, &[LABEL]).await?
            }
        };
        repo::mark_suggestion_filed(pool, s.id, n).await?;
        filed.push(n);
    }
    Ok(filed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrable_domain_strips_subdomains() {
        for (host, want) in [
            (
                "www.serpentinegalleries.org",
                Some("serpentinegalleries.org"),
            ),
            ("app.ticketmaster.com", Some("ticketmaster.com")),
            ("WWW.Barbican.org.uk.", Some("barbican.org.uk")),
            ("designmuseum.org", Some("designmuseum.org")),
            ("localhost", None),
            ("foo.localhost", None),
            ("printer.local", None),
            ("intranet", None),
            ("co.uk", None),
        ] {
            assert_eq!(registrable_domain(host).as_deref(), want, "{host}");
        }
    }

    #[test]
    fn validate_accepts_public_http_urls() {
        let s = validate(
            "  HTTP://www.Example.org/events?x=1  ",
            Some("  small gallery  "),
        )
        .unwrap();
        assert_eq!(s.url, "http://www.example.org/events?x=1");
        assert_eq!(s.domain, "example.org");
        assert_eq!(s.note.as_deref(), Some("small gallery"));
        assert_eq!(
            validate("https://example.org", Some("  ")).unwrap().note,
            None
        );
    }

    #[test]
    fn validate_rejects() {
        let long_url = format!("https://example.org/{}", "a".repeat(MAX_URL_LEN));
        for (url, want) in [
            ("not a url", Invalid::Unparseable),
            ("/relative", Invalid::Unparseable),
            ("ftp://example.org/", Invalid::Scheme),
            ("javascript:alert(1)", Invalid::Scheme),
            ("https://user:pw@example.org/", Invalid::Credentials),
            ("http://127.0.0.1/", Invalid::NotPublic),
            ("http://0x7f.1/", Invalid::NotPublic),
            ("http://10.0.0.5:8080/", Invalid::NotPublic),
            ("http://[::1]/", Invalid::NotPublic),
            ("http://localhost/", Invalid::NotPublic),
            ("http://nas.internal/", Invalid::NotPublic),
            (long_url.as_str(), Invalid::UrlTooLong),
        ] {
            assert_eq!(validate(url, None), Err(want), "{url}");
        }
        assert_eq!(
            validate("https://example.org", Some(&"é".repeat(MAX_NOTE_CHARS + 1))),
            Err(Invalid::NoteTooLong)
        );
        assert!(validate("https://example.org", Some(&"é".repeat(MAX_NOTE_CHARS))).is_ok());
    }

    #[test]
    fn client_ip_honours_only_trusted_hops() {
        let peer: IpAddr = "10.1.2.3".parse().unwrap();
        let xff = ["6.6.6.6, 1.2.3.4", "5.6.7.8"];
        assert_eq!(client_ip(peer, xff, 0), peer);
        assert_eq!(
            client_ip(peer, xff, 1),
            "5.6.7.8".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(peer, xff, 2),
            "1.2.3.4".parse::<IpAddr>().unwrap()
        );
        assert_eq!(client_ip(peer, xff, 4), peer);
        assert_eq!(client_ip(peer, ["garbage"], 1), peer);
        assert_eq!(client_ip(peer, [], 1), peer);
    }

    #[test]
    fn ip_hash_is_salted() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert_eq!(ip_hash(ip, "a"), ip_hash(ip, "a"));
        assert_ne!(ip_hash(ip, "a"), ip_hash(ip, "b"));
        assert_eq!(ip_hash(ip, "a").len(), 64);
    }

    #[test]
    fn issue_body_fills_the_template() {
        let body = issue_body(
            "https://www.example.org/events",
            "example.org",
            Some("see ```the``` listings"),
        );
        assert!(!body.contains("name: New scraper"), "front matter kept");
        assert!(body.contains("**Site:** example.org"));
        assert!(body.contains("**Events page URL:** <https://www.example.org/events>"));
        assert!(body.contains("````text\nsee ```the``` listings\n````"));
        assert!(body.contains("### Checklist"));
        assert!(
            !issue_body("https://example.org/", "example.org", None).contains("Submitter's note")
        );
    }
}
