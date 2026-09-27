-- Venues as first-class objects (issue #204), part 2: every venue gets a
-- location. After each ingest run `geocode::VenueChecks` looks up the
-- postcode (`events.venues.postcode`, parsed from the address) of each
-- venue still without coordinates on postcodes.io (ONS Postcode Directory,
-- Open Government Licence; credited on /about), through FetchContext
-- (robots.txt, User-Agent, rate limit), and stores the postcode's centroid
-- with coords_source 'postcodes.io <postcode> <date>'. Coordinates a
-- listing or a hand-seeded row gave are never replaced.
--
-- geocode_checked_at: the last lookup that found nothing, so an unknown
-- postcode is retried weekly rather than every run.
--
-- A venue with upcoming events and no coordinates raises the
-- 'venues-without-coordinates' alert in events.alert_state (one ntfy a day
-- while it lasts, one when it clears).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.venues
    ADD COLUMN geocode_checked_at TIMESTAMPTZ;
