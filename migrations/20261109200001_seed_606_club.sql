-- Seed the 606 Club scraper (src/sources/club_606.rs), daily; issue #225.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('606-club', 'scraper', 'https://www.606club.co.uk', 'www.606club.co.uk', 1440, TRUE,
        '606 Club', FALSE, FALSE,
        'Terms (https://www.606club.co.uk/606-online-terms-and-conditions/, checked 2026-11-09) '
            || 'are for its streaming subscription but say use of "our services and content is '
            || 'for personal and non-business use only" and that the website and content may not '
            || 'be distributed: facts + link only')
ON CONFLICT (key) DO NOTHING;
