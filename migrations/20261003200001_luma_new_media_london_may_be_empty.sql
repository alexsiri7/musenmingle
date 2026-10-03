-- luma-new-media-london lists only its next one or two meetups, so once the
-- latest has passed and the next is not posted yet it is empty between
-- postings (issue #272). See 20261003100001_source_may_be_empty.sql.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

UPDATE events.sources SET may_be_empty = TRUE
 WHERE key = 'luma-new-media-london';
