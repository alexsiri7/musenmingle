-- Manual corrections for cross-source merging (see repo::upsert_event).
-- A row names two source listings (source_id, source_event_id), stored in
-- canonical order (a < b), one row per pair. 'never_merge' keeps them in
-- separate events (even on an exact dedupe_key collision); 'force_merge'
-- puts them in one event even when matching would not. Takes effect the
-- next time either listing is ingested. Partners need not have been seen yet.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

CREATE TABLE events.merge_overrides (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    action            TEXT        NOT NULL CHECK (action IN ('never_merge', 'force_merge')),
    source_id_a       BIGINT      NOT NULL REFERENCES events.sources (id) ON DELETE CASCADE,
    source_event_id_a TEXT        NOT NULL,
    source_id_b       BIGINT      NOT NULL REFERENCES events.sources (id) ON DELETE CASCADE,
    source_event_id_b TEXT        NOT NULL,
    note              TEXT        NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT merge_overrides_canonical_order
        CHECK ((source_id_a, source_event_id_a) < (source_id_b, source_event_id_b)),
    CONSTRAINT merge_overrides_pair_key
        UNIQUE (source_id_a, source_event_id_a, source_id_b, source_event_id_b)
);

CREATE INDEX merge_overrides_b_idx ON events.merge_overrides (source_id_b, source_event_id_b);
