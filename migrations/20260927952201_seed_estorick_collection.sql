-- Seed the Estorick Collection scraper (src/sources/estorick_collection.rs),
-- daily; issue #41.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('estorick-collection', 'scraper', 'https://www.estorickcollection.com',
        'www.estorickcollection.com', 1440, TRUE,
        'Estorick Collection', TRUE, FALSE,
        'No terms of use on the site (https://www.estorickcollection.com/addres-and-legal-information '
            || 'has only a DACS copyright note for artists'' works, checked 2026-09-27), so '
            || 'descriptions are kept; no images, as they show artists'' works licensed via DACS')
ON CONFLICT (key) DO NOTHING;
