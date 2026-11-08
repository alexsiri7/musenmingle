-- Seed the Nonclassical scraper (src/sources/nonclassical.rs), daily; issue #219.
-- The promoter often lists one upcoming event or none, so it may be empty.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note,
                            may_be_empty)
VALUES ('nonclassical', 'scraper', 'https://www.nonclassical.co.uk', 'www.nonclassical.co.uk', 1440, TRUE,
        'Nonclassical', TRUE, TRUE,
        'No terms of use on the site (checked 2026-11-08: /terms, /terms-and-conditions and '
            || '/terms-of-use are 404; the footer links only the privacy and environmental policies), '
            || 'so nothing restricts reproducing listings: descriptions and images are kept',
        TRUE)
ON CONFLICT (key) DO NOTHING;
