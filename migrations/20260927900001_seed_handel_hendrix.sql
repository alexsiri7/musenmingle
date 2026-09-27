-- Seed the Handel Hendrix House scraper (src/sources/handel_hendrix.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('handel-hendrix', 'scraper', 'https://handelhendrix.org', 'handelhendrix.org', 1440, TRUE,
        'Handel Hendrix House', FALSE, FALSE,
        'Legal page (https://handelhendrix.org/legal, still naming www.handelhouse.org as the '
            || 'website) says the site''s text, graphics and images are Handel House''s copyright, '
            || 'may be copied only to place an order or give information to Handel House, and '
            || 'forbids other reproduction or re-publication and any reproduction of images '
            || 'without written permission (section 14): facts + link only (checked 2026-09-27)')
ON CONFLICT (key) DO NOTHING;
