-- Venues as first-class objects (issue #204), part 1: the table and links.
--
-- events.venues (coordinates #71, venue type #78) becomes THE venues table:
-- one row per venue, each event linked to it by events.events.venue_id.
-- Rows are matched, as before, on `normalise::normalise_venue_for_key` of
-- `name` (computed in Rust, so SQL never compares names directly).
--
-- `repo::sync_venues` runs after every ingest run (deterministic, no AI,
-- no network):
--   * creates a venue for every venue name on the events that has none
--     (name, address and coordinates: the most common ones on its events),
--     so the first run after this migration backfills them all;
--   * links each event to its venue (`venue_id`), through
--     events.venue_aliases for names that differ between sources;
--   * fills a venue's missing address/coordinates from its events, its
--     events' missing coordinates from the venue, its borough from its
--     coordinates, and its opening hours from events.venue_hours;
--   * gives each venue a stable, unique `slug` (for /venues/<slug>).
-- The event keeps the venue name and address as the listing gave them.
--
-- events.venue_hours stays as the place scraper PRs seed a venue's usual
-- hours; the sync copies them onto the venue, which the listing reads.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.venues
    DROP CONSTRAINT venues_has_something,
    ADD COLUMN slug          TEXT UNIQUE CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$'),
    ADD COLUMN postcode      TEXT,
    ADD COLUMN borough       TEXT,
    ADD COLUMN website       TEXT CHECK (website IS NULL OR website ~ '^https?://'),
    ADD COLUMN opening_hours JSONB CHECK (opening_hours IS NULL OR jsonb_typeof(opening_hours) = 'array'),
    ADD COLUMN hours_source  TEXT,
    ADD COLUMN updated_at    TIMESTAMPTZ NOT NULL DEFAULT now();

-- Venue names that are spelled differently by different sources but are the
-- same place (`venue_name` = the canonical venue's name), or that are not a
-- venue at all (`venue_name` NULL: an area such as "Clerkenwell", which must
-- never become a venue or get a made-up point). Both sides are matched on
-- the normalised name. Add rows in new migrations.
CREATE TABLE events.venue_aliases (
    name       TEXT PRIMARY KEY,
    venue_name TEXT,
    note       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO events.venue_aliases (name, venue_name, note) VALUES
    ('Institute of Contemporary Arts', 'ICA', 'same venue (#204)'),
    ('Battersea', NULL, 'an area, not a venue (tec-select-gallery, #204)'),
    ('Clerkenwell', NULL, 'an area, not a venue (clerkenwell-design-week, #204)'),
    ('South London', NULL, 'an area, not a venue (garden-museum, #204)')
ON CONFLICT (name) DO NOTHING;

ALTER TABLE events.events
    ADD COLUMN venue_id BIGINT REFERENCES events.venues (id) ON DELETE SET NULL;

CREATE INDEX events_venue_id_idx ON events.events (venue_id) WHERE venue_id IS NOT NULL;
