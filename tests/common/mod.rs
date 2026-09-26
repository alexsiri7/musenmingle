//! Shared helpers for integration tests.
//!
//! Database tests need `TEST_DATABASE_URL` pointing at any Postgres server
//! where the user may `CREATE DATABASE` (CI uses a `postgres:17` service).
//! Each test gets its own fresh database, dropped afterwards. When the
//! variable is unset the test prints a notice and passes, unless
//! `MUSENMINGLE_REQUIRE_DB=1` (set in CI) in which case it fails.
#![allow(dead_code)]

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{AssertSqlSafe, ConnectOptions};
use std::str::FromStr;

pub struct TestDb {
    pub name: String,
    pub admin_url: String,
    /// Options for the fresh database (no search_path override).
    pub options: PgConnectOptions,
}

/// Whether the test server has pgvector (then every fresh database has it in
/// schema `extensions`, like production).
pub async fn has_pgvector(pool: &PgPool) -> bool {
    sqlx::query_scalar("SELECT to_regtype('extensions.vector') IS NOT NULL")
        .fetch_one(pool)
        .await
        .unwrap_or(false)
}

impl TestDb {
    /// Create a fresh, empty database, or `None` (with a notice) if
    /// `TEST_DATABASE_URL` is not set.
    pub async fn create(test_name: &str) -> Option<TestDb> {
        let Ok(admin_url) = std::env::var("TEST_DATABASE_URL") else {
            if std::env::var("MUSENMINGLE_REQUIRE_DB").is_ok_and(|v| v == "1") {
                panic!("MUSENMINGLE_REQUIRE_DB=1 but TEST_DATABASE_URL is not set");
            }
            notice(&format!(
                "SKIPPING {test_name}: TEST_DATABASE_URL is not set (see README: Local development)"
            ));
            return None;
        };
        let name = format!("musenmingle_test_{}", uuid::Uuid::new_v4().simple());
        let admin = PgConnectOptions::from_str(&admin_url).expect("valid TEST_DATABASE_URL");
        let mut conn = admin.connect().await.expect("connect to TEST_DATABASE_URL");
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&mut conn)
            .await
            .expect("CREATE DATABASE");
        let options = admin.clone().database(&name);
        // Mirror the shared production database: pgvector installed in an
        // `extensions` schema (by the owner, not by our migrations). Skipped
        // when the server has no pgvector; embeddings then stay off.
        let mut fresh = options
            .connect()
            .await
            .expect("connect to the fresh database");
        let has_vector: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'vector')",
        )
        .fetch_one(&mut fresh)
        .await
        .expect("pg_available_extensions");
        if has_vector {
            sqlx::raw_sql(
                "CREATE SCHEMA IF NOT EXISTS extensions;
                 CREATE EXTENSION IF NOT EXISTS vector SCHEMA extensions;",
            )
            .execute(&mut fresh)
            .await
            .expect("create pgvector in schema extensions");
        }
        drop(fresh);
        Some(TestDb {
            name,
            admin_url,
            options,
        })
    }

    /// A pool on the fresh database WITHOUT the application's search_path
    /// pinning (so unqualified names would land in `public`).
    pub async fn raw_pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(4)
            .connect_with(self.options.clone())
            .await
            .expect("connect raw pool")
    }

    /// URL of the fresh database.
    pub fn url(&self) -> String {
        let mut u = url::Url::parse(&self.admin_url).expect("valid url");
        u.set_path(&format!("/{}", self.name));
        u.to_string()
    }

    /// A pool configured exactly like the binaries (search_path=events),
    /// with migrations applied.
    pub async fn migrated_pool(&self) -> PgPool {
        let pool = musenmingle::db::connect(&self.url())
            .await
            .expect("connect app pool");
        musenmingle::db::migrate(&pool).await.expect("migrate");
        pool
    }

    pub async fn drop_db(self) {
        let admin = PgConnectOptions::from_str(&self.admin_url).unwrap();
        let mut conn = admin.connect().await.unwrap();
        sqlx::raw_sql(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.name
        )))
        .execute(&mut conn)
        .await
        .expect("DROP DATABASE");
    }
}

/// Print a line that stays visible although libtest captures test output
/// (it captures `std::io::stderr()` too, so write to fd 2 directly).
pub fn notice(line: &str) {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::fd::FromRawFd;
        // SAFETY: fd 2 is open for the life of the process; ManuallyDrop
        // prevents closing it.
        let mut err = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(2) });
        let _ = writeln!(err, "{line}");
    }
    #[cfg(not(unix))]
    eprintln!("{line}");
}

/// Read a fixture file relative to `tests/fixtures`.
pub fn fixture(path: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}
