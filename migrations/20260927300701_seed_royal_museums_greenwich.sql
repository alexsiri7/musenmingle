-- Seed the Royal Museums Greenwich source (src/sources/royal_museums_greenwich.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('royal-museums-greenwich', 'scraper', 'https://www.rmg.co.uk', 'www.rmg.co.uk', 1440, TRUE,
        'Royal Museums Greenwich', FALSE, FALSE,
        'Website terms allow fair dealing (private study, non-commercial research, criticism '
            || 'and review) and otherwise forbid copying, reproducing or republishing any content '
            || 'without written permission: facts + link only '
            || '(https://www.rmg.co.uk/policies/terms-conditions, checked 2026-09-27)')
ON CONFLICT (key) DO NOTHING;
