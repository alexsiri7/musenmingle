-- Seed the Gasworks scraper (src/sources/gasworks.rs), daily; issue #108.
-- Also the gallery's usual hours during exhibitions.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('gasworks', 'scraper', 'https://www.gasworks.org.uk', 'gasworks.org.uk', 1440, TRUE,
        'Gasworks', TRUE, TRUE,
        'Terms and conditions (https://www.gasworks.org.uk/terms-and-conditions/, checked 2026-09-27) '
            || 'cover only accuracy and cookies and don''t restrict listings, so images are kept '
            || '(no descriptions are read). robots.txt asks for Crawl-delay 20 and Request-rate 1/60 '
            || '(a 60 s built-in floor) and disallows OpenAI''s crawlers with a comment objecting to '
            || '"AI" use of the gallery''s work; we read only the two listings, no detail pages')
ON CONFLICT (key) DO NOTHING;

INSERT INTO events.venue_hours (name, opening_hours, hours_source) VALUES
    -- "Opening Times: (during exhibitions only) Wed–Sun 12–6pm or by
    -- appointment" in the footer of every gasworks.org.uk page (the gasworks
    -- fixtures, 2026-09-27).
    ('Gasworks',
        '[{"days": [3, 4, 5, 6, 7], "opens": "12:00", "closes": "18:00"}]',
        'site footer, gasworks.org.uk 2026-09-27')
ON CONFLICT (name) DO NOTHING;
