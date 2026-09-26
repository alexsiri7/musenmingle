//! `thaleia-api`: the HTTP service (`GET /healthz`, `POST /v1/suggestions`).

use std::net::SocketAddr;

use anyhow::Context;
use thaleia::github::{DEFAULT_API_BASE, GitHubIssueFiler, IssueFiler};
use thaleia::suggestions::Suggestions;
use thaleia::{api, config::Config, db};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    thaleia::init_tracing();
    let config = Config::from_env()?;
    let filer: Option<Box<dyn IssueFiler>> = match &config.github_token {
        Some(token) => Some(Box::new(GitHubIssueFiler::new(
            DEFAULT_API_BASE,
            &config.github_repo,
            token,
        )?)),
        None => {
            tracing::warn!("GITHUB_TOKEN not set; suggestions are filed by the next ingest run");
            None
        }
    };
    let suggestions = Suggestions::new(config.suggestions.clone(), filer)?;
    let pool = db::connect(&config.database_url)
        .await
        .context("connecting to the database")?;
    db::migrate(&pool).await.context("running migrations")?;

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, version = thaleia::VERSION, "thaleia-api listening");
    let app = api::router(pool, suggestions).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!("shutting down");
}
