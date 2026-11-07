-- William Morris Society listings changed identity (issue #403): its title
-- links now go to its ticket shop or other pages of its site, and every one
-- of its own /events/<slug>/ pages 404s, so the scraper's source_event_id is
-- now the link without its scheme instead of the old 'events/<slug>' path.
-- Links stored under the old ids would otherwise keep the dead page as the
-- event's url (a merge never replaces an existing URL by itself) and as an
-- extra "Found on" entry, oldest first, ahead of the new link.
--
-- The dead pages go from events.url. Upcoming events lose their old link, so
-- the next run finds them by dedupe key and links them under their new id,
-- whose URL then fills the empty url. Past events are no longer listed, so
-- they keep their link, without its dead page.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

UPDATE events.events e
   SET url = NULL
 WHERE e.url IN (SELECT es.source_url FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
                  WHERE es.event_id = e.id AND s.key = 'william-morris-society'
                    AND es.source_event_id LIKE 'events/%');

DELETE FROM events.event_sources es
 USING events.sources s, events.events e
 WHERE s.id = es.source_id AND s.key = 'william-morris-society'
   AND es.source_event_id LIKE 'events/%'
   AND e.id = es.event_id AND COALESCE(e.ends_at, e.starts_at) >= now();

UPDATE events.event_sources
   SET source_url = NULL
 WHERE source_id = (SELECT id FROM events.sources WHERE key = 'william-morris-society')
   AND source_event_id LIKE 'events/%';
