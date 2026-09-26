//! Proves Thaleia never touches anything outside the `events` schema.

mod common;

use common::TestDb;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, ConnectOptions, PgPool};
use std::collections::BTreeSet;
use std::str::FromStr;
use tokio::sync::Mutex;

/// Every relation, TOAST owner, type, function, schema and extension, as
/// `kind:schema.name`.
async fn catalog_snapshot(pool: &PgPool) -> BTreeSet<String> {
    let rows: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT 'rel:' || n.nspname || '.' || c.relname || ':' || c.relkind::text
          FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname <> 'pg_toast'
        UNION ALL
        -- TOAST tables (and their indexes) live in pg_toast by definition;
        -- attribute each one to the schema of the table that owns it.
        SELECT 'toast:' || n.nspname || '.' || o.relname
          FROM pg_catalog.pg_class o JOIN pg_catalog.pg_namespace n ON n.oid = o.relnamespace
         WHERE o.reltoastrelid <> 0
        UNION ALL
        SELECT 'orphan-toast:pg_toast.' || c.relname
          FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'pg_toast' AND c.relkind = 't'
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class o WHERE o.reltoastrelid = c.oid)
        UNION ALL
        SELECT 'type:' || n.nspname || '.' || t.typname
          FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace
        UNION ALL
        SELECT 'proc:' || n.nspname || '.' || p.proname
          FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
        UNION ALL
        SELECT 'schema:' || nspname FROM pg_catalog.pg_namespace
        UNION ALL
        SELECT 'ext:' || extname FROM pg_catalog.pg_extension
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter().map(|(s,)| s).collect()
}

fn schema_of(entry: &str) -> Option<&str> {
    let rest = entry.split_once(':')?.1;
    if entry.starts_with("schema:") {
        return Some(rest);
    }
    if entry.starts_with("ext:") {
        return None;
    }
    rest.split_once('.').map(|(s, _)| s)
}

#[tokio::test]
async fn migrations_create_nothing_outside_events_schema() {
    let Some(db) = TestDb::create("migrations_create_nothing_outside_events_schema").await else {
        return;
    };
    // Deliberately NOT the app pool: default search_path ("$user", public),
    // so any unqualified CREATE in a migration would land in `public`.
    let raw = db.raw_pool().await;
    let before = catalog_snapshot(&raw).await;

    thaleia::db::migrate(&raw).await.expect("first migrate");
    thaleia::db::migrate(&raw)
        .await
        .expect("second migrate is a no-op");

    let after = catalog_snapshot(&raw).await;
    let added: Vec<&String> = after.difference(&before).collect();
    let removed: Vec<&String> = before.difference(&after).collect();
    assert!(
        removed.is_empty(),
        "migrations removed objects: {removed:?}"
    );
    assert!(!added.is_empty());

    let outside: Vec<&&String> = added
        .iter()
        .filter(|e| schema_of(e) != Some("events"))
        .collect();
    assert!(
        outside.is_empty(),
        "objects created outside the `events` schema: {outside:?}"
    );

    // The bookkeeping table specifically.
    let (in_events, in_public): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT to_regclass('events._sqlx_migrations')::text, to_regclass('public._sqlx_migrations')::text",
    )
    .fetch_one(&raw)
    .await
    .unwrap();
    assert_eq!(in_events.as_deref(), Some("events._sqlx_migrations"));
    assert_eq!(in_public, None);
    let applied: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events._sqlx_migrations WHERE success")
            .fetch_one(&raw)
            .await
            .unwrap();
    assert_eq!(applied as usize, thaleia::db::migrator().iter().count());

    // Sanity: the tables we expect exist.
    for t in [
        "sources",
        "events",
        "event_sources",
        "source_runs",
        "health_issues",
        "site_suggestions",
        "merge_overrides",
    ] {
        assert!(
            added
                .iter()
                .any(|e| e.as_str() == format!("rel:events.{t}:r")),
            "missing table events.{t}"
        );
    }

    raw.close().await;
    db.drop_db().await;
}

/// `thaleia` is a cluster-wide role, so tests that create or drop it must
/// not overlap.
static THALEIA_ROLE: Mutex<()> = Mutex::const_new(());

fn create_role_script() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ops/sql/create-role.sql"),
    )
    .unwrap()
}

/// A single-connection pool that logs in as the test superuser and then
/// switches to `role` (avoids needing a password / pg_hba entry in CI).
async fn pool_as(options: PgConnectOptions, role: &str) -> PgPool {
    let set_role = format!("SET ROLE {role}");
    PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |conn, _| {
            let set_role = set_role.clone();
            Box::pin(async move {
                sqlx::raw_sql(AssertSqlSafe(set_role)).execute(conn).await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .unwrap()
}

/// Runs ops/sql/create-role.sql twice (it must be idempotent) on `pool`.
async fn run_create_role(pool: &PgPool) {
    let script = create_role_script();
    sqlx::raw_sql(AssertSqlSafe(script.clone()))
        .execute(pool)
        .await
        .expect("create-role.sql");
    sqlx::raw_sql(AssertSqlSafe(script))
        .execute(pool)
        .await
        .expect("create-role.sql rerun");
}

/// Checks the `thaleia` role's attributes, then migrates AS the role and
/// checks it cannot reach other schemas.
async fn assert_thaleia_confined(db: &TestDb, admin: &PgPool) {
    let attrs: (bool, bool, bool, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolinherit, rolreplication, rolbypassrls
           FROM pg_catalog.pg_roles WHERE rolname = 'thaleia'",
    )
    .fetch_one(admin)
    .await
    .unwrap();
    assert_eq!(attrs, (true, false, false, false, false, false, false));

    // A sentinel table in another schema the role must not see.
    sqlx::raw_sql(
        "CREATE SCHEMA other_app; CREATE TABLE other_app.secrets (x int); INSERT INTO other_app.secrets VALUES (1);
         CREATE TABLE public.public_secrets (x int);",
    )
    .execute(admin)
    .await
    .unwrap();

    let as_role = pool_as(
        db.options.clone().options([("search_path", "events")]),
        "thaleia",
    )
    .await;
    let who: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&as_role)
        .await
        .unwrap();
    assert_eq!(who, "thaleia");

    thaleia::db::migrate(&as_role)
        .await
        .expect("migrate as thaleia");
    thaleia::db::migrate(&as_role)
        .await
        .expect("re-migrate as thaleia");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM events.sources")
        .fetch_one(&as_role)
        .await
        .unwrap();
    assert!(n >= 2);

    for (sql, what) in [
        ("CREATE TABLE public.nope (x int)", "create in public"),
        ("SELECT * FROM other_app.secrets", "read other schema"),
        ("SELECT * FROM public.public_secrets", "read public table"),
        ("CREATE SCHEMA another", "create schema"),
    ] {
        let r = sqlx::raw_sql(sql).execute(&as_role).await;
        assert!(r.is_err(), "thaleia role should not be able to {what}");
    }

    as_role.close().await;
}

/// Runs ops/sql/create-role.sql as the superuser.
#[tokio::test]
async fn restricted_role_can_migrate_but_not_touch_other_schemas() {
    let _role = THALEIA_ROLE.lock().await;
    let Some(db) = TestDb::create("restricted_role_can_migrate_but_not_touch_other_schemas").await
    else {
        return;
    };
    let admin = db.raw_pool().await;
    run_create_role(&admin).await;
    assert_thaleia_confined(&db, &admin).await;

    admin.close().await;
    db.drop_db().await;
}

/// Runs ops/sql/create-role.sql as a database owner that is not a superuser
/// but has what Supabase's `postgres` role has (CREATEROLE, REPLICATION,
/// BYPASSRLS).
#[tokio::test]
async fn create_role_script_runs_as_non_superuser_owner() {
    let _role = THALEIA_ROLE.lock().await;
    let Some(db) = TestDb::create("create_role_script_runs_as_non_superuser_owner").await else {
        return;
    };
    let admin = db.raw_pool().await;
    // A `thaleia` left by another test (or run) was created by the superuser;
    // this owner must create its own.
    sqlx::raw_sql("DROP ROLE IF EXISTS thaleia")
        .execute(&admin)
        .await
        .expect("drop stale thaleia role");
    let owner = format!("thaleia_test_owner_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(AssertSqlSafe(format!(
        "CREATE ROLE {owner} LOGIN NOSUPERUSER CREATEDB CREATEROLE REPLICATION BYPASSRLS;
         ALTER DATABASE {} OWNER TO {owner};",
        db.name
    )))
    .execute(&admin)
    .await
    .unwrap();

    let as_owner = pool_as(db.options.clone(), &owner).await;
    let is_superuser: bool =
        sqlx::query_scalar("SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname = current_user")
            .fetch_one(&as_owner)
            .await
            .unwrap();
    assert!(!is_superuser);
    run_create_role(&as_owner).await;
    as_owner.close().await;

    assert_thaleia_confined(&db, &admin).await;

    admin.close().await;
    let admin_url = db.admin_url.clone();
    db.drop_db().await;
    let mut conn = PgConnectOptions::from_str(&admin_url)
        .unwrap()
        .connect()
        .await
        .unwrap();
    sqlx::raw_sql(AssertSqlSafe(format!(
        "DROP ROLE thaleia; DROP ROLE {owner};"
    )))
    .execute(&mut conn)
    .await
    .expect("drop test roles");
}
