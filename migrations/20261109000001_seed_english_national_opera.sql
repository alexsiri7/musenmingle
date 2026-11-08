-- Seed the English National Opera scraper (src/sources/english_national_opera.rs), daily; issue #223.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('english-national-opera', 'scraper', 'https://www.eno.org', 'www.eno.org', 1440, TRUE,
        'English National Opera', FALSE, FALSE,
        'Website terms of use (https://www.eno.org/terms-and-conditions/website-use/, checked '
            || '2026-11-08) allow printing or downloading extracts for personal use only and forbid '
            || 'using images separately from their text or any content commercially: facts + link only')
ON CONFLICT (key) DO NOTHING;
