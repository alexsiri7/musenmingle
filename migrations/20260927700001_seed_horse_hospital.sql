-- Seed The Horse Hospital scraper (src/sources/horse_hospital.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('horse-hospital', 'scraper', 'https://www.thehorsehospital.com',
        'www.thehorsehospital.com', 1440, TRUE,
        'The Horse Hospital', TRUE, FALSE,
        'No terms of use on the site (/terms, /terms-of-use, /terms-and-conditions, '
            || '/privacy-policy and /privacy are 404s; the footer has only a general "All Rights '
            || 'Reserved" notice), so short excerpts are kept; but images are event posters by '
            || 'credited designers (e.g. "Poster: Andrew Ciccone"), so no images (checked 2026-09-27)')
ON CONFLICT (key) DO NOTHING;
