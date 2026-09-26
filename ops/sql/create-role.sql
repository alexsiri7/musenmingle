-- Muse & Mingle: create the restricted `musenmingle` login role.
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
--     ALTER ROLE musenmingle WITH PASSWORD '<generate a long random password>';
--
-- and use it as DATABASE_URL for musenmingle-api and musenmingle-ingest.
--
-- Result: `musenmingle` can log in, has USAGE + CREATE on schema `events` only
-- (it creates and therefore owns the tables via migrations), USAGE on the
-- `extensions` schema when it exists (to use pgvector's type), and no other
-- privileges on any other schema. No psql meta-commands are used, so the
-- script can also be run verbatim by tests (tests/schema_isolation.rs).

-- 1. The role (race-safe: roles are cluster-wide).
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'musenmingle') THEN
        BEGIN
            CREATE ROLE musenmingle LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
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
    IF (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname = 'musenmingle') THEN
        ALTER ROLE musenmingle NOSUPERUSER;
    END IF;
END
$$;
ALTER ROLE musenmingle LOGIN NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
-- Unqualified names resolve inside `events` only.
ALTER ROLE musenmingle SET search_path = events;

-- 2. The schema, owned by the operator (not by musenmingle), so musenmingle cannot
--    drop it or change its privileges.
CREATE SCHEMA IF NOT EXISTS events;

-- 3. Privileges on `events` only.
GRANT USAGE, CREATE ON SCHEMA events TO musenmingle;
GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA events TO musenmingle;
GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA events TO musenmingle;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA events TO musenmingle;
-- Objects the operator creates in `events` later are usable by musenmingle too.
ALTER DEFAULT PRIVILEGES IN SCHEMA events GRANT ALL PRIVILEGES ON TABLES TO musenmingle;
ALTER DEFAULT PRIVILEGES IN SCHEMA events GRANT ALL PRIVILEGES ON SEQUENCES TO musenmingle;
ALTER DEFAULT PRIVILEGES IN SCHEMA events GRANT EXECUTE ON FUNCTIONS TO musenmingle;

-- 3b. pgvector: USAGE (only) on the owner's `extensions` schema, where the
--     shared database has the `vector` extension installed, so migrations can
--     use the `extensions.vector` type for event embeddings. musenmingle
--     cannot create anything there. Skipped when the schema does not exist or
--     this owner may not grant on it.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = 'extensions') THEN
        BEGIN
            GRANT USAGE ON SCHEMA extensions TO musenmingle;
        EXCEPTION WHEN insufficient_privilege THEN
            RAISE NOTICE 'cannot grant USAGE on schema extensions (run as its owner): embeddings stay off';
        END;
    END IF;
END
$$;

-- 4. Nothing elsewhere. Explicitly strip anything that may have been granted
--    to the role directly (PUBLIC grants are shared by every role and are
--    managed by the database owner; on PostgreSQL 15+ PUBLIC no longer has
--    CREATE on `public`).
REVOKE ALL PRIVILEGES ON SCHEMA public FROM musenmingle;
REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM musenmingle;
REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public FROM musenmingle;
DO $$
BEGIN
    -- The role may connect to this database but not create schemas in it.
    EXECUTE format('REVOKE CREATE, TEMPORARY ON DATABASE %I FROM musenmingle', current_database());
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO musenmingle', current_database());
END
$$;
