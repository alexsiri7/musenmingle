-- Music, option (b) (issue #209): the 'arty' end of music (classical,
-- contemporary, experimental, jazz, sound art) as a sixth category, with
-- deterministic subtags.
--
-- * events.events.category may now be 'music'. The CHECK is the inline
--   column constraint of the initial schema, which Postgres named
--   events_category_check.
-- * events.events.music_tags: the event's music subtags
--   (`crate::music::MUSIC_TAGS`), derived from its category, source tags
--   and title by `crate::music::tags_for` and kept up to date by
--   `repo::sync_music_tags` after each ingest run. Empty for every other
--   category.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.events DROP CONSTRAINT events_category_check;
ALTER TABLE events.events ADD CONSTRAINT events_category_check
    CHECK (category IN ('exhibition', 'expo', 'community', 'talk', 'workshop', 'music'));

ALTER TABLE events.events
    ADD COLUMN music_tags TEXT[] NOT NULL DEFAULT '{}';
