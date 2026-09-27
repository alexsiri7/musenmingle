//! Every enabled seeded `events.sources` row has an implementation in
//! `sources::build` (the keys are restated in SQL seed migrations). Retired
//! sources (e.g. `artrabbit`, #96) stay as disabled rows with no code.

mod common;

use std::time::Duration;

use common::TestDb;
use musenmingle::config::{Config, RateLimitConfig};
use musenmingle::repo::SourceRow;
use musenmingle::sources;

#[tokio::test]
async fn every_enabled_seeded_source_has_an_implementation() {
    let Some(db) = TestDb::create("every_enabled_seeded_source_has_an_implementation").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let rows: Vec<SourceRow> = sqlx::query_as(
        "SELECT id, key, kind, base_url, domain, interval_minutes, enabled, last_run_at,
                platform, config
           FROM events.sources WHERE enabled",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let config = Config {
        database_url: db.url(),
        ticketmaster_api_key: Some("test-key".into()),
        github_token: None,
        github_repo: "owner/repo".into(),
        port: 0,
        rate_limit: RateLimitConfig::disabled(),
        source_timeout: Duration::from_secs(1),
        suggestions: Default::default(),
        cors_origins: Vec::new(),
        requesty_api_key: None,
        requesty_base_url: String::new(),
        enrich: Default::default(),
        qa: Default::default(),
        ntfy_topic: None,
        ntfy_base_url: String::new(),
        tiles_path: Default::default(),
        transit: musenmingle::transit::TransitConfig::parse(Some("off"), None, None).unwrap(),
    };
    assert!(!rows.is_empty());
    for row in &rows {
        let source = sources::build(row, &config)
            .unwrap_or_else(|reason| panic!("cannot build seeded source {:?}: {reason}", row.key));
        assert_eq!(source.key(), row.key);
    }
    pool.close().await;
    db.drop_db().await;
}
