-- Retire D&AD (issue #101). Its website terms
-- (https://www.dandad.org/policies/terms-and-conditions, checked 2026-09-26)
-- allow copying site material for personal use only and forbid
-- re-circulating it to third parties, which rules out even the facts + link
-- listing we kept. The owner decided to remove it (2026-09-26); its scraper
-- is gone from the code (sources::build answers UnknownKey for this key).
--
-- As with ArtRabbit, the events.sources row is disabled, not deleted, so the
-- run history survives. The events it listed are removed here (owner-approved
-- data-only cleanup, see docs/venue-requests.md): D&AD never stored
-- descriptions or images, so only its events, links and URLs go.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

UPDATE events.sources SET enabled = FALSE WHERE key = 'dandad';

-- Events only D&AD lists go (their links, enrichment and embeddings cascade).
DELETE FROM events.events e
 WHERE EXISTS (SELECT 1 FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
               WHERE es.event_id = e.id AND s.key = 'dandad')
   AND NOT EXISTS (SELECT 1 FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
                   WHERE es.event_id = e.id AND s.key <> 'dandad');

-- Events other sources also list stay, pointing at another source's page
-- instead of D&AD's (a merge never replaces an existing URL by itself), or
-- at that source's site when none of its links recorded a page.
UPDATE events.events e
   SET url = (SELECT COALESCE(es.source_url, s.base_url)
                FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
               WHERE es.event_id = e.id AND s.key <> 'dandad'
               ORDER BY es.source_url IS NULL, es.first_seen_at LIMIT 1)
 WHERE e.url IN (SELECT es.source_url FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
                  WHERE es.event_id = e.id AND s.key = 'dandad');
DELETE FROM events.event_sources
 WHERE source_id = (SELECT id FROM events.sources WHERE key = 'dandad');

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('dandad.org', 'D&AD', 'https://www.dandad.org/events',
        'terms',
        'its terms limit the site''s content to personal use '
        || '(https://www.dandad.org/policies/terms-and-conditions)',
        DATE '2026-09-26', 'https://github.com/alexsiri7/musenmingle/issues/101')
ON CONFLICT (domain) DO NOTHING;
