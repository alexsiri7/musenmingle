-- Thaleia: create the restricted `thaleia` login role.
--
-- Run ONCE (it is idempotent, re-running is safe) as the database owner,
-- e.g. the Supabase `postgres` user, in the target database. The owner need
-- not be a superuser, but needs CREATEROLE, plus REPLICATION and BYPASSRLS
-- (PostgreSQL only lets a role grant or revoke those attributes if it holds
-- them itself); Supabase's `postgres` has all three:
--
--     psql "$OWNER_DATABASE_URL" -v ON_ERROR_STOP=1 -f ops/sql/create-role.sql
--
-- Then set a password (not stored in this file):
--
--     ALTER ROLE thaleia WITH PASSWORD '<generate a long random password>';
--
-- and use it as DATABASE_URL for thaleia-api and thaleia-ingest.
--
-- Result: `thaleia` can log in, has USAGE + CREATE on schema `events` only
-- (it creates and therefore owns the tables via migrations), and gets no
-- privileges on any other schema. No psql meta-commands are used, so the
-- script can also be run verbatim by tests (tests/schema_isolation.rs).

-- 1. The role (race-safe: roles are cluster-wide).
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'thaleia') THEN
        BEGIN
            CREATE ROLE thaleia LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;
        END;
    END IF;
END
$$;

-- Keep attributes correct even if the role pre-existed. Only a superuser may
-- mention NOSUPERUSER at all, so it is applied only when the role really is a
-- superuser (then a non-superuser owner fails here, as it should).
DO $$
BEGIN
    IF (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname = 'thaleia') THEN
        ALTER ROLE thaleia NOSUPERUSER;
    END IF;
END
$$;
ALTER ROLE thaleia LOGIN NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
-- Unqualified names resolve inside `events` only.
ALTER ROLE thaleia SET search_path = events;

-- 2. The schema, owned by the operator (not by thaleia), so thaleia cannot
--    drop it or change its privileges.
CREATE SCHEMA IF NOT EXISTS events;

-- 3. Privileges on `events` only.
GRANT USAGE, CREATE ON SCHEMA events TO thaleia;
GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA events TO thaleia;
GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA events TO thaleia;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA events TO thaleia;
-- Objects the operator creates in `events` later are usable by thaleia too.
ALTER DEFAULT PRIVILEGES IN SCHEMA events GRANT ALL PRIVILEGES ON TABLES TO thaleia;
ALTER DEFAULT PRIVILEGES IN SCHEMA events GRANT ALL PRIVILEGES ON SEQUENCES TO thaleia;
ALTER DEFAULT PRIVILEGES IN SCHEMA events GRANT EXECUTE ON FUNCTIONS TO thaleia;

-- 4. Nothing elsewhere. Explicitly strip anything that may have been granted
--    to the role directly (PUBLIC grants are shared by every role and are
--    managed by the database owner; on PostgreSQL 15+ PUBLIC no longer has
--    CREATE on `public`).
REVOKE ALL PRIVILEGES ON SCHEMA public FROM thaleia;
REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM thaleia;
REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public FROM thaleia;
DO $$
BEGIN
    -- The role may connect to this database but not create schemas in it.
    EXECUTE format('REVOKE CREATE, TEMPORARY ON DATABASE %I FROM thaleia', current_database());
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO thaleia', current_database());
END
$$;
