-- Annely Juda Fine Art (artlogic-annely-juda) read only
-- /exhibitions/current/, so between shows it found nothing (issue #416):
-- on 2026-11-09 that page was empty while "Leon Kossoff: Trains and
-- Stations" (from 19 Nov 2026) was already announced. Its site serves
-- /exhibitions/current-forthcoming/ like Richard Saltoun's and Waddington
-- Custot's: the grid of both, or, with one show, a redirect to that show.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

UPDATE events.sources
   SET config = config || '{"listing_paths": ["/exhibitions/current-forthcoming/"]}'::jsonb
 WHERE key = 'artlogic-annely-juda';
