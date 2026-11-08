-- Seed the Stone Nest scraper (src/sources/stone_nest.rs), daily; issue #220.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('stone-nest', 'scraper', 'https://www.stonenest.org', 'www.stonenest.org', 1440, TRUE,
        'Stone Nest', TRUE, TRUE,
        'No terms of use on the site (checked 2026-11-08: /terms and /terms-and-conditions are 404; '
            || 'the footer links only the privacy policy), so nothing restricts reproducing listings: '
            || 'descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;
