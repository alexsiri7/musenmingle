-- Seed the Lisson Gallery scraper (src/sources/lisson_gallery.rs), daily;
-- issue #38. London exhibitions only (27 Bell Street and 67 Lisson Street).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('lisson-gallery', 'scraper', 'https://lisson.com', 'lisson.com', 1440, TRUE,
        'Lisson Gallery', TRUE, FALSE,
        'The site''s only terms page (https://lisson.com/legal/terms-and-conditions, checked '
            || '2026-09-27) is a privacy notice and doesn''t restrict listings, so descriptions '
            || 'are kept; no images, as they show artists'' works under copyright')
ON CONFLICT (key) DO NOTHING;
