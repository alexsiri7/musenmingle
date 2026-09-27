-- Venue aliases (issue #204): Goldsmiths CCA is listed under its long name
-- by one source and its short name by another (same address, SE14 6AD).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.venue_aliases (name, venue_name, note) VALUES
    ('Goldsmiths Centre for Contemporary Art', 'Goldsmiths CCA', 'same venue (#204)')
ON CONFLICT (name) DO NOTHING;
