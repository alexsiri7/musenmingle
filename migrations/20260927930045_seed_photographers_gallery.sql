-- Seed The Photographers' Gallery scraper (src/sources/photographers_gallery.rs), daily.
-- Facts + link only: its terms of use (https://thephotographersgallery.org.uk/terms-use,
-- 1(a)) allow viewing, printing and downloading "for personal use, but not for any
-- commercial purposes or re-publication".
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('photographers-gallery', 'scraper', 'https://thephotographersgallery.org.uk',
        'thephotographersgallery.org.uk', 1440, TRUE,
        'The Photographers'' Gallery', FALSE, FALSE,
        'Terms of use (https://thephotographersgallery.org.uk/terms-use, checked 2026-09-27) '
            || 'allow the site''s contents for personal use only, not re-publication, so facts '
            || '+ link only')
ON CONFLICT (key) DO NOTHING;
