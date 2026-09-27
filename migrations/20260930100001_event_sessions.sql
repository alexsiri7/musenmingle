-- Multi-session events (issue #207). Some events are a few sessions spread
-- over months (a fortnightly workshop series, a Saturday family club);
-- stored as one all-day range they read as open every day.
--
-- sessions is NULL for a one-off or a continuous run, else a JSON array of
-- at least two {"starts_at": <RFC 3339>, "ends_at": <RFC 3339 or null>}
-- objects in start order (crate::model::Session). starts_at/ends_at stay
-- the envelope (first session's start, last session's end), so every
-- date filter keeps working as a coarse pre-filter; the listing SQL then
-- checks the sessions themselves.
--
-- No backfill: the sources that emit sessions re-set them on their next run.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.events ADD COLUMN sessions JSONB;

ALTER TABLE events.events ADD CONSTRAINT events_sessions_array
    CHECK (sessions IS NULL OR (jsonb_typeof(sessions) = 'array'
                                AND jsonb_array_length(sessions) >= 2));
