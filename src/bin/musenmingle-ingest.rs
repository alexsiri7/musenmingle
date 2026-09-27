//! `musenmingle-ingest`: one-shot ingestion run (scheduled by a Railway cron).
//!
//! Exits 0 when the run completed, even if individual sources failed (those
//! are recorded in `events.source_runs` and surfaced as GitHub issues by the
//! health checker). Exits non-zero only on infrastructure errors.

use anyhow::Context;
use chrono::Utc;
use musenmingle::config::Config;
use musenmingle::enrich::Enricher;
use musenmingle::enrich::requesty::Requesty;
use musenmingle::fetch::FetchContext;
use musenmingle::github::{DEFAULT_API_BASE, GitHubIssueFiler, IssueFiler};
use musenmingle::health::{HealthChecker, HealthConfig};
use musenmingle::notify::{LogNotifier, Notifier, Ntfy};
use musenmingle::qa::QaChecker;
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
    if let Err(e) = db::ensure_optional_schema(&pool).await {
        tracing::warn!(error = %e, "optional schema (event embeddings) not applied");
    }

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

    let enrich = match &config.requesty_api_key {
        Some(key) => {
            let notifier: Box<dyn Notifier> = match &config.ntfy_topic {
                Some(topic) => Box::new(Ntfy::new(&config.ntfy_base_url, topic)?),
                None => {
                    tracing::warn!("NTFY_TOPIC not set; owner alerts will only be logged");
                    Box::new(LogNotifier)
                }
            };
            Some(Enricher {
                client: Requesty::new(&config.requesty_base_url, key)?,
                notifier,
                config: config.enrich.clone(),
            })
        }
        None => {
            tracing::info!("REQUESTY_API_KEY not set; AI enrichment and embeddings are off");
            None
        }
    };

    let qa = match &config.requesty_api_key {
        Some(key) if config.qa.max_checks_per_run > 0 => Some(QaChecker {
            client: Requesty::new(&config.requesty_base_url, key)?,
            config: config.qa.clone(),
        }),
        _ => {
            tracing::info!("QA checks off (needs REQUESTY_API_KEY and QA_MAX_CHECKS_PER_RUN > 0)");
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
        enrich,
        qa,
    };
    match runner.run_once(Utc::now()).await? {
        RunSummary::Locked => tracing::info!("skipped: another run in progress"),
        RunSummary::Ran(reports) => {
            for r in &reports {
                tracing::info!(
                    source = %r.key, ok = r.ok, events = r.events_found, created = r.created,
                    skipped = r.skipped, errors = r.errors, health = ?r.health,
                    qa_findings = r.qa_findings, qa_check = ?r.qa_check,
                    "source summary"
                );
            }
            tracing::info!(sources = reports.len(), "ingest run complete");
        }
    }
    Ok(())
}
