-- Seed the Hunterian Museum scraper (src/sources/hunterian_museum.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('hunterian-museum', 'scraper', 'https://hunterianmuseum.org',
        'hunterianmuseum.org', 1440, TRUE,
        'Hunterian Museum', FALSE, FALSE,
        'No terms of use on the site (/terms, /terms-of-use, /terms-and-conditions, /privacy and '
            || '/privacy-policy are 404s); the owner Royal College of Surgeons of England''s website '
            || 'terms (https://www.rcseng.ac.uk/terms-and-conditions/, written for rcseng.ac.uk) allow '
            || 'extracts for personal use only and forbid using images separately from their text or '
            || 'any content commercially: facts + link only (checked 2026-09-27)')
ON CONFLICT (key) DO NOTHING;
