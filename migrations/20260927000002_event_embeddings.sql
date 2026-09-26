-- Event embeddings (src/embed.rs): one vector per event, computed after AI
-- enrichment from stored facts + enrichment output, used for "More like
-- this" and (later) hybrid search.
--
-- The vector type comes from pgvector, which the shared database has
-- installed in the `extensions` schema. We never create extensions; we only
-- USE the type, which needs `GRANT USAGE ON SCHEMA extensions TO musenmingle`
-- (ops/sql/create-role.sql). Without that grant (or without pgvector) this
-- migration does nothing and embeddings stay off. The ingest binary re-runs
-- this same file on every start (`db::ensure_optional_schema`), so the table
-- appears on the first run after the grant. It must therefore stay
-- idempotent.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

DO $embeddings$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = 'extensions')
       OR NOT has_schema_privilege('extensions', 'USAGE')
       OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_type t
                        JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace
                       WHERE n.nspname = 'extensions' AND t.typname = 'vector') THEN
        RAISE NOTICE 'extensions.vector is not usable: event embeddings stay off';
        RETURN;
    END IF;

    EXECUTE $ddl$
        CREATE TABLE IF NOT EXISTS events.event_embeddings (
            event_id      UUID        PRIMARY KEY REFERENCES events.events (id) ON DELETE CASCADE,
            model         TEXT        NOT NULL,
            embed_version INTEGER     NOT NULL,
            text_hash     TEXT        NOT NULL,
            -- true when the text had no AI enrichment (re-embedded once enriched)
            facts_only    BOOLEAN     NOT NULL DEFAULT FALSE,
            embedding     extensions.vector(1536) NOT NULL,
            created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
        )
    $ddl$;
    EXECUTE $ddl$
        CREATE INDEX IF NOT EXISTS event_embeddings_hnsw_idx ON events.event_embeddings
            USING hnsw (embedding extensions.vector_cosine_ops)
    $ddl$;
END
$embeddings$;
