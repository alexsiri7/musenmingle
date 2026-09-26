-- Seed the Goldsmiths CCA scraper (src/sources/goldsmiths_cca.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('goldsmiths-cca', 'scraper', 'https://goldsmithscca.art', 'goldsmithscca.art', 1440, TRUE,
        'Goldsmiths CCA', TRUE, TRUE,
        'No website terms restricting reuse found (the footer links only a cookie/privacy '
            || 'policy); short excerpt and a credited thumbnail (https://goldsmithscca.art, '
            || 'checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
