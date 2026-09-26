-- Seed the Courtauld scraper (src/sources/courtauld.rs), daily: exhibitions
-- at the Courtauld Gallery (Somerset House) and public talks at the
-- Courtauld's Vernon Square campus.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('courtauld', 'scraper', 'https://courtauld.ac.uk', 'courtauld.ac.uk', 1440, TRUE,
        'The Courtauld', TRUE, TRUE,
        'Website terms only disclaim warranties (information "provided as is for information '
            || 'purposes only") and do not restrict reuse; short excerpt and a credited thumbnail '
            || '(https://courtauld.ac.uk/about-us/policies/data-privacy-and-it/terms-and-conditions/, '
            || 'checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
