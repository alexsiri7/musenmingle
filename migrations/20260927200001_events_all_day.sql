-- Date-only events (issue #99). all_day is true when the source gave a date
-- but no time of day: starts_at is then London midnight of the first day and
-- ends_at is NULL (one day) or London midnight of the last day, inclusive.
--
-- No backfill: every ingest run re-sets the flag for the events its sources
-- still list (upsert refresh/merge), so existing rows correct themselves.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.events ADD COLUMN all_day BOOLEAN NOT NULL DEFAULT false;
