//! `musenmingle-api`: the HTTP service (health check, read API, site suggestions).

use std::net::SocketAddr;

use anyhow::Context;
use musenmingle::github::{DEFAULT_API_BASE, GitHubIssueFiler, IssueFiler};
use musenmingle::host_redirect::HostRedirect;
use musenmingle::suggestions::Suggestions;
use musenmingle::{api, config::Config, db, host_redirect};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    musenmingle::init_tracing();
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
    tracing::info!(%addr, version = musenmingle::VERSION, "musenmingle-api listening");
    let settings = api::ApiSettings {
        github_repo: config.github_repo.clone(),
        cors_origins: config.cors_origins.clone(),
    };
    let redirect = HostRedirect::from_env();
    if redirect.is_active() {
        tracing::info!("redirecting legacy hosts to CANONICAL_HOST");
    }
    let router = api::router_with_tiles(pool, suggestions, settings, Some(&config.tiles_path));
    let app =
        host_redirect::apply(router, redirect).into_make_service_with_connect_info::<SocketAddr>();
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
