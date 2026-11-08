-- Seed the Blackheath Halls scraper (src/sources/blackheath_halls.rs), daily; issue #218.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('blackheath-halls', 'scraper', 'https://www.blackheathhalls.com', 'www.blackheathhalls.com', 1440, TRUE,
        'Blackheath Halls', TRUE, TRUE,
        'Terms & Conditions (https://www.blackheathhalls.com/terms-conditions/, checked 2026-11-08) '
            || 'cover ticket purchases and memberships only and don''t restrict reproducing listings, '
            || 'so descriptions and images are kept. robots.txt asks for Crawl-delay 10')
ON CONFLICT (key) DO NOTHING;
