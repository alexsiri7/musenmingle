//! Minimal GitHub REST client for health issues.
//!
//! This is NOT a scraping client and deliberately does not use
//! [`crate::fetch::FetchContext`]: it talks to an authenticated API
//! (api.github.com's robots.txt is irrelevant to API calls), with its own
//! User-Agent (GitHub rejects requests without one).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

/// Minimal view of an issue.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IssueRef {
    pub number: i64,
    pub title: String,
}

/// Where health issues are filed. A trait so tests can use wiremock or fakes.
#[async_trait]
pub trait IssueFiler: Send + Sync {
    /// Open issues (not PRs) carrying `label`.
    async fn list_open_issues(&self, label: &str) -> anyhow::Result<Vec<IssueRef>>;
    async fn create_issue(&self, title: &str, body: &str, labels: &[&str]) -> anyhow::Result<i64>;
    async fn comment(&self, number: i64, body: &str) -> anyhow::Result<()>;
    async fn close_issue(&self, number: i64) -> anyhow::Result<()>;
}

pub const DEFAULT_API_BASE: &str = "https://api.github.com";

pub struct GitHubIssueFiler {
    client: reqwest::Client,
    api_base: String,
    repo: String,
    token: String,
}

#[derive(Deserialize)]
struct ApiIssue {
    number: i64,
    title: String,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

impl GitHubIssueFiler {
    pub fn new(api_base: &str, repo: &str, token: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(crate::fetch::user_agent())
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            client,
            api_base: api_base.trim_end_matches('/').to_string(),
            repo: repo.to_string(),
            token: token.to_string(),
        })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(
                method,
                format!("{}/repos/{}{}", self.api_base, self.repo, path),
            )
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    async fn check(resp: reqwest::Response, what: &str) -> anyhow::Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            Ok(resp)
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub {what} failed: HTTP {status}: {}",
                body.chars().take(300).collect::<String>()
            )
        }
    }
}

#[async_trait]
impl IssueFiler for GitHubIssueFiler {
    async fn list_open_issues(&self, label: &str) -> anyhow::Result<Vec<IssueRef>> {
        let mut out = Vec::new();
        for page in 1..=10 {
            let resp = self
                .req(reqwest::Method::GET, "/issues")
                .query(&[
                    ("state", "open"),
                    ("labels", label),
                    ("per_page", "100"),
                    ("page", &page.to_string()),
                ])
                .send()
                .await?;
            let items: Vec<ApiIssue> = Self::check(resp, "list issues").await?.json().await?;
            let n = items.len();
            out.extend(
                items
                    .into_iter()
                    .filter(|i| i.pull_request.is_none())
                    .map(|i| IssueRef {
                        number: i.number,
                        title: i.title,
                    }),
            );
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }

    async fn create_issue(&self, title: &str, body: &str, labels: &[&str]) -> anyhow::Result<i64> {
        let resp = self
            .req(reqwest::Method::POST, "/issues")
            .json(&json!({ "title": title, "body": body, "labels": labels }))
            .send()
            .await?;
        let issue: ApiIssue = Self::check(resp, "create issue").await?.json().await?;
        Ok(issue.number)
    }

    async fn comment(&self, number: i64, body: &str) -> anyhow::Result<()> {
        let resp = self
            .req(reqwest::Method::POST, &format!("/issues/{number}/comments"))
            .json(&json!({ "body": body }))
            .send()
            .await?;
        Self::check(resp, "comment").await?;
        Ok(())
    }

    async fn close_issue(&self, number: i64) -> anyhow::Result<()> {
        let resp = self
            .req(reqwest::Method::PATCH, &format!("/issues/{number}"))
            .json(&json!({ "state": "closed", "state_reason": "completed" }))
            .send()
            .await?;
        Self::check(resp, "close issue").await?;
        Ok(())
    }
}
