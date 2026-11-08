-- Seed the Vortex Jazz Club scraper (src/sources/vortex_jazz_club.rs), daily; issue #215.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('vortex-jazz-club', 'scraper', 'https://www.vortexjazz.co.uk', 'www.vortexjazz.co.uk', 1440, TRUE,
        'Vortex Jazz Club', FALSE, FALSE,
        'Terms of use (https://www.vortexjazz.co.uk/legal/terms/, checked 2026-11-08) say '
            || '"Reproduction is prohibited other than in accordance with the copyright notice", '
            || 'which restricts the site''s text and images but not event facts: facts + link only')
ON CONFLICT (key) DO NOTHING;
