-- Backfill all_day (issue #99). 20260927200001_events_all_day.sql added the
-- flag without a backfill, relying on each ingest run to re-set it; rows no
-- source re-reports any more (past or delisted events, merges no source
-- claims) kept all_day = false although they were listed with a date only.
--
-- A row is date-only when it starts at exactly 00:00 London time, ends at
-- 00:00 London time or has no end (the shape the scrapers emit for a date
-- without a time), and every source linked to it is one of the venue
-- scrapers that already mapped a missing time to London midnight before the
-- flag existed. Ticketmaster is left out on purpose: a ticketing API can list
-- a real 00:00 start and its stored payload is redacted, so the two can't be
-- told apart; its live rows keep correcting themselves on re-ingest. Sources
-- added after the flag always set it, so they need no backfill.
--
-- Idempotent: rows already flagged are left alone.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

UPDATE events.events e
   SET all_day = TRUE, updated_at = now()
 WHERE NOT e.all_day
   AND (e.starts_at AT TIME ZONE 'Europe/London')::time = TIME '00:00'
   AND (e.ends_at IS NULL
        OR (e.ends_at AT TIME ZONE 'Europe/London')::time = TIME '00:00')
   AND EXISTS (SELECT 1 FROM events.event_sources es WHERE es.event_id = e.id)
   AND NOT EXISTS (
       SELECT 1 FROM events.event_sources es
         JOIN events.sources s ON s.id = es.source_id
        WHERE es.event_id = e.id
          AND s.key NOT IN ('barbican', 'chisenhale-gallery', 'clerkenwell-design-week',
                            'courtauld', 'design-museum', 'garden-museum', 'goldsmiths-cca',
                            'mall-galleries', 'serpentine-galleries', 'soane-museum',
                            'somerset-house', 'whitechapel-gallery',
                            'william-morris-society'));
