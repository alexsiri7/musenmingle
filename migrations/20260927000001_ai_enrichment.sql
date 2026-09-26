-- AI enrichment (src/enrich/): after ingest, a language model reads ONLY the
-- facts we already store about an event (title, venue, dates, category,
-- source tags, price, stored excerpt, source names) and returns tags from
-- fixed vocabularies plus a short, labelled "What's cool" note. Scrapers
-- still never use a model to extract data.
--
--   * events.model_prices     catalogue prices per model (cost tracking; a
--                             model without a row is never called) and
--                             prompt retention (/about promises the chat
--                             model keeps nothing: retention_days must be 0)
--   * events.enrichments      one current result per event, with the input
--                             hash and prompt version it was made from
--   * events.enrichment_failures  give-ups, so a bad event is not re-sent
--                             every tick (retried when its input changes)
--   * events.enrichment_calls the spend ledger (every call, failed ones too),
--                             read by the daily cap
--   * events.alert_state      "Requesty credits exhausted" state, for the
--                             once-a-day ntfy notification
--   * events.sources.default_medium_tags  deterministic fallback tags
--   * events.events.*         the materialised tags and notes the filters and
--                             pages read (cleared when the input changes)
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

CREATE TABLE events.model_prices (
    model                    TEXT          PRIMARY KEY,
    input_usd_per_mtok       NUMERIC(10, 4) NOT NULL CHECK (input_usd_per_mtok >= 0),
    output_usd_per_mtok      NUMERIC(10, 4) NOT NULL CHECK (output_usd_per_mtok >= 0),
    cache_read_usd_per_mtok  NUMERIC(10, 4) NOT NULL CHECK (cache_read_usd_per_mtok >= 0),
    cache_write_usd_per_mtok NUMERIC(10, 4) NOT NULL CHECK (cache_write_usd_per_mtok >= 0),
    retention_days           INTEGER       NULL,
    checked_on               DATE          NOT NULL,
    note                     TEXT          NULL
);

-- Requesty model catalogue (GET https://router.requesty.ai/v1/models), 2026-09-26.
INSERT INTO events.model_prices
    (model, input_usd_per_mtok, output_usd_per_mtok, cache_read_usd_per_mtok,
     cache_write_usd_per_mtok, retention_days, checked_on, note)
VALUES
    ('anthropic/claude-opus-5-5', 4, 20, 0.2, 5, 0, '2026-09-26', 'Default ENRICH_MODEL; 0-day retention'),
    ('anthropic/claude-sonnet-5', 2, 10, 0.2, 2.5, 30, '2026-09-26', 'Evaluated 2026-09-26; 30-day retention'),
    ('vertex/gemini-3.8-flash', 0.75, 3.75, 0.075, 0.75, 0, '2026-09-26', 'Evaluated 2026-09-26; no cache-write surcharge listed'),
    -- Embeddings (EMBED_MODEL): not in the chat catalogue; OpenAI list price.
    -- Retention unknown (OpenAI API: up to 30 days for abuse monitoring).
    ('openai/text-embedding-3-small', 0.02, 0, 0, 0, NULL, '2026-09-26', 'Default EMBED_MODEL, 1536 dimensions');

CREATE TABLE events.enrichments (
    event_id       UUID          PRIMARY KEY REFERENCES events.events (id) ON DELETE CASCADE,
    model          TEXT          NOT NULL,
    prompt_version INTEGER       NOT NULL,
    input_hash     TEXT          NOT NULL,
    output         JSONB         NOT NULL,
    tokens_in      INTEGER       NOT NULL,
    tokens_out     INTEGER       NOT NULL,
    cost_usd       NUMERIC(12, 6) NOT NULL,
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT now()
);

CREATE TABLE events.enrichment_failures (
    event_id       UUID        PRIMARY KEY REFERENCES events.events (id) ON DELETE CASCADE,
    input_hash     TEXT        NOT NULL,
    prompt_version INTEGER     NOT NULL,
    error          TEXT        NOT NULL,
    failed_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE events.enrichment_calls (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    called_at          TIMESTAMPTZ    NOT NULL DEFAULT now(),
    model              TEXT           NOT NULL,
    prompt_version     INTEGER        NOT NULL,
    events_requested   INTEGER        NOT NULL,
    events_ok          INTEGER        NOT NULL DEFAULT 0,
    tokens_in          INTEGER        NOT NULL DEFAULT 0,
    tokens_cached      INTEGER        NOT NULL DEFAULT 0,
    tokens_cache_write INTEGER        NOT NULL DEFAULT 0,
    tokens_out         INTEGER        NOT NULL DEFAULT 0,
    cost_usd           NUMERIC(12, 6) NOT NULL DEFAULT 0,
    provider_cost_usd  NUMERIC(12, 6) NULL,
    ok                 BOOLEAN        NOT NULL,
    error              TEXT           NULL
);

CREATE INDEX enrichment_calls_called_at_idx ON events.enrichment_calls (called_at);

CREATE TABLE events.alert_state (
    key              TEXT        PRIMARY KEY,
    active_since     TIMESTAMPTZ NULL,
    last_notified_at TIMESTAMPTZ NULL,
    detail           TEXT        NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Deterministic medium tags every event of a source gets (the fallback when
-- AI enrichment is off or has not reached an event yet).
ALTER TABLE events.sources
    ADD COLUMN default_medium_tags TEXT[] NOT NULL DEFAULT '{}';

UPDATE events.sources SET default_medium_tags = '{design}' WHERE key = 'design-museum';

ALTER TABLE events.events
    ADD COLUMN medium_tags    TEXT[]      NOT NULL DEFAULT '{}',
    ADD COLUMN format_tags    TEXT[]      NOT NULL DEFAULT '{}',
    ADD COLUMN good_for       TEXT[]      NOT NULL DEFAULT '{}',
    ADD COLUMN vibe_tags      TEXT[]      NOT NULL DEFAULT '{}',
    ADD COLUMN is_opening     BOOLEAN     NULL,
    ADD COLUMN whats_cool     TEXT        NULL CHECK (char_length(whats_cool) <= 220),
    ADD COLUMN one_liner      TEXT        NULL CHECK (char_length(one_liner) <= 90),
    ADD COLUMN ai_grounding   TEXT        NULL
        CHECK (ai_grounding IN ('listing', 'listing_plus_general_knowledge', 'insufficient')),
    ADD COLUMN ai_model       TEXT        NULL,
    ADD COLUMN ai_enriched_at TIMESTAMPTZ NULL;

CREATE INDEX events_medium_tags_idx ON events.events USING gin (medium_tags);
CREATE INDEX events_format_tags_idx ON events.events USING gin (format_tags);
CREATE INDEX events_good_for_idx ON events.events USING gin (good_for);

-- Existing events start with their sources' default tags.
UPDATE events.events AS e
SET medium_tags = d.tags
FROM (SELECT es.event_id,
             ARRAY(SELECT DISTINCT t FROM unnest(array_agg_tags) AS t ORDER BY t) AS tags
        FROM (SELECT es.event_id, array_agg(t) AS array_agg_tags
                FROM events.event_sources es
                JOIN events.sources s ON s.id = es.source_id
                CROSS JOIN LATERAL unnest(s.default_medium_tags) AS t
               GROUP BY es.event_id) AS es) AS d
WHERE e.id = d.event_id;
