-- Seed the Foundling Museum scraper (src/sources/foundling_museum.rs), daily.
-- Facts + link only: its terms (https://foundlingmuseum.org.uk/terms-and-conditions/,
-- section 4) allow the site's material to be used "only as expressly authorised",
-- i.e. extracts for personal non-commercial use, and say its photographic and graphic
-- images "may not be copied, reproduced, licensed or otherwise exploited".
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('foundling-museum', 'scraper', 'https://foundlingmuseum.org.uk',
        'foundlingmuseum.org.uk', 1440, TRUE,
        'Foundling Museum', FALSE, FALSE,
        'Terms (https://foundlingmuseum.org.uk/terms-and-conditions/, checked 2026-09-27) allow '
            || 'extracts for personal non-commercial use only and forbid copying the site''s '
            || 'images, so facts + link only')
ON CONFLICT (key) DO NOTHING;
