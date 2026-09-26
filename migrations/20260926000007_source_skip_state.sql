-- Skip state for sources that could not be built (missing credentials, an
-- invalid base_url, or no implementation for the key). Set by
-- `thaleia-ingest` each time a due source is skipped; cleared by the next
-- recorded run. `last_run_at` is left alone so a skipped source stays due.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.sources
    ADD COLUMN skip_reason TEXT NULL,
    ADD COLUMN skipped_at  TIMESTAMPTZ NULL,
    ADD CONSTRAINT sources_skip_both_or_neither
        CHECK ((skip_reason IS NULL) = (skipped_at IS NULL));
