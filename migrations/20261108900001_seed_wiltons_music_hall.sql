-- Seed the Wilton's Music Hall scraper (src/sources/wiltons_music_hall.rs), daily; issue #222.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('wiltons-music-hall', 'scraper', 'https://wiltons.org.uk', 'wiltons.org.uk', 1440, TRUE,
        'Wilton''s Music Hall', TRUE, TRUE,
        'Booking Terms & Conditions (https://wiltons.org.uk/visit-us/booking-terms-conditions/, '
            || 'checked 2026-11-08) cover ticket sales only and the site has no website terms '
            || 'restricting reproduction: descriptions and images are kept')
ON CONFLICT (key) DO NOTHING;
