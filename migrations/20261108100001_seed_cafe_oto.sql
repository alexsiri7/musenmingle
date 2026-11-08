-- Seed the Cafe OTO scraper (src/sources/cafe_oto.rs), daily; issue #214.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('cafe-oto', 'scraper', 'https://www.cafeoto.co.uk', 'cafeoto.co.uk', 1440, TRUE,
        'Cafe OTO', TRUE, TRUE,
        'Terms and conditions (https://www.cafeoto.co.uk/terms-and-conditions/, checked 2026-11-08) '
            || 'cover accuracy, security, data protection and ticket sales and don''t restrict '
            || 'listings, so descriptions and images are kept. robots.txt asks for Crawl-delay 10')
ON CONFLICT (key) DO NOTHING;
