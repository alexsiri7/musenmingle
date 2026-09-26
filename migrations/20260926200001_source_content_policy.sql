-- Per-source content policy, human-readable source names, image provenance
-- and self-hosted thumbnails.
--
-- LetsArt is a free, for-fun aggregator: we don't want to use venues'
-- resources or take their traffic. So:
--   * `display_name` is the source's human-readable name ("Barbican"). It is
--     shown instead of the key on the web pages and used for image credits
--     ("Image: Barbican"). NULL falls back to a title-cased key (#48).
--   * `store_description` / `store_image` say what we may keep from this
--     source, decided from the site's terms (see docs/adding-a-scraper.md).
--     When false, `repo::upsert_event` never persists that field, and the
--     ingest maintenance pass clears what is already stored.
--   * `policy_note` records why (terms URL and date checked).
-- Every stored description is cut to a short excerpt (normalise::excerpt);
-- the ingest maintenance pass trims existing rows with the same code.
--
-- New columns are nullable or defaulted so seed migrations written against
-- the old column list keep working.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.sources
    ADD COLUMN display_name      TEXT    NULL,
    ADD COLUMN store_description BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN store_image       BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN policy_note       TEXT    NULL;

UPDATE events.sources AS s
SET display_name = v.display_name
FROM (VALUES
    ('barbican', 'Barbican'),
    ('design-museum', 'Design Museum'),
    ('serpentine-galleries', 'Serpentine Galleries'),
    ('somerset-house', 'Somerset House'),
    ('whitechapel-gallery', 'Whitechapel Gallery'),
    ('ticketmaster', 'Ticketmaster')
) AS v (key, display_name)
WHERE s.key = v.key AND s.display_name IS NULL;

-- Terms checked 2026-09-26.
UPDATE events.sources SET store_description = FALSE, store_image = FALSE,
    policy_note = 'Ticketmaster API terms of use forbid caching or storing Event Content beyond '
        || 'what the service needs and using Ticketmaster as an image host: facts + link only '
        || '(https://developer.ticketmaster.com/support/terms-of-use/, checked 2026-09-26)'
WHERE key = 'ticketmaster';

UPDATE events.sources SET store_description = FALSE, store_image = FALSE,
    policy_note = 'Site terms allow extracts for personal use only and forbid using photographs '
        || 'separately from their accompanying text: facts + link only '
        || '(https://www.serpentinegalleries.org/legal/, checked 2026-09-26)'
WHERE key = 'serpentine-galleries';

UPDATE events.sources SET
    policy_note = 'Site terms have only a general copyright notice; short excerpt and a credited '
        || 'thumbnail (https://www.barbican.org.uk/terms-conditions, checked 2026-09-26)'
WHERE key = 'barbican';

UPDATE events.sources SET
    policy_note = 'Terms found do not restrict listing reuse; short excerpt and a credited thumbnail '
        || '(https://designmuseum.org/general-terms-and-conditions, checked 2026-09-26)'
WHERE key = 'design-museum';

UPDATE events.sources SET
    policy_note = 'No website terms restricting reuse found (only ticket terms); short excerpt and '
        || 'a credited thumbnail (https://www.somersethouse.org.uk, checked 2026-09-26)'
WHERE key = 'somerset-house';

UPDATE events.sources SET
    policy_note = 'Terms found cover ticket and edition sales only; short excerpt and a credited '
        || 'thumbnail (https://www.whitechapelgallery.org/terms-conditions/, checked 2026-09-26)'
WHERE key = 'whitechapel-gallery';

-- Which source the stored image_url came from (for the credit line).
ALTER TABLE events.events
    ADD COLUMN image_source_id BIGINT NULL REFERENCES events.sources (id) ON DELETE SET NULL;

-- Backfill: the linked source whose raw payload contains the URL, else the
-- earliest-linked source.
UPDATE events.events AS e
SET image_source_id = COALESCE(
    (SELECT es.source_id FROM events.event_sources es
      WHERE es.event_id = e.id AND strpos(es.raw::text, e.image_url) > 0
      ORDER BY es.first_seen_at, es.source_id LIMIT 1),
    (SELECT es.source_id FROM events.event_sources es
      WHERE es.event_id = e.id
      ORDER BY es.first_seen_at, es.source_id LIMIT 1))
WHERE e.image_url IS NOT NULL;

-- Small self-hosted thumbnails made once per source image by the ingest
-- thumbnailer (src/thumbs.rs) and served at /thumbs/{event_id}-{hash}.jpg.
-- A failed attempt keeps a row with `error` set (and no bytes) so the same
-- image URL is not retried on every tick.
CREATE TABLE events.thumbnails (
    event_id         UUID        PRIMARY KEY REFERENCES events.events (id) ON DELETE CASCADE,
    source_id        BIGINT      NULL REFERENCES events.sources (id) ON DELETE SET NULL,
    source_image_url TEXT        NOT NULL,
    bytes            BYTEA       NULL,
    content_type     TEXT        NULL,
    width            INTEGER     NULL,
    height           INTEGER     NULL,
    content_hash     TEXT        NULL,
    etag             TEXT        NULL,
    last_modified    TEXT        NULL,
    error            TEXT        NULL,
    fetched_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT thumbnails_bytes_or_error CHECK (
        (bytes IS NOT NULL AND content_type IS NOT NULL AND width IS NOT NULL
         AND height IS NOT NULL AND content_hash IS NOT NULL AND error IS NULL)
        OR (bytes IS NULL AND error IS NOT NULL))
);
