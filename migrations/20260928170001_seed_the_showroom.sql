-- Seed The Showroom scraper (src/sources/the_showroom.rs), daily; issue #107.
-- Also the gallery's usual hours during exhibitions.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('the-showroom', 'scraper', 'https://theshowroom.org', 'theshowroom.org', 1440, TRUE,
        'The Showroom', TRUE, TRUE,
        'The site has no terms of use (checked 2026-09-27); its only policy is a privacy policy PDF '
            || '(https://theshowroom.org/media/site/657ec6611e-1712668857/privacy_policy_14-03_22.pdf) '
            || 'that doesn''t restrict listings, and robots.txt carries only Cloudflare''s content-signals '
            || 'preamble with no Content-Signal line or rules, so descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;

INSERT INTO events.venue_hours (name, opening_hours, hours_source) VALUES
    -- "Gallery opening hours (during exhibitions) Wed – Sat, 12–6pm" in the
    -- footer of every theshowroom.org page (the the-showroom fixtures, 2026-09-27).
    ('The Showroom',
        '[{"days": [3, 4, 5, 6], "opens": "12:00", "closes": "18:00"}]',
        'site footer, theshowroom.org 2026-09-27')
ON CONFLICT (name) DO NOTHING;
