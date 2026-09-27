-- Seed the Conway Hall scraper (src/sources/conway_hall.rs), daily; issue #32.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('conway-hall', 'scraper', 'https://www.conwayhall.org.uk', 'www.conwayhall.org.uk', 1440, TRUE,
        'Conway Hall', TRUE, TRUE,
        'Terms (https://www.conwayhall.org.uk/terms-conditions/, checked 2026-09-27) cover '
            || 'bookings and visits and don''t restrict listings, so descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;
