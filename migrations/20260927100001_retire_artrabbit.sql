-- Retire ArtRabbit (issue #96). Its terms of use
-- (https://www.artrabbit.com/about/terms, checked 2026-09-26) prohibit
-- "reproducing, copying, editing, transmitting, uploading or incorporating
-- into any other materials, any of the Website, including without limitation,
-- any information ...": even the facts + link listing we kept is not allowed.
-- The owner decided to remove it (2026-09-26); its scraper is gone from the
-- code (sources::build answers UnknownKey for this key).
--
-- The events.sources row is disabled, not deleted: deleting it would cascade
-- through event_sources/source_runs/merge_overrides inside the migration, so
-- the one-off production cleanup (delete events only ArtRabbit listed, unlink
-- the ones it shared with another source) could no longer tell them apart,
-- and the run history would be lost. The disabled row is hidden on /sources;
-- the site appears there under "Sites we couldn't use" instead.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

UPDATE events.sources SET enabled = FALSE WHERE key = 'artrabbit';

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('artrabbit.com', 'ArtRabbit', 'https://www.artrabbit.com',
        'terms',
        'its terms don''t allow reproducing information from the site, even basic event facts '
        || '(https://www.artrabbit.com/about/terms)',
        DATE '2026-09-26', 'https://github.com/alexsiri7/musenmingle/issues/96')
ON CONFLICT (domain) DO NOTHING;
