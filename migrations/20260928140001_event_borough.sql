-- London borough of each event (issue #79), from its coordinates:
-- `crate::borough` (point-in-polygon against the ONS boundaries in
-- `src/london_boroughs.geojson`). NULL when the event has no coordinates
-- or is outside Greater London. `repo::upsert_event` sets it from the
-- row's final coordinates and `repo::sync_boroughs` refreshes every row
-- after each ingest run, which also backfills existing rows (a point-in-
-- polygon test can't be done here without PostGIS).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.events
    ADD COLUMN borough TEXT CHECK (borough IN (
        'barking-and-dagenham',
        'barnet',
        'bexley',
        'brent',
        'bromley',
        'camden',
        'city-of-london',
        'croydon',
        'ealing',
        'enfield',
        'greenwich',
        'hackney',
        'hammersmith-and-fulham',
        'haringey',
        'harrow',
        'havering',
        'hillingdon',
        'hounslow',
        'islington',
        'kensington-and-chelsea',
        'kingston-upon-thames',
        'lambeth',
        'lewisham',
        'merton',
        'newham',
        'redbridge',
        'richmond-upon-thames',
        'southwark',
        'sutton',
        'tower-hamlets',
        'waltham-forest',
        'wandsworth',
        'westminster'));

CREATE INDEX events_borough_idx ON events.events (borough);
