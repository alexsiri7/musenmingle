//! Owner notifications via ntfy (https://ntfy.sh): `POST <base>/<topic>`
//! with the message as the body. Used for operational alerts that need a
//! human (e.g. Requesty credits exhausted). The topic is a secret (anyone
//! who knows it can read the messages) and is never logged.

use async_trait::async_trait;

pub const DEFAULT_NTFY_BASE: &str = "https://ntfy.sh";

#[async_trait]
pub trait Notifier: Send + Sync {
    /// `priority`: ntfy priority name (`min`, `low`, `default`, `high`, `urgent`).
    async fn notify(&self, title: &str, body: &str, priority: &str) -> anyhow::Result<()>;
}

/// Sends to an ntfy topic.
pub struct Ntfy {
    http: reqwest::Client,
    url: String,
}

impl Ntfy {
    pub fn new(base: &str, topic: &str) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()?,
            url: format!("{}/{}", base.trim_end_matches('/'), topic),
        })
    }
}

#[async_trait]
impl Notifier for Ntfy {
    async fn notify(&self, title: &str, body: &str, priority: &str) -> anyhow::Result<()> {
        let resp = self
            .http
            .post(&self.url)
            .header("Title", title)
            .header("Priority", priority)
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("ntfy request failed: {}", e.without_url()))?;
        if !resp.status().is_success() {
            anyhow::bail!("ntfy answered {}", resp.status());
        }
        Ok(())
    }
}

/// No topic configured: log instead.
pub struct LogNotifier;

#[async_trait]
impl Notifier for LogNotifier {
    async fn notify(&self, title: &str, body: &str, priority: &str) -> anyhow::Result<()> {
        tracing::warn!(title, body, priority, "notification (NTFY_TOPIC not set)");
        Ok(())
    }
}
