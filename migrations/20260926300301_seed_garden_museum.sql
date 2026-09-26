-- Seed the Garden Museum scraper (src/sources/garden_museum.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('garden-museum', 'scraper', 'https://www.gardenmuseum.org.uk', 'www.gardenmuseum.org.uk',
        1440, TRUE, 'Garden Museum', TRUE, TRUE,
        'Site terms cover ticket sales and privacy only, plus a general copyright notice; short '
            || 'excerpt and a credited thumbnail (https://www.gardenmuseum.org.uk/tcs/, checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
