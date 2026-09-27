-- Seed the Camden Art Centre scraper (src/sources/camden_art_centre.rs),
-- daily; issue #37. Also the Centre's usual hours for its exhibitions.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('camden-art-centre', 'scraper', 'https://camdenartcentre.org', 'camdenartcentre.org', 1440, TRUE,
        'Camden Art Centre', TRUE, TRUE,
        'The site has no terms of use; its policies page (https://camdenartcentre.org/policies, '
            || 'checked 2026-09-27) covers privacy and visitor photography only and doesn''t '
            || 'restrict listings, so descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;

INSERT INTO events.venue_hours (name, opening_hours, hours_source) VALUES
    -- "Opening times: Wed-Sun, 11am-6pm" in the header and footer of every
    -- camdenartcentre.org page (the camden-art-centre fixtures, 2026-09-27).
    ('Camden Art Centre',
        '[{"days": [3, 4, 5, 6, 7], "opens": "11:00", "closes": "18:00"}]',
        'site header and footer, camdenartcentre.org 2026-09-27')
ON CONFLICT (name) DO NOTHING;
