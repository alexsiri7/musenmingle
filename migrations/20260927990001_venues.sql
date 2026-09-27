-- Venue coordinates (issue #71, the map). Some listings carry a venue name
-- and address but no latitude/longitude, so they never appear on the map or
-- in area filters. `repo::upsert_event` fills a listing's missing lat/lng
-- from this table when the venue's normalised name
-- (`normalise::normalise_venue_for_key`) matches a row's `name`; coordinates
-- a source gives are never overwritten. Existing events pick the
-- coordinates up the next time a source lists them (upserts fill gaps).
--
-- Seeded by hand for the venues of upcoming events without coordinates on
-- 2026-09-27. Each point was looked up once in OpenStreetMap (Nominatim,
-- one request per venue; data (c) OpenStreetMap contributors, ODbL). There
-- is no automatic geocoding: add rows in a new migration.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

CREATE TABLE events.venues (
    id            BIGSERIAL PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    address       TEXT,
    lat           DOUBLE PRECISION NOT NULL CHECK (lat BETWEEN -90 AND 90),
    lng           DOUBLE PRECISION NOT NULL CHECK (lng BETWEEN -180 AND 180),
    -- Where the point came from, e.g. 'osm-nominatim 2026-09-27'.
    coords_source TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO events.venues (name, address, lat, lng, coords_source) VALUES
    ('Big Belly Comedy - Southbank', '30 Stamford Street, London SE1 9LQ',
        51.5077742, -0.1069201, 'osm-nominatim 2026-09-27'),
    ('Cutty Sark', 'King William Walk, Greenwich, London SE10 9HT',
        51.4829330, -0.0096145, 'osm-nominatim 2026-09-27'),
    ('Freud Museum London', '20 Maresfield Gardens, London NW3 5SX',
        51.5483391, -0.1773926, 'osm-nominatim 2026-09-27'),
    ('Housmans Bookshop', '5 Caledonian Road, London N1 9DX',
        51.5311694, -0.1212707, 'osm-nominatim 2026-09-27'),
    ('Ibraaz', '93 Mortimer Street, London W1W 7SS',
        51.5173522, -0.1418900, 'osm-nominatim 2026-09-27'),
    ('Marshgate Building, UCL East, London', 'Marshgate, UCL East, Stratford, London E20 2AD',
        51.5375420, -0.0119821, 'osm-nominatim 2026-09-27'),
    ('National Maritime Museum', 'Romney Road, Greenwich, London SE10 9NF',
        51.4807759, -0.0051225, 'osm-nominatim 2026-09-27'),
    -- The road, not the building (not mapped in OSM on 2026-09-27).
    ('Prince Philip Maritime Collections Centre', 'Nelson Mandela Road, Kidbrooke, London SE3 9QS',
        51.4636474, 0.0320653, 'osm-nominatim 2026-09-27 (street)'),
    ('Queen''s House', 'Romney Road, Greenwich, London SE10 9NF',
        51.4811874, -0.0038594, 'osm-nominatim 2026-09-27'),
    ('Royal Academy of Arts', 'Burlington House, Piccadilly, London W1J 0BD',
        51.5092768, -0.1397381, 'osm-nominatim 2026-09-27'),
    ('South London Botanical Institute', '323 Norwood Road, London SE24 9AQ',
        51.4421615, -0.1050128, 'osm-nominatim 2026-09-27'),
    ('The Cinema Museum', '2 Dugard Way, London SE11 4TH',
        51.4924085, -0.1051561, 'osm-nominatim 2026-09-27'),
    ('The Horse Hospital', 'Colonnade, Bloomsbury, London WC1N 1JD',
        51.5227858, -0.1243617, 'osm-nominatim 2026-09-27'),
    ('The Rum Factory', '49 Pennington Street, Wapping, London E1W 2BD',
        51.5085729, -0.0625010, 'osm-nominatim 2026-09-27'),
    ('Wapping Hydraulic Power Station', 'Wapping Wall, London E1W 3SS',
        51.5074715, -0.0519074, 'osm-nominatim 2026-09-27')
ON CONFLICT (name) DO NOTHING;
