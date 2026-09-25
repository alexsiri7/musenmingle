//! `thaleia-api`: the HTTP service (currently only `GET /healthz`).

use anyhow::Context;
use thaleia::{api, config::Config, db};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    thaleia::init_tracing();
    let config = Config::from_env()?;
    let pool = db::connect(&config.database_url)
        .await
        .context("connecting to the database")?;
    db::migrate(&pool).await.context("running migrations")?;

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, version = thaleia::VERSION, "thaleia-api listening");
    axum::serve(listener, api::router(pool))
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
