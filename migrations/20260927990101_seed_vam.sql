-- Seed the V&A scraper (src/sources/vam.rs), daily.
-- Facts + link only: the websites terms
-- (https://www.vam.ac.uk/info/va-websites-terms-conditions) allow V&A content
-- only for a narrow list of non-commercial uses (section 2), send other digital
-- use to paid licensing (section 4), and clear third-party content (much of the
-- event photography) for the V&A's own use only (section 6).
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('vam', 'scraper', 'https://www.vam.ac.uk', 'www.vam.ac.uk', 1440, TRUE,
        'V&A', FALSE, FALSE,
        'Terms (https://www.vam.ac.uk/info/va-websites-terms-conditions, checked 2026-09-27) '
            || 'permit only narrow non-commercial uses of V&A content, license other digital use, '
            || 'and clear third-party images for the V&A only, so facts + link only')
ON CONFLICT (key) DO NOTHING;
