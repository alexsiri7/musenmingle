-- Larkin Durey (artlogic-larkin-durey) lists its artists' museum shows
-- among its current exhibitions (issue #305): "Anina Major: The Sacred
-- Mangrove at Pérez Art Museum Miami" and "Through Our Eyes: Sámi Art and
-- Culture, Somerset House" were stored at the gallery, 13 Mason's Yard. Its
-- cards carry no location label, only the host in the title, so:
--   * skip_match ["Museum", "Somerset House"]: museum loans are not shows
--     at the gallery and are skipped, as src/sources/artlogic.rs intends.
--     None of its own shows (the past list, checked 2026-11-09) says
--     "museum"; Somerset House has its own source.
-- The two events are removed when Larkin Durey is their only source (their
-- links, enrichment and embeddings cascade). One another source also lists
-- stays: that source's next run refreshes it with its own venue.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

UPDATE events.sources
   SET config = config || '{"skip_match": ["Museum", "Somerset House"]}'::jsonb
 WHERE key = 'artlogic-larkin-durey';

DELETE FROM events.events e
 WHERE EXISTS (SELECT 1 FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
               WHERE es.event_id = e.id AND s.key = 'artlogic-larkin-durey'
                 AND es.source_event_id IN ('www.larkindurey.com:196', 'www.larkindurey.com:198'))
   AND NOT EXISTS (SELECT 1 FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
                   WHERE es.event_id = e.id AND s.key <> 'artlogic-larkin-durey');
