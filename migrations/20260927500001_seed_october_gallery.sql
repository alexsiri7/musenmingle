-- Seed the October Gallery scraper (src/sources/october_gallery.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('october-gallery', 'scraper', 'https://www.octobergallery.co.uk',
        'www.octobergallery.co.uk', 1440, TRUE,
        'October Gallery', TRUE, FALSE,
        'No terms of use on the site (/terms, /terms-and-conditions, /terms-of-use and '
            || '/copyright are 404s; https://www.octobergallery.co.uk/contact/privacy.php doesn''t '
            || 'restrict listings), so descriptions are kept; but images are artists'' works and '
            || 'credited third-party photographs (e.g. "Photo by Jonathan Greet" on /events/), '
            || 'so no images (checked 2026-09-27)')
ON CONFLICT (key) DO NOTHING;
