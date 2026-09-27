-- Venue type (issue #78): museum / commercial_gallery / artist_run /
-- community / other, a filter facet (`venue_type=`).
--
-- events.events.venue_type: set by `repo::sync_venue_types` after every
-- ingest run from `crate::venue_type::classify` (deterministic, no AI:
-- manual override, then the sources' defaults, then a keyword list on the
-- venue name, else 'other'). Existing events are 'other' until the first
-- ingest run after this migration.
--
-- events.venues becomes the one venues table for both coordinates (#71)
-- and venue type: `venue_type` is the manual override for a venue (matched
-- like the coordinates, on `normalise::normalise_venue_for_key` of
-- `name`), NULL = use the rules. A venue can now have a type without
-- coordinates; the coordinates lookup skips such rows. Add rows in new
-- migrations.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.events
    ADD COLUMN venue_type TEXT NOT NULL DEFAULT 'other'
        CHECK (venue_type IN ('museum', 'commercial_gallery', 'artist_run', 'community', 'other'));

CREATE INDEX events_venue_type_idx ON events.events (venue_type);

ALTER TABLE events.venues
    ALTER COLUMN lat DROP NOT NULL,
    ALTER COLUMN lng DROP NOT NULL,
    ALTER COLUMN coords_source DROP NOT NULL,
    ADD COLUMN venue_type TEXT
        CHECK (venue_type IN ('museum', 'commercial_gallery', 'artist_run', 'community', 'other')),
    ADD CONSTRAINT venues_coords_complete
        CHECK ((lat IS NULL) = (lng IS NULL) AND (lat IS NULL OR coords_source IS NOT NULL)),
    ADD CONSTRAINT venues_has_something
        CHECK (lat IS NOT NULL OR venue_type IS NOT NULL);

-- Overrides for well-known venues whose events come from sources without a
-- default (Ticketmaster, Luma, off-site events), or whose name alone would
-- mislead. Rows already there (from #71) keep their coordinates.
INSERT INTO events.venues (name, venue_type) VALUES
    ('Royal Academy of Arts', 'museum'),
    ('Cutty Sark', 'museum'),
    ('Queen''s House', 'museum'),
    ('Prince Philip Maritime Collections Centre', 'museum'),
    ('South London Botanical Institute', 'museum'),
    ('Tate Modern', 'museum'),
    ('Tate Britain', 'museum'),
    ('National Gallery', 'museum'),
    ('National Portrait Gallery', 'museum'),
    ('British Museum', 'museum'),
    ('Hayward Gallery', 'museum'),
    ('Southbank Centre', 'museum'),
    ('Barbican Centre', 'museum'),
    ('ICA', 'museum'),
    ('Institute of Contemporary Arts', 'museum'),
    ('White Cube Bermondsey', 'commercial_gallery'),
    ('White Cube Mason''s Yard', 'commercial_gallery'),
    ('Gagosian', 'commercial_gallery'),
    ('Hauser & Wirth', 'commercial_gallery'),
    ('Housmans Bookshop', 'community'),
    ('Chats Palace Arts Centre', 'community')
ON CONFLICT (name) DO UPDATE SET venue_type = EXCLUDED.venue_type;
