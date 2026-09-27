-- Seed the Four Corners scraper (src/sources/four_corners.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('four-corners', 'scraper', 'https://www.fourcornersfilm.co.uk',
        'www.fourcornersfilm.co.uk', 1440, TRUE,
        'Four Corners', TRUE, FALSE,
        'No terms of use on the site (/terms and /terms-and-conditions are 404s; '
            || 'https://www.fourcornersfilm.co.uk/privacy doesn''t restrict listings), so '
            || 'descriptions are kept; but event images are credited third-party photographs '
            || '(e.g. "Image credit: … by Paul Trevor" on /whats-on/battle-for-the-east-end, '
            || '"Courtesy of the People''s History Museum" on /whats-on/solidarity-on-the-streets), '
            || 'so no images (checked 2026-09-27)')
ON CONFLICT (key) DO NOTHING;
