-- Seed the Clerkenwell Design Week scraper (src/sources/clerkenwell_design_week.rs):
-- one homepage request for the next festival's JSON-LD Event, daily.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('clerkenwell-design-week', 'scraper', 'https://www.clerkenwelldesignweek.com/',
        'www.clerkenwelldesignweek.com', 1440, TRUE,
        'Clerkenwell Design Week', TRUE, TRUE,
        'No website terms of use found (the site links only a privacy policy); short excerpt '
        || 'and a credited thumbnail (https://www.clerkenwelldesignweek.com, checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
