-- Shared-platform sources: one implementation, many venue rows.
--
--   * `platform` names the implementation for rows that share one (e.g.
--     'tec', WordPress sites running The Events Calendar). NULL means a
--     one-off source that `sources::build` finds by its `key`; otherwise
--     `build` dispatches on `platform`, so a new venue on a known platform
--     is a new row, not new code.
--   * `config` is that platform's per-venue settings as JSON (API path,
--     default venue, category map, ...). Its shape belongs to the platform's
--     code, which validates it; a row it rejects is skipped with the reason
--     recorded on the source.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.sources
    ADD COLUMN platform TEXT  NULL,
    ADD COLUMN config   JSONB NULL;
