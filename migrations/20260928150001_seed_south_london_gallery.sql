-- Seed the South London Gallery scraper (src/sources/south_london_gallery.rs),
-- daily; issue #42.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('south-london-gallery', 'scraper', 'https://www.southlondongallery.org', 'www.southlondongallery.org', 1440, TRUE,
        'South London Gallery', TRUE, TRUE,
        'The site''s terms (https://www.southlondongallery.org/terms-conditions/), checked 2026-09-27, cover shop orders '
            || 'and bookings only (the image-reproduction clause is about purchased editions) and don''t restrict listings, '
            || 'so descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;
