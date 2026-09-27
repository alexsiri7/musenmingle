-- Seed the first venues on The Events Calendar platform
-- (src/sources/tec.rs, platform 'tec'), daily; issue #115. One row per venue:
-- `config` holds only what differs from the defaults (TecConfig). To add a
-- venue, see "Adding a venue on The Events Calendar" in
-- docs/adding-a-scraper.md.
--
-- Probed 2026-09-27 with the MuseNMingleBot UA:
--   * API with upcoming events: Housmans, Chats Palace, Select Gallery, Freud
--     Museum, the Cinema Museum.
--   * API ruled out by robots.txt, list view JSON-LD used: Bow Arts
--     (`Disallow: /*?`) and the South London Botanical Institute
--     (`Disallow: /wp-json/`).
--   * Hall Place & Gardens is refused below: its terms forbid storing any
--     part of the site in a retrieval system.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note,
                            platform, config)
VALUES
    ('tec-housmans', 'scraper', 'https://housmans.com', 'housmans.com', 1440, TRUE,
     'Housmans Bookshop', TRUE, TRUE,
     'No terms of use on the site (none linked; /terms-and-conditions/ is a 404), so '
         || 'descriptions and images are kept (checked 2026-09-27)',
     'tec',
     '{"list_path": "/events/", "default_category": "talk", "venue": {"name": "Housmans Bookshop", "address": "5 Caledonian Road, London N1 9DX"}}'::jsonb),
    ('tec-chats-palace', 'scraper', 'https://chatspalace.com', 'chatspalace.com', 1440, TRUE,
     'Chats Palace', TRUE, TRUE,
     'No terms of use on the site (only https://chatspalace.com/privacy-policy/, which '
         || 'doesn''t restrict listings; /terms-and-conditions/ is a 404), so descriptions and '
         || 'images are kept (checked 2026-09-27)',
     'tec',
     '{"skip_categories": ["music", "performance", "performance-music", "theatre"]}'::jsonb),
    ('tec-select-gallery', 'scraper', 'https://selectgallery.art', 'selectgallery.art', 1440, TRUE,
     'Select Gallery', FALSE, FALSE,
     'Terms allow personal use only and forbid republishing, reproducing or redistributing '
         || 'material from the site: facts + link only '
         || '(https://selectgallery.art/terms-and-condition/, checked 2026-09-27)',
     'tec',
     '{"category_map": {"events-fairs": "expo"}}'::jsonb),
    ('tec-freud-museum', 'scraper', 'https://www.freud.org.uk', 'www.freud.org.uk', 1440, TRUE,
     'Freud Museum London', FALSE, FALSE,
     'Legal page says all text and images are the museum''s, may be copied for personal or '
         || 'private use and not used commercially without permission: facts + link only '
         || '(https://www.freud.org.uk/legal/, checked 2026-09-27)',
     'tec',
     '{"category_map": {"courses": "workshop"}, "skip_categories": ["tours"]}'::jsonb),
    ('tec-cinema-museum', 'scraper', 'https://cinemamuseum.org.uk', 'cinemamuseum.org.uk', 1440, TRUE,
     'The Cinema Museum', TRUE, FALSE,
     'No terms of use on the site (only https://cinemamuseum.org.uk/privacy-policy/, which '
         || 'doesn''t restrict listings; /terms/ and /terms-and-conditions/ are 404s), so '
         || 'descriptions are kept; but event images are the films'' posters and stills, '
         || 'so no images (checked 2026-09-27)',
     'tec',
     '{"skip_categories": ["tours"], "venue": {"name": "The Cinema Museum", "address": "2 Dugard Way, London SE11 4TH"}}'::jsonb),
    ('tec-bow-arts', 'scraper', 'https://bowarts.org', 'bowarts.org', 1440, TRUE,
     'Bow Arts', TRUE, TRUE,
     'No terms of use on the site (https://bowarts.org/policies/ lists none; /terms/ is a '
         || '404), so descriptions and images are kept (checked 2026-09-27)',
     'tec',
     '{"api_path": null, "list_path": "/bow-arts-events/"}'::jsonb),
    ('tec-slbi', 'scraper', 'https://www.slbi.org.uk', 'www.slbi.org.uk', 1440, TRUE,
     'South London Botanical Institute', FALSE, FALSE,
     'Copyright page allows copying for personal and non-commercial use only and forbids '
         || 'reproducing photos without permission: facts + link only '
         || '(https://www.slbi.org.uk/copyright/, checked 2026-09-27)',
     'tec',
     '{"api_path": null, "list_path": "/events/", "venue": {"name": "South London Botanical Institute", "address": "323 Norwood Road, London SE24 9AQ"}}'::jsonb)
ON CONFLICT (key) DO NOTHING;

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('hallplace.org.uk', 'Hall Place & Gardens', 'https://www.hallplace.org.uk/whats-on/',
        'terms',
        'its website terms allow personal use only and forbid storing any part of the site in '
        || 'another website or in any public or private electronic retrieval system without '
        || 'written permission (https://www.hallplace.org.uk/terms/)',
        DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/115')
ON CONFLICT (domain) DO NOTHING;
