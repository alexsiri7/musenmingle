-- Seed the ICA scraper (src/sources/ica.rs), daily; issue #43.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('ica', 'scraper', 'https://www.ica.art', 'www.ica.art', 1440, TRUE,
        'ICA', TRUE, TRUE,
        'The site''s terms (https://www.ica.art/terms-conditions), checked 2026-09-27, cover membership, tickets, '
            || 'venue hire and limited-edition sales (the image-reproduction clause is about purchased artworks) and '
            || 'don''t restrict listings, so descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;
