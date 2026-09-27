-- Seed the LUX scraper (src/sources/lux.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('lux', 'scraper', 'https://lux.org.uk', 'lux.org.uk', 1440, TRUE,
        'LUX', TRUE, FALSE,
        'No terms of use on the site (/terms/, /terms-and-conditions/ and /terms-of-use/ are 404s; '
            || 'https://lux.org.uk/privacy-policy/ doesn''t restrict listings), so descriptions are '
            || 'kept; but event images are stills from artists'' works ("Courtesy of the artist") '
            || 'and https://lux.org.uk/about/faqs/ says works are "copyright the artist and LUX" '
            || 'and stills are for use "subject to a licensing agreement and fee", so no images '
            || '(checked 2026-09-27)')
ON CONFLICT (key) DO NOTHING;
