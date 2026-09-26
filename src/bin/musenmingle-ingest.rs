//! `musenmingle-ingest`: one-shot ingestion run (scheduled by a Railway cron).
//!
//! Exits 0 when the run completed, even if individual sources failed (those
//! are recorded in `events.source_runs` and surfaced as GitHub issues by the
//! health checker). Exits non-zero only on infrastructure errors.

use anyhow::Context;
use chrono::Utc;
use musenmingle::config::Config;
use musenmingle::fetch::FetchContext;
use musenmingle::github::{DEFAULT_API_BASE, GitHubIssueFiler, IssueFiler};
use musenmingle::health::{HealthChecker, HealthConfig};
use musenmingle::runner::{RunSummary, Runner};
use musenmingle::{db, sources};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    musenmingle::init_tracing();
    let config = Config::from_env()?;
    let pool = db::connect(&config.database_url)
        .await
        .context("connecting to the database")?;
    db::migrate(&pool).await.context("running migrations")?;

    let filer: Option<Box<dyn IssueFiler>> = match &config.github_token {
        Some(token) => Some(Box::new(GitHubIssueFiler::new(
            DEFAULT_API_BASE,
            &config.github_repo,
            token,
        )?)),
        None => {
            tracing::warn!("GITHUB_TOKEN not set; health issues will only be logged");
            None
        }
    };

    let factory_config = config.clone();
    let runner = Runner {
        pool,
        ctx: FetchContext::new(config.rate_limit.clone())?,
        factory: Box::new(move |row| sources::build(row, &factory_config)),
        health: HealthChecker::new(HealthConfig::default(), filer),
        source_timeout: config.source_timeout,
    };
    match runner.run_once(Utc::now()).await? {
        RunSummary::Locked => tracing::info!("skipped: another run in progress"),
        RunSummary::Ran(reports) => {
            for r in &reports {
                tracing::info!(
                    source = %r.key, ok = r.ok, events = r.events_found, created = r.created,
                    skipped = r.skipped, errors = r.errors, health = ?r.health,
                    "source summary"
                );
            }
            tracing::info!(sources = reports.len(), "ingest run complete");
        }
    }
    Ok(())
}
