-- Seed The Old Church scraper (src/sources/the_old_church.rs), daily; issue #224.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('the-old-church', 'scraper', 'https://www.theoldchurch.org.uk', 'www.theoldchurch.org.uk', 1440, TRUE,
        'The Old Church', TRUE, TRUE,
        'No terms of use on the site (checked 2026-11-09: the footer, the sitemap''s pages and '
            || '/faqs link only a privacy policy), so nothing restricts reproducing listings: '
            || 'descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;
