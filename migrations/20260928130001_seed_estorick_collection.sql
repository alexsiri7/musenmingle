-- Seed the Estorick Collection scraper (src/sources/estorick_collection.rs),
-- daily; issue #41.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('estorick-collection', 'scraper', 'https://www.estorickcollection.com', 'www.estorickcollection.com', 1440, TRUE,
        'Estorick Collection', TRUE, TRUE,
        'The site has no terms of use; its legal page (https://www.estorickcollection.com/addres-and-legal-information) '
            || 'and privacy notice (https://www.estorickcollection.com/privacy-notice-and-cookie-use), checked 2026-09-27, '
            || 'don''t restrict listings, so descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;
