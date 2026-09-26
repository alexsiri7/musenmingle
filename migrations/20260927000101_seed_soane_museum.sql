-- Seed the Sir John Soane's Museum scraper (src/sources/soane_museum.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('soane-museum', 'scraper', 'https://www.soane.org', 'www.soane.org', 1440, TRUE,
        'Sir John Soane''s Museum', FALSE, FALSE,
        'Site terms allow reproduction only for research, private study or internal educational '
            || 'use, and forbid altering images: facts + link only '
            || '(https://www.soane.org/terms-and-conditions, checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
