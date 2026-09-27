-- Venue coordinates (issue #204) for venues whose listings give no full
-- postcode, so `geocode::VenueChecks` can't look them up. Each point was
-- looked up once in OpenStreetMap (Nominatim; data (c) OpenStreetMap
-- contributors, ODbL), as in the #71 migration. The rows may already exist
-- (created by `repo::sync_venues` without coordinates): fill them, never
-- replace a point.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.venues (name, address, lat, lng, coords_source) VALUES
    ('Lisson Gallery', '27 Bell Street, London NW1 5BY',
        51.5208732, -0.1695849, 'osm-nominatim 2026-09-27'),
    -- The street, not the building (not mapped in OSM on 2026-09-27).
    ('Edel Assanti', '11 Bury Street, St James''s, London',
        51.5069312, -0.1385263, 'osm-nominatim 2026-09-27 (street)')
ON CONFLICT (name) DO UPDATE SET
    lat = EXCLUDED.lat, lng = EXCLUDED.lng, coords_source = EXCLUDED.coords_source
    WHERE events.venues.lat IS NULL;
