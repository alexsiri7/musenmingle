-- Seed the D&AD events scraper (src/sources/dandad.rs), daily. London events only.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('dandad', 'scraper', 'https://www.dandad.org', 'www.dandad.org', 1440, TRUE,
        'D&AD', FALSE, FALSE,
        'Website terms allow copying site material for personal use only and forbid re-circulating '
        || 'it to third parties: facts + link only '
        || '(https://www.dandad.org/policies/terms-and-conditions, checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
