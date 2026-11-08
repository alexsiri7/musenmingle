-- Seed the Sinfonia Smith Square scraper (src/sources/sinfonia_smith_square.rs), daily; issue #217.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('sinfonia-smith-square', 'scraper', 'https://www.sinfoniasmithsq.org.uk', 'www.sinfoniasmithsq.org.uk', 1440, TRUE,
        'Sinfonia Smith Square', TRUE, TRUE,
        'Ts&Cs and Policies (https://www.sinfoniasmithsq.org.uk/visit-us/policies/, checked 2026-11-08) '
            || 'cover ticket sales, admission and privacy only and don''t restrict reproducing listings')
ON CONFLICT (key) DO NOTHING;
