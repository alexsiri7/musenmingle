-- Issue #208: AI enrichment sees a listing's full text while the ingest run
-- that scraped it still holds it in memory. The text itself is never
-- stored; only its sha256, so an event is enriched again when its page
-- changes (not on every run).
--
-- events.events.page_text_hash: hash of the page text behind the stored
--   excerpt ('' = the listing was seen and has no text beyond the excerpt;
--   NULL = not scraped since this column was added).
-- enrichments / enrichment_failures.page_text_hash: hash of the page text
--   the attempt saw (NULL = it saw none).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.events ADD COLUMN page_text_hash TEXT;
ALTER TABLE events.enrichments ADD COLUMN page_text_hash TEXT;
ALTER TABLE events.enrichment_failures ADD COLUMN page_text_hash TEXT;
