//! Muse & Mingle: London cultural events for creative people.
//!
//! This crate contains the ingestion backend (sources, normalisation, the
//! ingest runner and health checks) and the HTTP API. See
//! `README.md` for the architecture overview and `CLAUDE.md` for invariants.

pub mod api;
pub mod calendar;
pub mod config;
pub mod contact;
pub mod db;
pub mod enrich;
pub mod fetch;
pub mod github;
pub mod health;
pub mod host_redirect;
pub mod hours;
pub mod ics;
pub mod listing;
pub mod matching;
pub mod model;
pub mod normalise;
pub mod notify;
pub mod repo;
pub mod runner;
pub mod search;
pub mod share;
pub mod sources;
pub mod suggestions;
pub mod thumbs;
pub mod web;

/// Crate version, used in the bot User-Agent.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Initialise `tracing` from `RUST_LOG` (default `info`).
pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
