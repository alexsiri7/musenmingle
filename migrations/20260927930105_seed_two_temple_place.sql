-- Seed the Two Temple Place scraper (src/sources/two_temple_place.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('two-temple-place', 'scraper', 'https://twotempleplace.org',
        'twotempleplace.org', 1440, TRUE,
        'Two Temple Place', TRUE, FALSE,
        'No terms of use on the site (/terms, /terms-and-conditions and /terms-conditions are '
            || '404s; the footer and /about-us/our-policies/ link only a privacy policy and '
            || 'safeguarding/volunteer policies), so short excerpts are kept; but images are '
            || 'credited photographs (e.g. "© Agnese Sanvito"), so no images (checked 2026-09-27)')
ON CONFLICT (key) DO NOTHING;
