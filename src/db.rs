//! Database connection and migrations.
//!
//! Schema isolation: Muse & Mingle shares a production Postgres with other apps, so
//! everything it creates lives in the `events` schema. That includes sqlx's
//! migration bookkeeping table, which is configured as
//! `events._sqlx_migrations` in `sqlx.toml` (read by `migrate!` at compile
//! time) and re-asserted here at runtime.

use std::str::FromStr;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// The only schema Muse & Mingle may touch.
pub const SCHEMA: &str = "events";
/// Fully-qualified migration bookkeeping table.
pub const MIGRATIONS_TABLE: &str = "events._sqlx_migrations";

/// Migrations embedded at compile time (configured by `sqlx.toml`).
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(
        "schema `events` does not exist and this role cannot create it; \
         run ops/sql/create-role.sql as the database owner first: {0}"
    )]
    SchemaBootstrap(#[source] sqlx::Error),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Connection options for `url` with `search_path=events` set at startup,
/// so that even an accidental unqualified name resolves inside `events`.
pub fn connect_options(url: &str) -> Result<PgConnectOptions, sqlx::Error> {
    Ok(PgConnectOptions::from_str(url)?.options([("search_path", SCHEMA)]))
}

/// Connect a pool (search_path pinned to `events`).
pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(connect_options(url)?)
        .await
}

/// Create the `events` schema if (and only if) it is missing.
///
/// We deliberately do not use `CREATE SCHEMA IF NOT EXISTS` unconditionally:
/// Postgres checks CREATE-on-database before the IF NOT EXISTS short-circuit,
/// so it fails for the restricted `musenmingle` role even when the schema exists.
pub async fn ensure_schema(pool: &PgPool) -> Result<(), DbError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1)",
    )
    .bind(SCHEMA)
    .fetch_one(pool)
    .await?;
    if !exists {
        sqlx::query("CREATE SCHEMA events")
            .execute(pool)
            .await
            .map_err(DbError::SchemaBootstrap)?;
    }
    Ok(())
}

/// The migrator with the bookkeeping table pinned to `events`.
pub fn migrator() -> Migrator {
    let mut m = sqlx::migrate!("./migrations");
    // Redundant with sqlx.toml, kept so the invariant does not hinge on a
    // config file being picked up by the macro.
    m.dangerous_set_table_name(MIGRATIONS_TABLE);
    m
}

/// Bootstrap the schema and apply pending migrations.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    ensure_schema(pool).await?;
    migrator().run(pool).await?;
    Ok(())
}

/// Re-run the idempotent embeddings migration (see its header): it creates
/// `events.event_embeddings` only once pgvector is usable (the owner granted
/// USAGE on schema `extensions`), so the ingest calls this on every start and
/// embeddings switch on without a new migration.
pub async fn ensure_optional_schema(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(include_str!(
        "../migrations/20260927000002_event_embeddings.sql"
    ))
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlx_toml_puts_bookkeeping_table_in_events() {
        // Proves sqlx.toml was honoured by the `migrate!` macro, which is what
        // sqlx-cli would use too.
        assert_eq!(MIGRATOR.table_name, MIGRATIONS_TABLE);
        assert!(MIGRATOR.create_schemas.is_empty());
        assert_eq!(migrator().table_name, MIGRATIONS_TABLE);
        assert_eq!(migrator().migrations.len(), MIGRATOR.migrations.len());
    }
}
