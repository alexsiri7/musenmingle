-- Seed the William Morris Gallery scraper (src/sources/william_morris_gallery.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('william-morris-gallery', 'scraper', 'https://www.wmgallery.org.uk',
        'www.wmgallery.org.uk', 1440, TRUE,
        'William Morris Gallery', TRUE, FALSE,
        'No terms of use on the site (/terms-and-conditions/ and /terms-of-use/ are 404s; the '
            || 'privacy policy doesn''t restrict listings) or from its owner Waltham Forest Council '
            || '(https://www.walthamforest.gov.uk/council-and-elections/information-about-website/website-disclaimer '
            || 'restricts only its logos), so descriptions are kept; but publishing images of the '
            || 'collection needs the Gallery''s permission '
            || '(https://www.wmgallery.org.uk/collection/loans-and-images/) and event images are '
            || 'collection objects or third-party works, so no images (checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
