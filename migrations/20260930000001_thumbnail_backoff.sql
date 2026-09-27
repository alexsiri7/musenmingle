-- Thumbnail failure backoff (issue #88): events.thumbnails.failures counts
-- the consecutive failed attempts for the row's source_image_url (0 once a
-- thumbnail is made). The thumbnailer retries a failure after
-- `ThumbConfig::retry_failed_after`, doubling for each further failure, and
-- gives up after `ThumbConfig::max_failures` until the image URL changes
-- (`repo::thumbnail_jobs`). Existing failures count as one.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.thumbnails
    ADD COLUMN failures INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0);

UPDATE events.thumbnails SET failures = 1 WHERE bytes IS NULL;
