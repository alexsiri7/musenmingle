-- Seed the William Morris Society scraper (src/sources/william_morris_society.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('william-morris-society', 'scraper', 'https://williammorrissociety.org',
        'williammorrissociety.org', 1440, TRUE,
        'William Morris Society', TRUE, TRUE,
        'No terms of use on the site; its only policies are privacy notices '
            || '(https://williammorrissociety.org/privacy-cookies/, '
            || 'https://williammorrissociety.org/about-the-society/privacy-policy/) and /licensing/ '
            || 'covers commercial use of Morris designs, none restricting listings (checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
