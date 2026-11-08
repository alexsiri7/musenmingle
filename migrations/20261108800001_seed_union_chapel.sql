-- Seed the Union Chapel scraper (src/sources/union_chapel.rs), daily; issue #221.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('union-chapel', 'scraper', 'https://unionchapel.org.uk', 'unionchapel.org.uk', 1440, TRUE,
        'Union Chapel', TRUE, TRUE,
        'Terms & Conditions (https://unionchapel.org.uk/terms-conditions, checked 2026-11-08) '
            || 'cover tickets and website liability only and don''t restrict reproducing listings: '
            || 'descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;
