-- Seed the Old Royal Naval College scraper (src/sources/old_royal_naval_college.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('old-royal-naval-college', 'scraper', 'https://ornc.org', 'ornc.org', 1440, TRUE,
        'Old Royal Naval College', FALSE, FALSE,
        'Website terms say the site''s images, illustrations and text are for personal, '
            || 'educational and non-commercial use only and may not be reproduced without written '
            || 'permission: facts + link only '
            || '(https://ornc.org/about-us/policies/website-terms-and-conditions/, checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
