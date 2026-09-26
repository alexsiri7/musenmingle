-- Seed the Headstone Manor & Museum scraper (src/sources/headstone_manor.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('headstone-manor', 'scraper', 'https://headstonemanor.org',
        'headstonemanor.org', 1440, TRUE,
        'Headstone Manor & Museum', TRUE, TRUE,
        'No terms of use on the site: the footer''s "Terms & Conditions" item has no link, '
            || 'https://headstonemanor.org/terms-and-conditions/ is a 404, the parent Harrow Arts Centre''s '
            || 'https://harrowarts.com/terms-and-conditions has no terms text, and the privacy policy '
            || '(https://headstonemanor.org/privacy-policy) doesn''t restrict listings (checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
