-- Seed the Ibraaz scraper (src/sources/ibraaz.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('ibraaz', 'scraper', 'https://ibraaz.org', 'ibraaz.org', 1440, TRUE,
        'Ibraaz', TRUE, FALSE,
        'No terms of use on the site (/terms, /terms-and-conditions, /terms-of-use and '
            || '/copyright are 404s; https://ibraaz.org/privacy-policy doesn''t restrict '
            || 'listings), so descriptions are kept; but images are artists'' works and credited '
            || 'photographs (e.g. "Photo Ollie Hammick", "Image courtesy of Taring Padi"), so no '
            || 'images (checked 2026-09-27)')
ON CONFLICT (key) DO NOTHING;
