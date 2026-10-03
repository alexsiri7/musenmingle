-- Sources whose normal state can be zero upcoming events. The health
-- checker skips its zero-events rule for them; consecutive errors and count
-- drops still trip.
--
-- luma-creative-ai-meetup is a single-series calendar that only lists its
-- next meetup, posted every one to six months, so it is empty between
-- postings (issue #268).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.sources
    ADD COLUMN may_be_empty BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE events.sources SET may_be_empty = TRUE
 WHERE key = 'luma-creative-ai-meetup';
