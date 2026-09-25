//! Shared helpers for integration tests.
//!
//! Database tests need `TEST_DATABASE_URL` pointing at any Postgres server
//! where the user may `CREATE DATABASE` (CI uses a `postgres:17` service).
//! Each test gets its own fresh database, dropped afterwards. When the
//! variable is unset the test prints a notice and passes, unless
//! `THALEIA_REQUIRE_DB=1` (set in CI) in which case it fails.
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

impl TestDb {
    /// Create a fresh, empty database, or `None` (with a notice) if
    /// `TEST_DATABASE_URL` is not set.
    pub async fn create(test_name: &str) -> Option<TestDb> {
        let Ok(admin_url) = std::env::var("TEST_DATABASE_URL") else {
            if std::env::var("THALEIA_REQUIRE_DB").is_ok_and(|v| v == "1") {
                panic!("THALEIA_REQUIRE_DB=1 but TEST_DATABASE_URL is not set");
            }
            eprintln!(
                "SKIPPING {test_name}: TEST_DATABASE_URL is not set (see README: Local development)"
            );
            return None;
        };
        let name = format!("thaleia_test_{}", uuid::Uuid::new_v4().simple());
        let admin = PgConnectOptions::from_str(&admin_url).expect("valid TEST_DATABASE_URL");
        let mut conn = admin.connect().await.expect("connect to TEST_DATABASE_URL");
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&mut conn)
            .await
            .expect("CREATE DATABASE");
        let options = admin.clone().database(&name);
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
        let pool = thaleia::db::connect(&self.url())
            .await
            .expect("connect app pool");
        thaleia::db::migrate(&pool).await.expect("migrate");
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

/// Read a fixture file relative to `tests/fixtures`.
pub fn fixture(path: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}
